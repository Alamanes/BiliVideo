# bilivideo (bvideo)

网站视频嗅探+下载工具 (Rust) —— **B站专精** + **通用网页视频流提取**，带美观进度条。

输入一个 URL，自动识别类型并下载：
- **B站链接**（`www.bilibili.com/video/BVxxx`、`b23.tv` 短链、`player.bilibili.com`）→ 走 B站专精 API 路径（DASH 流）
- **视频直链**（`.mp4` / `.webm` / `.m3u8` 等）→ 直接下载
- **任意网页** → 通用嗅探提取页面中正在播放的视频流再下载

## 安装

### macOS / Linux（Homebrew）

```sh
brew tap orangerin/apps
brew install bilivideo
```

公式来自 [`Shiorangerin/homebrew-apps`](https://github.com/Shiorangerin/homebrew-apps)，自动按架构选择 `aarch64` / `x86_64` 预编译二进制，秒装、无需本地编译。依赖系统 `ffmpeg`（公式已声明，缺时会自动装）。

安装后即得全局命令 `bvideo`。

### Windows

从 [Releases](https://github.com/Shiorangerin/BiliVideo/releases) 下载 `bilivideo-x86_64-pc-windows-msvc.zip`，解压得到 `bvideo.exe`，放入任意 `PATH` 目录（推荐放进 `%USERPROFILE%\.cargo\bin` 或自建目录后加入环境变量）。

需系统装有 `ffmpeg`：

```powershell
winget install Gyan.FFmpeg
# 或
scoop install ffmpeg
```

### 从源码构建

需要 **Rust 1.70+** 与系统级 **ffmpeg**。

```sh
git clone https://github.com/Shiorangerin/BiliVideo.git
cd BiliVideo
cargo build --release
# 产物：./target/release/bvideo
```

把 `bvideo` 放进 `PATH` 即可全局调用。

## 用法

```
bvideo [OPTIONS] <URL...>
```

### 示例

```bash
# 下载 B站视频（自动选最高码率，ffmpeg 合并音视频）
bvideo "https://www.bilibili.com/video/BV1GJ411x7h7"

# 只列流不下载（查看可用清晰度与 DASH id）
bvideo --list "https://www.bilibili.com/video/BV1GJ411x7h7"

# 指定清晰度：-q 可传 DASH id（如 80=1080P、64=720P、32=480P）或候选序号（1 起）
bvideo -q 32 "https://www.bilibili.com/video/BV1GJ411x7h7"

# 只下载音频流（B站走 DASH audio）
bvideo -a "https://www.bilibili.com/video/BV1GJ411x7h7"

# b23.tv 短链 / player 页同样支持
bvideo "https://b23.tv/xxxxxx"

# 视频直链
bvideo "https://example.com/videos/movie.mp4"

# 通用网页嗅探：先列出候选流及来源
bvideo --list "https://example.com/watch/video-page"

# 嗅探后按序号选流下载（1 起）
bvideo -q 2 "https://example.com/watch/video-page"

# 嗅探网页时优先音频流（og:audio / <audio> 标签）
bvideo -a "https://example.com/music-page"

# HLS 下载：自定义分片并发（默认 8）、保留分片、合并格式
bvideo -c 16 "https://example.com/live/playlist.m3u8"
bvideo --no-merge "https://example.com/live/playlist.m3u8"
bvideo -f mkv "https://example.com/live/playlist.m3u8"

# 防盗链站点：自定义 UA / Cookie / Referer
bvideo --referer "https://example.com" --cookie "session=xxx" "https://cdn.example.com/v.mp4"

# 批量下载多个 URL
bvideo "https://.../a.mp4" "https://.../b.mp4" "https://www.bilibili.com/video/BVxxxx"
```

### 全部参数

```
-o, --output <dir>     输出目录（默认 download）
-a, --audio            只下载音频流（B站走 DASH audio；通用模式优先 og:audio/audio 标签）
    --list             仅列出嗅探到的候选流及来源，不下载
-q, --quality <n>      候选序号选择（从 1 开始）；B站模式可传 DASH id 或序号
-c, --concurrency <n>  HLS 分片并发数（默认 8）
    --no-merge          HLS 不合并保留分片
-f, --format <mp4|mkv> HLS 合并格式（默认 mp4）
    --ua <str>          自定义 User-Agent
    --cookie <str>      自定义 Cookie（防盗链站点）
    --referer <str>     自定义 Referer
```

## 支持的站点类型

| 类型 | 说明 |
| --- | --- |
| B站视频 | 走官方 view / playurl(WBI 签名) API，取 DASH 音视频流，ffmpeg 无损合并 |
| b23.tv 短链 | 自动展开重定向到完整链接 |
| 视频直链 | `.mp4` / `.webm` / `.ts` / `.flv` / `.mov` / `.mkv` / `.m4a` 等 |
| HLS (m3u8) | 支持 master playlist 自动选最高带宽变体、AES-128 解密（显式 IV 或默认取分片序号）、并发分片下载、ffmpeg `-c copy` 合并或二进制拼接 |
| 任意网页 | 按优先级嗅探：`<video>`/`<audio>` 标签 → `og:video`/`og:audio` meta → 内嵌 JSON（`__INITIAL_STATE__`/`__NUXT__`/`__NEXT_DATA__`/`player.__vars`）→ 全文本正则 |

## 注意事项

- B站 API 有风控：高频请求可能触发「未能从首页获取 buvid3」，稍等片刻重试即可。
- B站未登录时部分视频清晰度受限（最高约 1080P），需要更高清晰度请 `--cookie` 传入浏览器 Cookie。
- B站 DASH 视频流与音频流分离，合并依赖 ffmpeg；未安装时视频模式会报错并保留分片。
- 通用嗅探只提取页面 HTML 中静态存在的流地址；纯 JS 动态加载（如点击后才请求）的站点可能嗅探不到，可配合浏览器抓包后使用直链。
- HLS 的 fMP4 分片（`EXT-X-MAP`）合并需要 ffmpeg。
- 请遵守目标网站的条款与版权规定，仅下载你有权获取的内容。

## 实现说明

- B站逻辑移植自 [BiliMusic](https://github.com/Shiorangerin/BiliMusic)（`bili-rust`）：buvid3/b_nut 风控 cookie、view API、playurl WBI 签名（fnval=4048）、最高码率选流、文件名清洗。
- 进度条使用 [indicatif](https://github.com/console-rs/indicatif)：直链/B站下载显示百分比、已下载/总大小、速度、ETA；未知大小时显示 spinner。
- HLS AES-128 解密使用 `aes` + `cbc` crate（PKCS7 填充，失败退回无填充）；IV 缺省取分片序号的大端 16 字节（RFC 8216）。
- 代码模块：`main`(路由) / `cli` / `bili` / `wbi` / `sniff` / `hls` / `download` / `ffmpeg` / `util`。
