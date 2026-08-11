//! 命令行参数定义（clap derive，风格与 bmusic 一致，中文 help）。

use clap::Parser;
use std::path::PathBuf;

/// bilivideo —— 网站视频嗅探+下载工具。
///
/// 输入 B站链接 / 视频直链 / 任意网页，自动识别类型并下载。
///  - B站链接（bilibili.com / b23.tv / player.bilibili.com）→ B站专精 API 路径
///  - 视频直链（.mp4 / .webm / .m3u8 等）→ 直接下载
///  - 任意网页 → 通用嗅探提取页面中正在播放的视频流再下载
#[derive(Debug, Parser)]
#[command(
    name = "bvideo",
    version,
    about = "网站视频嗅探+下载工具",
    long_about = "bvideo —— B站专精 + 通用网页视频流嗅探下载。\n\
                  输入 B站链接 / 视频直链 / 任意网页，自动识别类型并下载，带进度条。"
)]
pub struct Cli {
    /// 待处理的 URL（一个或多个，依次处理）
    pub urls: Vec<String>,

    /// 输出目录（默认 download）
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// 只下载音频流（B站走 DASH audio；通用模式优先 og:audio/audio 标签）
    #[arg(short, long)]
    pub audio: bool,

    /// 仅列出嗅探到的候选流及来源，不下载
    #[arg(long)]
    pub list: bool,

    /// 候选序号选择（从 1 开始）；B站模式可传 DASH id 或序号
    #[arg(short, long)]
    pub quality: Option<String>,

    /// HLS 分片并发数（默认 8）
    #[arg(short, long, default_value_t = 8)]
    pub concurrency: usize,

    /// HLS 不合并保留分片
    #[arg(long)]
    pub no_merge: bool,

    /// HLS 合并格式（默认 mp4）
    #[arg(short, long, value_parser = ["mp4", "mkv"], default_value = "mp4")]
    pub format: String,

    /// 自定义 User-Agent
    #[arg(long)]
    pub ua: Option<String>,

    /// 自定义 Cookie（防盗链站点）
    #[arg(long)]
    pub cookie: Option<String>,

    /// 自定义 Referer
    #[arg(long)]
    pub referer: Option<String>,
}
