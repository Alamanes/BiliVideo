//! m3u8 解析与下载：AES-128 解密、分片并发下载、ffmpeg 合并或二进制拼接。
//!
//! 支持：
//!  - EXT-X-KEY AES-128 解密（aes + cbc，IV 缺省取分片序号的大端 16 字节）
//!  - 分片并发下载（rayon，默认并发 8）
//!  - master playlist（EXT-X-STREAM-INF）自动选最高带宽变体递归
//!  - 合并：ffmpeg 可用则 `-c copy` 转 mp4/mkv，否则直接二进制拼接 ts
//!  - fMP4 分片（EXT-X-MAP）需要 ffmpeg，走 ffmpeg 原生解析

use crate::{download, ffmpeg, util};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// HLS 下载选项。
#[derive(Clone, Copy)]
pub struct Options<'a> {
    pub output: &'a Path,
    /// 输出文件名（不含扩展名）
    pub name: &'a str,
    /// 分片并发数
    pub concurrency: usize,
    /// 是否合并分片（false 时保留分片目录）
    pub merge: bool,
    /// 合并格式："mp4" | "mkv"
    pub format: &'a str,
    pub http: download::HttpOpts<'a>,
}

/// 单个分片。
struct Segment {
    uri: String,
    /// 分片序号（默认 IV 的来源）
    seq: u64,
    key: Option<KeyInfo>,
}

/// AES-128 密钥信息。
#[derive(Clone)]
struct KeyInfo {
    uri: String,
    iv: Option<Vec<u8>>,
}

/// 解析结果。
struct Playlist {
    segments: Vec<Segment>,
    /// 是否出现 EXT-X-MAP（fMP4，需 ffmpeg 原生处理）
    has_map: bool,
}

/// 主入口：下载 m3u8 并合并/保留分片，返回产物路径。
pub fn download(url: &str, opt: &Options) -> Result<PathBuf> {
    let name = util::sanitize(opt.name);
    let opt = Options {
        name: &name,
        ..*opt
    };
    fs::create_dir_all(opt.output)
        .with_context(|| format!("创建输出目录失败: {}", opt.output.display()))?;
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build();

    let pl = load_playlist(&agent, url, 0, &opt)?;
    if pl.has_map {
        // fMP4 分片：交给 ffmpeg 原生解析（含解密与 init 分片）
        return ffmpeg_native(url, &opt);
    }
    let parts = download_segments(&agent, &pl, &opt)?;
    merge(&parts, &opt)
}

/// 递归加载 m3u8：master playlist 选最高带宽变体。
fn load_playlist(agent: &ureq::Agent, url: &str, depth: usize, opt: &Options) -> Result<Playlist> {
    if depth > 3 {
        bail!("m3u8 嵌套过深（master playlist 递归超过 3 层）");
    }
    let text = fetch(agent, url, opt)?;
    let mut key: Option<KeyInfo> = None;
    let mut seq: u64 = 0;
    let mut segments: Vec<Segment> = Vec::new();
    let mut variants: Vec<(u64, String)> = Vec::new();
    let mut has_map = false;
    let mut pending_bandwidth: Option<u64> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXT-X-KEY:") {
            // 后续分片使用该密钥（绝大多数站点为单密钥）
            key = parse_key(line, url);
        } else if line.starts_with("#EXT-X-MEDIA-SEQUENCE:") {
            seq = line
                .split(':')
                .nth(1)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
        } else if line.starts_with("#EXT-X-MAP") {
            has_map = true;
        } else if line.starts_with("#EXT-X-STREAM-INF:") {
            // 下一行是变体播放列表 URI，记录带宽
            pending_bandwidth = line.split(':').nth(1).and_then(|s| {
                s.split(',').find_map(|kv| {
                    let mut it = kv.split('=');
                    if it.next() == Some("BANDWIDTH") {
                        it.next().and_then(|v| v.trim().parse().ok())
                    } else {
                        None
                    }
                })
            });
        } else if line.starts_with('#') {
            continue;
        } else {
            // 数据行：变体 URI 或分片 URI
            let uri = util::resolve_url(url, line);
            if let Some(bw) = pending_bandwidth.take() {
                variants.push((bw, uri));
            } else {
                segments.push(Segment {
                    uri,
                    seq,
                    key: key.clone(),
                });
                seq += 1;
            }
        }
    }

    if !variants.is_empty() {
        // master playlist：选带宽最高的变体递归
        let (_, best) = variants
            .into_iter()
            .max_by_key(|(b, _)| *b)
            .ok_or_else(|| anyhow::anyhow!("master playlist 无可用变体"))?;
        return load_playlist(agent, &best, depth + 1, opt);
    }
    if segments.is_empty() {
        bail!("m3u8 中未解析到任何分片: {url}");
    }
    Ok(Playlist { segments, has_map })
}

/// 解析 `#EXT-X-KEY:` 行：METHOD=AES-128 时返回 (key_uri, iv)。
fn parse_key(line: &str, base: &str) -> Option<KeyInfo> {
    let method = attr(line, "METHOD")?;
    if !method.eq_ignore_ascii_case("AES-128") {
        return None; // METHOD=NONE 等无需解密
    }
    let uri = attr(line, "URI")?;
    let uri = util::resolve_url(base, &uri);
    // IV=0x...（16 字节十六进制）
    let iv = attr(line, "IV").and_then(|s| {
        let hexs = s.trim_start_matches("0x").trim_start_matches("0X");
        let bytes = hex::decode(hexs).ok()?;
        (bytes.len() == 16).then_some(bytes)
    });
    Some(KeyInfo { uri, iv })
}

/// 提取 `#EXT-X-KEY:` 行中 `NAME=value` 的属性值（兼容带引号/裸值）。
fn attr(line: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let upper = line.to_ascii_uppercase();
    let idx = upper.find(&needle.to_ascii_uppercase())?;
    let rest = &line[idx + needle.len()..];
    if let Some(q) = rest.strip_prefix('"') {
        let end = q.find('"')?;
        Some(q[..end].to_string())
    } else {
        let end = rest.find(',').unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

/// 抓取 m3u8 文本。
fn fetch(agent: &ureq::Agent, url: &str, opt: &Options) -> Result<String> {
    request(agent, url, opt)
        .call()
        .with_context(|| format!("获取 m3u8 失败: {url}"))?
        .into_string()
        .context("读取 m3u8 内容失败")
}

/// 抓取二进制（分片 / 密钥）。
fn fetch_bytes(agent: &ureq::Agent, url: &str, opt: &Options) -> Result<Vec<u8>> {
    let resp = request(agent, url, opt)
        .call()
        .with_context(|| format!("下载失败: {url}"))?;
    if resp.status() != 200 {
        bail!("下载响应非 200: {} ({})", resp.status(), url);
    }
    let mut reader = resp.into_reader();
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    Ok(buf)
}

/// 构造带请求头的 GET 请求。
fn request(agent: &ureq::Agent, url: &str, opt: &Options) -> ureq::Request {
    download::request(agent, url, &opt.http)
}

/// 并发下载全部分片（解密后返回，顺序与播放列表一致）。
fn download_segments(agent: &ureq::Agent, pl: &Playlist, opt: &Options) -> Result<Vec<Vec<u8>>> {
    // 预取所有用到的 AES key（按 key URI 去重）
    let mut key_map: HashMap<String, Vec<u8>> = HashMap::new();
    for seg in &pl.segments {
        if let Some(k) = &seg.key {
            if !key_map.contains_key(&k.uri) {
                let kb = fetch_bytes(agent, &k.uri, opt)
                    .with_context(|| format!("获取 AES key 失败: {}", k.uri))?;
                if kb.len() != 16 {
                    bail!("AES-128 key 长度应为 16 字节，实际 {}", kb.len());
                }
                key_map.insert(k.uri.clone(), kb);
            }
        }
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opt.concurrency.clamp(1, 64))
        .build()
        .context("创建分片下载线程池失败")?;
    let pb = indicatif::ProgressBar::new(pl.segments.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "分片下载 [{bar:30.cyan/blue}] {pos}/{len}  {msg}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏  "),
    );
    pb.set_message(format!("并发 {}", opt.concurrency));
    let data: Result<Vec<Vec<u8>>> = pool.install(|| {
        use rayon::prelude::*;
        pl.segments
            .par_iter()
            .map(|seg| {
                let raw = fetch_bytes(agent, &seg.uri, opt)
                    .with_context(|| format!("分片下载失败: {}", seg.uri))?;
                let out = if let Some(k) = &seg.key {
                    let key = key_map.get(&k.uri).context("缺少 AES key 数据")?;
                    // IV 缺省取分片序号的大端 16 字节（RFC 8216）
                    let iv = k.iv.clone().unwrap_or_else(|| default_iv(seg.seq));
                    decrypt(&raw, key, &iv)?
                } else {
                    raw
                };
                pb.inc(1);
                Ok(out)
            })
            .collect()
    });
    pb.finish_and_clear();
    data
}

/// 默认 IV：分片序号的大端 16 字节。
fn default_iv(seq: u64) -> Vec<u8> {
    (0..16).map(|i| ((seq >> (8 * (15 - i))) & 0xff) as u8).collect()
}

/// AES-128-CBC 解密（PKCS7 填充；部分站点无填充时退回 NoPadding）。
fn decrypt(data: &[u8], key: &[u8], iv: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::block_padding::{NoPadding, Pkcs7};
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    type Aes128Cbc = cbc::Decryptor<aes::Aes128>;
    let mut buf = data.to_vec();
    // 先按 PKCS7 填充解密
    let dec = Aes128Cbc::new_from_slices(key, iv).context("AES key/IV 长度非法")?;
    match dec.decrypt_padded_mut::<Pkcs7>(&mut buf) {
        Ok(out) => Ok(out.to_vec()),
        Err(_) => {
            // 未按 PKCS7 填充的流，退回无填充解密（重建 cipher）
            let mut buf2 = data.to_vec();
            let dec = Aes128Cbc::new_from_slices(key, iv).context("AES key/IV 长度非法")?;
            dec.decrypt_padded_mut::<NoPadding>(&mut buf2)
                .map_err(|_| anyhow::anyhow!("AES 解密失败"))?;
            Ok(buf2)
        }
    }
}

/// 合并分片：ffmpeg concat demuxer 无损合并，或直接二进制拼接。
fn merge(parts: &[Vec<u8>], opt: &Options) -> Result<PathBuf> {
    let ext = if opt.format == "mkv" { "mkv" } else { "mp4" };
    let out = opt.output.join(format!("{}.{}", opt.name, ext));
    if !opt.merge {
        // 保留分片目录
        let dir = opt.output.join(format!("{}.parts", opt.name));
        fs::create_dir_all(&dir)?;
        for (i, p) in parts.iter().enumerate() {
            fs::write(dir.join(format!("part_{:05}.ts", i)), p)?;
        }
        println!(
            "已保留分片: {}（共 {} 个，未合并）",
            dir.display(),
            parts.len()
        );
        return Ok(dir);
    }
    if ffmpeg::which_ffmpeg() {
        // 写分片到临时目录，用 concat demuxer 无损合并
        let dirname = format!(".{}.parts", opt.name.replace('\'', "_"));
        let tmp = opt.output.join(dirname);
        fs::create_dir_all(&tmp)?;
        let mut list = String::new();
        for (i, p) in parts.iter().enumerate() {
            let f = tmp.join(format!("part_{:05}.ts", i));
            fs::write(&f, p)?;
            list.push_str(&format!("file '{}'\n", f.display()));
        }
        let listf = tmp.join("concat.txt");
        fs::write(&listf, list)?;
        let res = ffmpeg::run(
            &["-f", "concat", "-safe", "0"],
            &[listf.as_path()],
            &["-c", "copy"],
            &out,
        );
        let _ = fs::remove_dir_all(&tmp);
        res.with_context(|| format!("ffmpeg 合并失败: {}", out.display()))?;
    } else {
        // 无 ffmpeg：直接二进制拼接
        let mut f = fs::File::create(&out)
            .with_context(|| format!("创建文件失败: {}", out.display()))?;
        for p in parts {
            f.write_all(p)?;
        }
        println!("未检测到 ffmpeg，已二进制拼接为 {}", out.display());
    }
    println!("完成: {}", out.display());
    Ok(out)
}

/// fMP4 分片：交给 ffmpeg 原生解析 m3u8（含 AES 解密与 init 分片）。
fn ffmpeg_native(url: &str, opt: &Options) -> Result<PathBuf> {
    if !ffmpeg::which_ffmpeg() {
        bail!("该 m3u8 为 fMP4 分片（EXT-X-MAP），需要 ffmpeg 才能合并");
    }
    let ext = if opt.format == "mkv" { "mkv" } else { "mp4" };
    let out = opt.output.join(format!("{}.{}", opt.name, ext));
    // 透传自定义请求头给 ffmpeg（http 协议 -headers 选项）
    let mut headers = format!("User-Agent: {}\r\n", opt.http.ua);
    if let Some(c) = opt.http.cookie {
        headers.push_str(&format!("Cookie: {c}\r\n"));
    }
    if let Some(r) = opt.http.referer {
        headers.push_str(&format!("Referer: {r}\r\n"));
    }
    let pre = ["-headers", headers.as_str()];
    ffmpeg::run(&pre, &[Path::new(url)], &["-c", "copy"], &out)
        .with_context(|| format!("ffmpeg 下载 fMP4 失败: {url}"))?;
    println!("完成: {}", out.display());
    Ok(out)
}
