//! Bilibili WBI 反爬签名实现（纯算法，无副作用）。
//!
//! 移植自 BiliMusic（bili-rust/src/wbi.rs），原样保留。
use md5::{Digest, Md5};

/// WBI 混淆密钥重排表（B站官方固化算法）。
const MIXIN_KEY_ENC_TAB: [u8; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42,
    19, 29, 28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51,
    30, 4, 22, 25, 54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 由原始 key 重排生成 32 位 mixin key。
fn mixin_key(orig: &str) -> String {
    let b = orig.as_bytes();
    MIXIN_KEY_ENC_TAB
        .iter()
        .filter_map(|&i| b.get(i as usize).map(|c| *c as char))
        .take(32)
        .collect()
}

/// 通过 nav 接口拉取 wbi_img.img_url 与 sub_url，合成签名密钥。
pub fn fetch_key(agent: &ureq::Agent, cookie: &str) -> anyhow::Result<String> {
    let json: serde_json::Value = agent
        .get("https://api.bilibili.com/x/web-interface/nav")
        .set("User-Agent", UA)
        .set("Accept", "*/*")
        .set("Accept-Language", "zh-CN,zh;q=0.9")
        .set("Origin", "https://www.bilibili.com")
        .set("Sec-Fetch-Site", "same-site")
        .set("Sec-Fetch-Mode", "cors")
        .set("Sec-Fetch-Dest", "empty")
        .set("Referer", "https://www.bilibili.com/")
        .set("Cookie", cookie)
        .call()?
        .into_json()?;
    let data = &json["data"];
    let img_url = data["wbi_img"]["img_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("nav 接口缺少 img_url"))?;
    let sub_url = data["wbi_img"]["sub_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("nav 接口缺少 sub_url"))?;
    let img_key = img_url.rsplit('/').next().and_then(|s| s.split('.').next());
    let sub_key = sub_url.rsplit('/').next().and_then(|s| s.split('.').next());
    match (img_key, sub_key) {
        (Some(i), Some(s)) if !i.is_empty() && !s.is_empty() => Ok(mixin_key(&format!("{i}{s}"))),
        _ => anyhow::bail!("无法从 nav 接口解析 wbi 子密钥"),
    }
}

/// 对请求参数做 WBI 签名：注入 wts、排序、追加 w_rid。
pub fn sign(params: &mut Vec<(String, String)>, wbi_key: &str) {
    let wts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    let mut has_wts = false;
    for (k, v) in params.iter_mut() {
        if k == "wts" {
            *v = wts.clone();
            has_wts = true;
        }
    }
    if !has_wts {
        params.push(("wts".to_string(), wts));
    }
    params.sort_by(|a, b| a.0.cmp(&b.0));
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let mut hasher = Md5::new();
    hasher.update(query.as_bytes());
    hasher.update(wbi_key.as_bytes());
    let w_rid = hex::encode(hasher.finalize());
    params.push(("w_rid".to_string(), w_rid));
}

/// 请求头中复用的浏览器 UA。
pub const UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/119.0.0.0 Safari/537.36";
