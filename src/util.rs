//! 通用工具：文件名清洗、URL 规范化与解析。

use anyhow::{Context, Result};

/// 清洗为安全的文件名（复用 BiliMusic sanitize 思路）。
pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// 取 URL 路径部分的文件名（去掉查询串与锚点）。
pub fn url_filename(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let base = path.rsplit('/').next().unwrap_or(path);
    if base.is_empty() {
        "download".to_string()
    } else {
        base.to_string()
    }
}

/// 去掉最后一个扩展名。
pub fn stem(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].to_string(),
        _ => name.to_string(),
    }
}

/// 媒体类型（用于 URL 路由分发）。
pub enum MediaKind {
    /// m3u8 播放列表 → HLS 管线
    Hls,
    /// 视频/音频直链
    File,
    /// 其他（可能是网页）→ 嗅探
    Unknown,
}

/// 按扩展名判断 URL 属于哪类媒体。
pub fn media_kind(url: &str) -> MediaKind {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "m3u8" => MediaKind::Hls,
        "mp4" | "webm" | "ts" | "flv" | "mov" | "mkv" | "m4a" | "mp3" | "aac" | "m4s"
        | "mpd" => MediaKind::File,
        _ => MediaKind::Unknown,
    }
}

/// 展开 b23.tv 短链（跟随重定向直到非 3xx），非短链原样返回。
pub fn expand_short_url(url: &str) -> Result<String> {
    if !url.contains("b23.tv") {
        return Ok(url.to_string());
    }
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(std::time::Duration::from_secs(15))
        .build();
    let mut cur = url.to_string();
    for _ in 0..5 {
        let resp = agent
            .get(&cur)
            .set("User-Agent", crate::wbi::UA)
            .call()
            .with_context(|| format!("展开短链失败: {cur}"))?;
        match resp.status() {
            301 | 302 | 303 | 307 | 308 => {
                let loc = resp
                    .header("Location")
                    .context("重定向响应缺少 Location")?;
                cur = resolve_url(&cur, loc);
            }
            _ => break,
        }
    }
    Ok(cur)
}

/// 以 base 为基准解析 url（绝对 / 协议相对 / 根相对 / 目录相对）。
pub fn resolve_url(base: &str, url: &str) -> String {
    let url = url.trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    if let Some(rest) = url.strip_prefix("//") {
        // 协议相对：继承 base 的 scheme
        let scheme = base.split(':').next().unwrap_or("https");
        return format!("{scheme}:{rest}");
    }
    if let Some(origin) = origin_of(base) {
        if url.starts_with('/') {
            // 根相对
            return format!("{origin}{url}");
        }
        // 目录相对：取 base 去掉查询/锚点后的目录部分
        return format!("{}{url}", base_dir(base));
    }
    url.to_string()
}

/// base 的 origin（scheme://host，不含路径）。
fn origin_of(base: &str) -> Option<String> {
    let i = base.find("://")?;
    let scheme = &base[..i + 3];
    let rest = &base[i + 3..];
    let host: String = rest
        .chars()
        .take_while(|c| *c != '/' && *c != '?' && *c != '#')
        .collect();
    (!host.is_empty()).then(|| format!("{scheme}{host}"))
}

/// base 去掉查询/锚点后的目录部分（以 / 结尾）。
fn base_dir(base: &str) -> String {
    let base = base.split(['?', '#']).next().unwrap_or(base);
    match base.rfind('/') {
        Some(i) => base[..=i].to_string(),
        None => format!("{base}/"),
    }
}
