//! ffmpeg 检测与封装调用。

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// 检查系统中是否存在 ffmpeg。
pub fn which_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 调用 ffmpeg：`ffmpeg [pre] -i in1 -i in2 ... [post] -y out`。
///
/// `pre` 为输入选项（如 `-f concat -safe 0`），`post` 为输出选项（如 `-c copy`）。
pub fn run(pre: &[&str], inputs: &[&Path], post: &[&str], output: &Path) -> Result<()> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(pre);
    for i in inputs {
        cmd.arg("-i").arg(i);
    }
    cmd.args(post)
        .arg("-y")
        .arg(output)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let status = cmd
        .status()
        .context("未找到 ffmpeg，请先安装并加入 PATH")?;
    if !status.success() {
        bail!("ffmpeg 返回非零状态: {}", status.code().unwrap_or(-1));
    }
    Ok(())
}
