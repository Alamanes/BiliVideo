//! Bilibili API 调用与下载（移植自 BiliMusic，增强 indicatif 进度条与清晰度选择）。
//!
//! 移植自 `bili-rust/src/bili.rs`，保留原注释与核心实现，仅作以下增强：
//!  - 下载进度条改用 indicatif
//!  - 支持 `--quality` 选择清晰度（DASH id 或候选序号）
//!  - 支持 `--audio` 只下音频、`--list` 只列流不下载
//!  - 支持自定义 UA / Cookie / Referer 覆盖

use crate::{download, ffmpeg, util, wbi};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

pub use crate::util::sanitize;

/// 浏览器级 HTTP 客户端，自动携带反爬所需 cookie 与请求头。
pub struct Client {
    agent: ureq::Agent,
    /// 访问首页获得的 `buvid3; b_nut` cookie。
    cookie: String,
    ua: String,
    /// 用户自定义 Referer（None 时按请求自动设置 B站 referer）。
    referer: Option<String>,
}

impl Client {
    /// 访问 B站首页，捕获风控所需的 `buvid3` / `b_nut` cookie。
    ///
    /// `ua` / `cookie` 传 None 时使用默认；显式传入 cookie 则跳过首页风控采集。
    pub fn new(ua: Option<&str>, cookie: Option<&str>) -> Result<Self> {
        let ua = ua.unwrap_or(wbi::UA).to_string();
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(60))
            .build();
        // 用户显式提供 cookie 时直接使用（跳过首页采集）
        if let Some(c) = cookie {
            return Ok(Self {
                agent,
                cookie: c.to_string(),
                ua,
                referer: None,
            });
        }
        let home = agent
            .get("https://www.bilibili.com/")
            .set("User-Agent", &ua)
            .call()
            .context("访问 B站首页失败")?;
        let mut buvid3 = String::new();
        let mut b_nut = String::new();
        for c in home.all("set-cookie") {
            if let Some(i) = c.find("buvid3=") {
                let s = &c[i + 7..];
                buvid3 = s[..s.find(';').unwrap_or(s.len())].to_string();
            }
            if let Some(i) = c.find("b_nut=") {
                let s = &c[i + 6..];
                b_nut = s[..s.find(';').unwrap_or(s.len())].to_string();
            }
        }
        if buvid3.is_empty() {
            bail!("未能从首页获取 buvid3，可能被风控拦截");
        }
        let cookie = format!("buvid3={buvid3}; b_nut={b_nut}");
        Ok(Self {
            agent,
            cookie,
            ua,
            referer: None,
        })
    }

    /// 设置自定义 Referer。
    pub fn with_referer(mut self, r: Option<&str>) -> Self {
        self.referer = r.map(|s| s.to_string());
        self
    }

    /// 构造一个带浏览器头的请求。
    fn get(&self, url: &str, referer: &str) -> ureq::Request {
        let referer = self.referer.as_deref().unwrap_or(referer);
        self.agent
            .get(url)
            .set("User-Agent", &self.ua)
            .set("Accept", "*/*")
            .set("Accept-Language", "zh-CN,zh;q=0.9")
            .set("Origin", "https://www.bilibili.com")
            .set("Sec-Fetch-Site", "same-site")
            .set("Sec-Fetch-Mode", "cors")
            .set("Sec-Fetch-Dest", "empty")
            .set("Referer", referer)
            .set("Cookie", &self.cookie)
    }

    /// 获取 WBI 签名密钥。
    pub fn fetch_wbi_key(&self) -> Result<String> {
        wbi::fetch_key(&self.agent, &self.cookie)
    }
}

/// 视频基础信息。
pub struct Video {
    pub bvid: String,
    pub cid: String,
    pub title: String,
    /// 发布时间戳（保留字段，供后续扩展文件名日期后缀用）
    #[allow(dead_code)]
    pub pubdate: i64,
}

/// 从任意 URL 中提取首个合法 BV（12 位：BV + 10 位字母数字）。
pub fn extract_bvid(url: &str) -> Result<String> {
    scan_all_bvids(url)
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("URL 中未找到合法的 BVID: {url}"))
}

/// 扫描整段文本中所有合法的 BV（保持出现顺序、去重）。
///
/// 兼容：单个 BV、完整网页 URL、嵌在任意中文/英文叙述文本里的链接。
pub fn scan_all_bvids(content: &str) -> Vec<String> {
    let bytes = content.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut i = 0usize;
    while i + 12 <= bytes.len() {
        if bytes[i] == b'B' && bytes[i + 1] == b'V' {
            let tail = &bytes[i + 2..i + 12];
            if tail.iter().all(|c| c.is_ascii_alphanumeric()) {
                let bv = String::from_utf8(bytes[i..i + 12].to_vec()).unwrap();
                if seen.insert(bv.clone()) {
                    out.push(bv);
                }
                i += 12;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// 获取视频详情（标题、cid、发布时间戳）。
pub fn video_info(client: &Client, url: &str) -> Result<Video> {
    let bvid = extract_bvid(url).or_else(|_| {
        // URL 中找不到 BV（如 player 页），抓取页面后整页扫描
        let html = client
            .get(url, "https://www.bilibili.com/")
            .call()
            .context("请求页面失败")?
            .into_string()
            .context("读取页面内容失败")?;
        scan_all_bvids(&html)
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("页面中未找到合法的 BVID: {url}"))
    })?;
    let api = format!("https://api.bilibili.com/x/web-interface/view?bvid={bvid}");
    let json: serde_json::Value = client
        .get(&api, "https://www.bilibili.com/")
        .call()
        .context("请求视频详情失败")?
        .into_json()?;
    if json["code"].as_i64() != Some(0) {
        bail!(
            "API 返回错误: {}",
            json["message"].as_str().unwrap_or("(未知)")
        );
    }
    let d = &json["data"];
    Ok(Video {
        bvid,
        cid: d["cid"].to_string(),
        title: d["title"].as_str().unwrap_or("untitled").to_string(),
        pubdate: d["pubdate"].as_i64().unwrap_or(0),
    })
}

/// 通过 WBI 签名的 playurl 接口拉取 DASH 流。
pub fn play_streams(
    client: &Client,
    wbi_key: &str,
    bvid: &str,
    cid: &str,
) -> Result<serde_json::Value> {
    let mut params = vec![
        ("bvid".to_string(), bvid.to_string()),
        ("cid".to_string(), cid.to_string()),
        ("fnval".to_string(), "4048".to_string()),
        ("fourk".to_string(), "1".to_string()),
    ];
    wbi::sign(&mut params, wbi_key);
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let api = format!("https://api.bilibili.com/x/player/wbi/playurl?{query}");
    let json: serde_json::Value = client
        .get(&api, &format!("https://www.bilibili.com/video/{bvid}"))
        .call()
        .context("请求 playurl 失败")?
        .into_json()?;
    if json["code"].as_i64() != Some(0) {
        bail!(
            "playurl 返回错误: {}",
            json["message"].as_str().unwrap_or("(未知)")
        );
    }
    Ok(json)
}

/// 在 dash.video / dash.audio 中选流：quality 传 DASH id 或 1 起的候选序号，None 取码率最高。
pub fn pick_stream<'a>(
    streams: &'a serde_json::Value,
    kind: &str,
    quality: Option<&str>,
) -> Option<&'a serde_json::Value> {
    let arr = streams["data"]["dash"].get(kind)?.as_array()?;
    if let Some(q) = quality {
        if let Ok(id) = q.parse::<i64>() {
            // 先按 DASH id 精确匹配
            if let Some(s) = arr.iter().find(|x| x["id"].as_i64() == Some(id)) {
                return Some(s);
            }
            // 否则视为 1 起的候选序号
            if id >= 1 && (id as usize) <= arr.len() {
                return Some(&arr[(id - 1) as usize]);
            }
            return None;
        }
    }
    // 缺省：选 id（码率）最高的一条
    arr.iter().max_by_key(|x| x["id"].as_i64().unwrap_or(0))
}

/// 兼容 BiliMusic 命名：选码率最高的一条。
pub fn best_stream<'a>(streams: &'a serde_json::Value, kind: &str) -> Option<&'a serde_json::Value> {
    pick_stream(streams, kind, None)
}

/// 取流的实际下载地址（baseUrl 为空时退回第一个 backupUrl）。
fn stream_url(s: &serde_json::Value) -> String {
    let base = s["baseUrl"].as_str().unwrap_or("");
    if !base.is_empty() {
        return base.to_string();
    }
    s["backupUrl"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// 生成选流失败的错误信息（附带可用 id 列表）。
fn no_stream_err(
    streams: &serde_json::Value,
    kind: &str,
    quality: Option<&str>,
) -> anyhow::Error {
    let ids: Vec<String> = streams["data"]["dash"][kind]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x["id"].as_i64().unwrap_or(0).to_string())
                .collect()
        })
        .unwrap_or_default();
    match quality {
        Some(q) => anyhow::anyhow!(
            "未找到匹配的 {kind} 流 (quality={q})，可用 id: {}",
            ids.join(", ")
        ),
        None => anyhow::anyhow!("无可用的 {kind} 流"),
    }
}

/// 列出 DASH 视频/音频流（带序号与 id），供 `--list` 使用。
fn list_streams(streams: &serde_json::Value) {
    let dash = &streams["data"]["dash"];
    if let Some(v) = dash["video"].as_array() {
        println!("视频流:");
        for (i, s) in v.iter().enumerate() {
            println!(
                "  {:>2}. id={:<6} {}x{}  codecs={}  {} kb/s",
                i + 1,
                s["id"].as_i64().unwrap_or(0),
                s["width"].as_i64().unwrap_or(0),
                s["height"].as_i64().unwrap_or(0),
                s["codecs"].as_str().unwrap_or("?"),
                s["bandwidth"].as_i64().unwrap_or(0) / 1000,
            );
        }
    }
    if let Some(a) = dash["audio"].as_array() {
        println!("音频流:");
        for (i, s) in a.iter().enumerate() {
            println!(
                "  {:>2}. id={:<6} codecs={}  {} kb/s",
                i + 1,
                s["id"].as_i64().unwrap_or(0),
                s["codecs"].as_str().unwrap_or("?"),
                s["bandwidth"].as_i64().unwrap_or(0) / 1000,
            );
        }
    }
}

/// B站下载选项。
pub struct Options<'a> {
    pub output: &'a Path,
    pub audio_only: bool,
    pub quality: Option<&'a str>,
    pub list_only: bool,
}

impl Client {
    /// 处理一条 B站链接：取详情 → 列流或下载。
    pub fn process(&self, wbi_key: &str, url: &str, opt: &Options) -> Result<()> {
        // b23.tv 短链先展开为完整链接
        let url = util::expand_short_url(url)?;
        let video = video_info(self, &url).context("获取视频详情失败")?;
        println!("标题: {}", video.title);
        println!("BVID: {}  CID: {}", video.bvid, video.cid);

        let streams = play_streams(self, wbi_key, &video.bvid, &video.cid)?;
        if opt.list_only {
            list_streams(&streams);
            return Ok(());
        }

        let out_dir = opt.output;
        fs::create_dir_all(out_dir)
            .with_context(|| format!("创建输出目录失败: {}", out_dir.display()))?;
        let referer = format!("https://www.bilibili.com/video/{}", video.bvid);

        if opt.audio_only {
            // 只下载音频流（DASH audio）
            let a = pick_stream(&streams, "audio", opt.quality)
                .ok_or_else(|| no_stream_err(&streams, "audio", opt.quality))?;
            let name = format!("{}.m4a", sanitize(&video.title));
            let path = out_dir.join(name);
            println!(
                "下载音频流 id={} ...",
                a["id"].as_i64().unwrap_or(0)
            );
            download::download(self.get(&stream_url(a), &referer), &path)?;
            println!("完成: {}", path.display());
            return Ok(());
        }

        // 视频模式：视频流（可 -q 指定清晰度）+ 最佳音频流，ffmpeg 合并
        let v = pick_stream(&streams, "video", opt.quality)
            .ok_or_else(|| no_stream_err(&streams, "video", opt.quality))?;
        let a = best_stream(&streams, "audio")
            .ok_or_else(|| no_stream_err(&streams, "audio", None))?;
        let tmp_v = out_dir.join(format!(".bvideo_{}_v.m4s", video.bvid));
        let tmp_a = out_dir.join(format!(".bvideo_{}_a.m4a", video.bvid));
        println!("下载视频流 id={} ...", v["id"].as_i64().unwrap_or(0));
        download::download(self.get(&stream_url(v), &referer), &tmp_v)?;
        println!("下载音频流 id={} ...", a["id"].as_i64().unwrap_or(0));
        download::download(self.get(&stream_url(a), &referer), &tmp_a)?;
        if !ffmpeg::which_ffmpeg() {
            bail!(
                "合并需要 ffmpeg，请先安装并加入 PATH（视频/音频分片已保留在 {} 与 {}）",
                tmp_v.display(),
                tmp_a.display()
            );
        }
        let base = sanitize(&video.title);
        let out = out_dir.join(format!("{base}.mp4"));
        println!("ffmpeg 合并 → {}", out.display());
        ffmpeg::run(&[], &[&tmp_v, &tmp_a], &["-c", "copy"], &out)?;
        let _ = fs::remove_file(&tmp_v);
        let _ = fs::remove_file(&tmp_a);
        println!("完成: {}", out.display());
        Ok(())
    }
}
