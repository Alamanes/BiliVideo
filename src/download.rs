//! 通用直链下载（indicatif 进度条）。
//!
//! 有 Content-Length 时显示百分比条（已下载/总大小/速度/ETA），
//! 未知大小时显示 indeterminate 风格的 spinner。

use crate::wbi;
use anyhow::{bail, Context, Result};
use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

/// 默认 UA（与 B站路径保持一致）。
pub const DEFAULT_UA: &str = wbi::UA;

/// HTTP 请求选项（UA / Cookie / Referer，防盗链）。
#[derive(Clone, Copy)]
pub struct HttpOpts<'a> {
    pub ua: &'a str,
    pub cookie: Option<&'a str>,
    pub referer: Option<&'a str>,
}

/// 按选项构造带请求头的 GET 请求。
pub fn request(agent: &ureq::Agent, url: &str, http: &HttpOpts) -> ureq::Request {
    let mut r = agent.get(url).set("User-Agent", http.ua);
    if let Some(c) = http.cookie {
        r = r.set("Cookie", c);
    }
    if let Some(rf) = http.referer {
        r = r.set("Referer", rf);
    }
    r
}

/// 直接下载 URL 到 `path`。
pub fn download_url(url: &str, path: &Path, http: &HttpOpts) -> Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(120))
        .build();
    download(request(&agent, url, http), path)
}

/// 下载一个已构造好的请求，显示进度条，写入 `path`。
pub fn download(req: ureq::Request, path: &Path) -> Result<()> {
    let resp = req
        .call()
        .with_context(|| format!("发起下载请求失败: {}", path.display()))?;
    if resp.status() != 200 {
        bail!("下载响应非 200: {} ({})", resp.status(), path.display());
    }
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut out =
        fs::File::create(path).with_context(|| format!("创建文件失败: {}", path.display()))?;
    let pb = progress(total);
    let mut buf = [0u8; 16384];
    let mut got: u64 = 0;
    loop {
        let n = reader.read(&mut buf).context("读取下载流失败")?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        got += n as u64;
        pb.inc(n as u64);
        // 未知大小时实时更新已下载量
        if total == 0 {
            pb.set_message(format!("未知大小，已下载 {}", HumanBytes(got)));
        }
    }
    pb.finish_and_clear();
    if total > 0 && got != total {
        bail!("下载不完整: 期望 {total} 字节，实际 {got} 字节");
    }
    Ok(())
}

/// 构造进度条：有总大小 → 百分比条；未知 → spinner。
fn progress(total: u64) -> ProgressBar {
    if total > 0 {
        let pb = ProgressBar::new(total);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{bar:36.cyan/blue}] {percent:>3}%  {bytes}/{total_bytes}  \
                 {binary_speed}  ETA {eta}",
            )
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "),
        );
        pb
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
        pb.enable_steady_tick(std::time::Duration::from_millis(120));
        pb.set_message("下载中（未知大小）...");
        pb
    }
}
