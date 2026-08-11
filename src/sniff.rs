//! 通用网页视频流嗅探：按优先级提取页面中正在播放/引用的视频流 URL。
//!
//! 优先级：
//!   ① `<video>` / `<audio>` / `<source>` 标签的 src / data-src / data-url
//!   ② `<meta property="og:video">` / og:video:secure_url / og:audio
//!   ③ 内嵌 JSON（__INITIAL_STATE__ / __NUXT__ / __NEXT_DATA__ / player.__vars）递归扫描
//!   ④ 全文本正则嗅探（mp4|m3u8|webm|ts|flv）
//! 返回去重候选列表，每个带来源标注。

use crate::{download, util};
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;

/// 嗅探候选流。
pub struct Candidate {
    /// 绝对 URL（已按页面地址解析相对路径）
    pub url: String,
    /// 来源标注（video 标签 / og:video / 内嵌 JSON / 正则嗅探）
    pub source: String,
    /// 是否为音频流（audio 标签 / og:audio）
    pub audio: bool,
}

/// 视频 URL 模式（全文本与 JSON 递归共用）。
fn video_url_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"https?://[^\s"'<>]+\.(mp4|m3u8|webm|ts|flv|mov|mkv)(\?[^\s"'<>]*)?"#)
            .unwrap()
    })
}

/// 抓取页面 HTML（带 UA / Cookie / Referer）。
pub fn fetch_page(url: &str, http: &download::HttpOpts) -> Result<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build();
    let resp = download::request(&agent, url, http)
        .call()
        .with_context(|| format!("抓取页面失败: {url}"))?;
    if resp.status() != 200 {
        anyhow::bail!("页面响应非 200: {} ({})", resp.status(), url);
    }
    resp.into_string().context("读取页面内容失败")
}

/// 提取页面标题（<title> 优先，其次 og:title）。
pub fn page_title(html: &str) -> Option<String> {
    let re = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").ok()?;
    if let Some(m) = re.captures(html) {
        let t = m.get(1)?.as_str().trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    meta_pairs(html)
        .into_iter()
        .find(|(p, _)| p == "og:title")
        .map(|(_, c)| c)
}

/// 扫描所有 `<meta>` 的 property / content 键值对。
fn meta_pairs(html: &str) -> Vec<(String, String)> {
    let tag = Regex::new(r"(?i)<meta\b[^>]*>").unwrap();
    let prop = Regex::new(r#"(?i)\bproperty\s*=\s*["']([^"']+)["']"#).unwrap();
    let cont = Regex::new(r#"(?i)\bcontent\s*=\s*["']([^"']+)["']"#).unwrap();
    tag.find_iter(html)
        .filter_map(|m| {
            let t = m.as_str();
            let p = prop.captures(t)?.get(1)?.as_str().to_ascii_lowercase();
            let c = cont.captures(t)?.get(1)?.as_str().to_string();
            Some((p, c))
        })
        .collect()
}

/// 按优先级嗅探视频流，返回去重候选列表。
pub fn sniff(html: &str, base: &str) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // ① video/audio/source 标签
    for (url, audio) in tag_streams(html) {
        push(
            &mut out,
            &mut seen,
            base,
            &url,
            if audio { "audio 标签" } else { "video 标签" },
            audio,
        );
    }
    // ② og:video / og:video:secure_url / og:audio（secure_url 优先）
    let pairs = meta_pairs(html);
    for (p, c) in &pairs {
        if p == "og:video:secure_url" || p == "og:video" {
            push(&mut out, &mut seen, base, c, "og:video", false);
        }
    }
    for (p, c) in &pairs {
        if p == "og:audio" {
            push(&mut out, &mut seen, base, c, "og:audio", true);
        }
    }
    // ③ 内嵌 JSON 递归扫描
    for (url, audio) in json_streams(html) {
        push(&mut out, &mut seen, base, &url, "内嵌 JSON", audio);
    }
    // ④ 全文本正则嗅探（限制条数，避免大量无关命中刷屏）
    for m in video_url_re().find_iter(html).take(50) {
        push(&mut out, &mut seen, base, m.as_str(), "正则嗅探", false);
    }
    out
}

/// 去重并解析相对路径后入列。
fn push(
    out: &mut Vec<Candidate>,
    seen: &mut HashSet<String>,
    base: &str,
    url: &str,
    source: &str,
    audio: bool,
) {
    let abs = util::resolve_url(base, url);
    // 过滤明显非视频的 URL（海报图、占位图等）
    if !plausible(&abs) {
        return;
    }
    if seen.insert(abs.clone()) {
        out.push(Candidate {
            url: abs,
            source: source.to_string(),
            audio,
        });
    }
}

/// 粗略过滤：必须是 http(s)，且不以常见图片/静态资源扩展名结尾。
fn plausible(url: &str) -> bool {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or(&lower);
    let ext = path.rsplit('.').next().unwrap_or("");
    !["jpg", "jpeg", "png", "gif", "webp", "svg", "ico", "css", "js"].contains(&ext)
}

/// 从 `<video>` / `<audio>` / `<source>` 标签提取流 URL，返回 (url, is_audio)。
fn tag_streams(html: &str) -> Vec<(String, bool)> {
    let open = Regex::new(r"(?i)<(video|audio)\b[^>]*>").unwrap();
    let source = Regex::new(r"(?i)<source\b[^>]*>").unwrap();
    let attr = Regex::new(r#"(?i)\b(src|data-src|data-url)\s*=\s*["']([^"']+)["']"#).unwrap();
    // 记录各 video/audio 开标签位置，用于判断 <source> 归属
    let containers: Vec<(usize, bool)> = open
        .find_iter(html)
        .map(|m| (m.start(), m.as_str().to_ascii_lowercase().contains("<audio")))
        .collect();
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut emit = |tag: &str, is_audio: bool| {
        for cap in attr.captures_iter(tag) {
            if let Some(u) = cap.get(2) {
                out.push((u.as_str().to_string(), is_audio));
            }
        }
    };
    for m in open.find_iter(html) {
        emit(m.as_str(), m.as_str().to_ascii_lowercase().contains("<audio"));
    }
    for m in source.find_iter(html) {
        // 找最近的前置容器标签判断音频/视频
        let is_audio = containers
            .iter()
            .rev()
            .find(|(pos, _)| *pos < m.start())
            .map(|(_, a)| *a)
            .unwrap_or(false);
        emit(m.as_str(), is_audio);
    }
    out
}

/// 从内嵌 JSON（__INITIAL_STATE__ / __NUXT__ / __NEXT_DATA__ / player.__vars）递归提取视频 URL。
fn json_streams(html: &str) -> Vec<(String, bool)> {
    let script = Regex::new(r"(?is)<script\b[^>]*>(.*?)</script>").unwrap();
    let mut out: Vec<(String, bool)> = Vec::new();
    for m in script.captures_iter(html) {
        let body = m.get(1).map(|x| x.as_str()).unwrap_or("");
        if !(body.contains("__INITIAL_STATE__")
            || body.contains("__NUXT__")
            || body.contains("__NEXT_DATA__")
            || body.contains("player.__vars"))
        {
            continue;
        }
        // 取出标记后的 JSON 对象；解析失败则跳过（全文本正则兜底）
        if let Some(json_text) = extract_json(body) {
            if let Ok(v) = serde_json::from_str::<Value>(&json_text) {
                let mut urls: Vec<String> = Vec::new();
                walk_json(&v, &mut urls);
                for u in urls {
                    out.push((u, false));
                }
            }
        }
    }
    out
}

/// 平衡花括号扫描：从第一个 `{` 起提取完整 JSON 对象文本。
fn extract_json(s: &str) -> Option<String> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// 递归遍历 JSON，收集所有匹配视频 URL 模式的字符串。
fn walk_json(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            if video_url_re().is_match(s) {
                out.push(s.clone());
            }
        }
        Value::Array(a) => {
            for x in a {
                walk_json(x, out);
            }
        }
        Value::Object(m) => {
            for (_, x) in m {
                walk_json(x, out);
            }
        }
        _ => {}
    }
}
