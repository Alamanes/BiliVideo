//! bilivideo —— 网站视频嗅探+下载工具。
//!
//! 用法: `bvideo [OPTIONS] <URL_OR_TXT...>`
//!   输入 B站链接 / 视频直链 / 任意网页，自动识别类型并下载；
//!   也可传 txt 文件，自动提取其中所有 URL 批量处理。

mod bili;
mod cli;
mod download;
mod ffmpeg;
mod hls;
mod sniff;
mod util;
mod wbi;

use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use std::fs;
use std::path::Path;

use regex::Regex;
use std::sync::OnceLock;

/// 提取文本中所有 URL（支持整页复制、链接藏在任意文本中）。
fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"'，。；、]+"#).unwrap())
}

/// 判断参数是 URL 还是文件路径。
fn is_url_like(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("//")
        || t.contains("://")
        || t.contains("bilibili.com")
        || t.contains("b23.tv")
}

/// 展开参数：URL 直接使用；txt 文件读取并提取其中所有 URL（整页复制也可）。
fn collect_urls(args: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for a in args {
        let path = Path::new(a);
        if path.is_file() {
            let content = fs::read_to_string(path)
                .with_context(|| format!("读取文件失败: {}", path.display()))?;
            let urls: Vec<String> = url_re()
                .captures_iter(&content)
                .map(|c| c[0].to_string())
                .collect();
            if urls.is_empty() {
                println!("未在 `{}` 中提取到任何 URL。", path.display());
            } else {
                println!("`{}` 中提取到 {} 条 URL。", path.display(), urls.len());
            }
            out.extend(urls);
        } else if is_url_like(a) {
            out.push(a.clone());
        } else {
            anyhow::bail!("参数既不是存在的文件也不是合法 URL: {a}");
        }
    }
    Ok(out)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // 输出目录统一提前创建
    let out_dir = cli
        .output
        .clone()
        .unwrap_or_else(|| "download".into());
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("创建输出目录失败: {}", out_dir.display()))?;

    let urls = collect_urls(&cli.urls)?;
    if urls.is_empty() {
        return Ok(());
    }

    for url in &urls {
        println!("{}", "-".repeat(60));
        if let Err(e) = route(&cli, &out_dir, url) {
            eprintln!("处理失败: {e:#}");
        }
    }
    println!("{}", "-".repeat(60));
    println!("全部任务处理完毕。");
    Ok(())
}

/// URL 路由分发：按域名 / 扩展名判断走哪条路径。
fn route(cli: &Cli, out_dir: &Path, url: &str) -> Result<()> {
    let lower = url.to_ascii_lowercase();
    if lower.contains("bilibili.com") || lower.contains("b23.tv") {
        return bili_route(cli, out_dir, url);
    }
    match util::media_kind(url) {
        util::MediaKind::Hls => {
            // m3u8 直链 → HLS 管线
            let name = util::stem(&util::url_filename(url));
            if cli.list {
                println!("直链候选: [m3u8] {}", url);
                return Ok(());
            }
            let opt = hls::Options {
                output: out_dir,
                name: &name,
                concurrency: cli.concurrency,
                merge: !cli.no_merge,
                format: &cli.format,
                http: http_opts(cli, None),
            };
            hls::download(url, &opt).map(|_| ())
        }
        util::MediaKind::File => {
            // 视频直链 → 直接下载
            if cli.list {
                println!("直链候选: [直链] {}", url);
                return Ok(());
            }
            let name = util::url_filename(url);
            let path = out_dir.join(util::sanitize(&name));
            download::download_url(url, &path, &http_opts(cli, None))
        }
        util::MediaKind::Unknown => sniff_route(cli, out_dir, url),
    }
}

/// B站专精路径：初始化风控客户端 → WBI 密钥 → 取详情/列流/下载。
fn bili_route(cli: &Cli, out_dir: &Path, url: &str) -> Result<()> {
    println!("B站链接 → 走 Bilibili API 路径");
    let client = bili::Client::new(cli.ua.as_deref(), cli.cookie.as_deref())?
        .with_referer(cli.referer.as_deref());
    println!("获取 WBI 签名密钥...");
    let wbi_key = client.fetch_wbi_key().context("获取 WBI 密钥失败")?;
    let opt = bili::Options {
        output: out_dir,
        audio_only: cli.audio,
        quality: cli.quality.as_deref(),
        list_only: cli.list,
    };
    client.process(&wbi_key, url, &opt)
}

/// 通用嗅探路径：抓页面 → 嗅探候选 → 列出或下载。
fn sniff_route(cli: &Cli, out_dir: &Path, url: &str) -> Result<()> {
    // 下载嗅探到的流时默认以页面为 Referer（防盗链）
    let http = http_opts(cli, Some(url));
    let html = sniff::fetch_page(url, &http)?;
    let title = sniff::page_title(&html).unwrap_or_else(|| url.to_string());
    let cands = sniff::sniff(&html, url);
    if cands.is_empty() {
        anyhow::bail!("未在页面中嗅探到任何视频流: {url}");
    }
    if cli.list {
        println!("页面: {title}");
        println!("嗅探到 {} 个候选流:", cands.len());
        for (i, c) in cands.iter().enumerate() {
            println!(
                "  {:>2}. [{}] {}{}",
                i + 1,
                c.source,
                if c.audio { "[音频] " } else { "" },
                c.url
            );
        }
        return Ok(());
    }
    // 选候选：-q 按序号；--audio 优先音频流；否则取第一个（优先级最高）
    let pick: Option<&sniff::Candidate> = if let Some(q) = &cli.quality {
        match q.parse::<usize>() {
            Ok(n) if n >= 1 && n <= cands.len() => Some(&cands[n - 1]),
            _ => anyhow::bail!("候选序号越界: -q {q}（共 {} 个候选）", cands.len()),
        }
    } else if cli.audio {
        cands.iter().find(|c| c.audio).or_else(|| cands.first())
    } else {
        cands.first()
    };
    let cand = pick.context("没有可选中的候选流")?;
    println!("页面: {title}");
    println!("选中候选: [{}] {}", cand.source, cand.url);
    match util::media_kind(&cand.url) {
        util::MediaKind::Hls => {
            // HLS 合并时输出 `name.mp4/mkv`，这里取去掉扩展名的基名
            let name = util::stem(&util::url_filename(&cand.url));
            let opt = hls::Options {
                output: out_dir,
                name: &name,
                concurrency: cli.concurrency,
                merge: !cli.no_merge,
                format: &cli.format,
                http,
            };
            hls::download(&cand.url, &opt).map(|_| ())
        }
        _ => {
            // 直链：保留原始文件名（含扩展名）
            let name = util::url_filename(&cand.url);
            let path = out_dir.join(util::sanitize(&name));
            download::download_url(&cand.url, &path, &http)
        }
    }
}

/// 组装 HTTP 选项：自定义 UA 缺省用 wbi::UA；referer 缺省用 `default_ref`。
fn http_opts<'a>(cli: &'a Cli, default_ref: Option<&'a str>) -> download::HttpOpts<'a> {
    download::HttpOpts {
        ua: cli.ua.as_deref().unwrap_or(download::DEFAULT_UA),
        cookie: cli.cookie.as_deref(),
        referer: cli.referer.as_deref().or(default_ref),
    }
}
