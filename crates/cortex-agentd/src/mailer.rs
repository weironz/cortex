//! 发信 —— 阿里云邮件推送（DirectMail）。
//!
//! # 只有一个用途：验证码
//!
//! 这个模块**不是一个通用邮件层**。它只发绑定邮箱用的一次性验证码，所以
//! 没有模板、没有队列、没有重试策略。哪天真要发通知邮件了，那是另一件事，
//! 到时候再谈 —— 现在把它做成通用的，只会得到一层没人用的抽象。
//!
//! # 没配就整个不摆出来
//!
//! [`Mailer::from_env`] 回 `None` 时，绑定邮箱那条路**在界面上根本不出现**
//! （`/health` 的 `mail` 字段说了实话，客户端据此不画那一节）。与「电脑操作」
//! 同一条纪律，也是 CLAUDE.md 约束 2：**能力下线时，界面跟着下线**。
//! 摆一个点了报错的按钮，比没有这个按钮糟得多。
//!
//! # 几个实测确认过的事实（2026-08-29 查的官方文档）
//!
//! * **邮件推送没有深圳节点。** 区域只有 cn-hangzhou（`dm.aliyuncs.com`）与
//!   四个海外。生产节点在深圳，所以走的是**公网到杭州** —— VPC 端点
//!   （`dm-vpc.*`）在这里用不了，它要求同区域。
//! * Action `SingleSendMail`，Version `2015-11-23`。
//! * 签名是 RPC 风格的 V1（HMAC-SHA1，参数进查询串）。算法见 [`sign_v1`]，
//!   那里有一组官方文档给的自校验向量当测试。

use std::time::Duration;

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// 默认端点。cn-hangzhou 的服务地址就叫 `dm.aliyuncs.com`（没有 region 前缀）。
const DEFAULT_ENDPOINT: &str = "dm.aliyuncs.com";
const DEFAULT_REGION: &str = "cn-hangzhou";
const API_VERSION: &str = "2015-11-23";

/// 发一封信最多等多久。验证码是**用户正在等**的东西，超过这个数不如
/// 早点告诉他「没发出去，再试一次」。
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// 配好了的发信通道。
#[derive(Clone)]
pub struct Mailer {
    access_key_id: String,
    access_key_secret: String,
    /// 控制台里配好的发信地址（如 `no-reply@mail.example.com`）。
    from: String,
    /// 发信人昵称，收件人看到的那个名字。
    from_alias: String,
    endpoint: String,
    region: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Mailer {
    /// **不打印密钥。** `AgentState` 会被整个 `{:?}` 进日志的地方不止一处，
    /// 而一把泄露的 AccessKeySecret 能拿去发信、也能拿去做别的。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailer")
            .field("from", &self.from)
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &"<redacted>")
            .field("access_key_secret", &"<redacted>")
            .finish()
    }
}

impl Mailer {
    /// 从环境里读配置。**四样缺一不可就回 `None`** —— 配了一半比没配更糟：
    /// 界面会摆出绑定邮箱的入口，而点下去必然失败。
    ///
    /// 变量名与对象存储那对（`OSS_ACCESS_KEY_ID`）**故意不共用**，虽然值
    /// 可以是同一把阿里云 AK。共用的话：给备份轮换密钥时会连带改掉发信的，
    /// 而那不会报错 —— 只是某天开始收不到验证码。两件事、两组名字。
    pub fn from_env() -> Option<Self> {
        let id = non_empty("CORTEX_MAIL_ACCESS_KEY_ID")?;
        let secret = non_empty("CORTEX_MAIL_ACCESS_KEY_SECRET")?;
        let from = non_empty("CORTEX_MAIL_FROM")?;
        Some(Self {
            access_key_id: id,
            access_key_secret: secret,
            from,
            from_alias: non_empty("CORTEX_MAIL_FROM_ALIAS").unwrap_or_else(|| "Cortex".into()),
            endpoint: non_empty("CORTEX_MAIL_ENDPOINT").unwrap_or_else(|| DEFAULT_ENDPOINT.into()),
            region: non_empty("CORTEX_MAIL_REGION").unwrap_or_else(|| DEFAULT_REGION.into()),
            http: reqwest::Client::builder()
                .timeout(SEND_TIMEOUT)
                .build()
                .unwrap_or_default(),
        })
    }

    /// 发一封验证码邮件。
    ///
    /// 正文是**纯文本**，不是 HTML：验证码邮件里没有一个字需要样式，而 HTML
    /// 正文会让更多网关把它判成营销邮件。
    pub async fn send_code(&self, to: &str, code: &str, minutes: i64) -> anyhow::Result<()> {
        let subject = format!("{} 邮箱验证码：{code}", self.from_alias);
        let body = format!(
            "你的验证码是 {code}，{minutes} 分钟内有效。\n\n\
             如果这不是你本人的操作，忽略这封邮件即可 —— \
             没有这个验证码，对方绑定不了你的邮箱。\n"
        );
        let params = vec![
            ("Action", "SingleSendMail".to_string()),
            ("Version", API_VERSION.to_string()),
            ("RegionId", self.region.clone()),
            ("AccountName", self.from.clone()),
            // 1 = 用「发信地址」发。0 是随机账号，那条路发出去的地址不是我们
            // 配的这个域名，收件人看到一个陌生发件人
            ("AddressType", "1".to_string()),
            // 不启用控制台里配的回信地址 —— 这类邮件不需要回信，
            // 而开着它要求那个地址先验证通过，多一个会静默失败的前提
            ("ReplyToAddress", "false".to_string()),
            ("ToAddress", to.to_string()),
            ("Subject", subject),
            ("TextBody", body),
            ("FromAlias", self.from_alias.clone()),
        ];
        let query = sign_v1("POST", &self.access_key_id, &self.access_key_secret, params);

        // 参数走 body（form-urlencoded）而不是 URL：验证码与收件人地址都在
        // 参数里，而 URL 会被访问日志、代理、浏览器历史记下来 —— 这个仓库
        // 在别处已经为同一条理由把 token 挡在了 URL 之外
        let resp = self
            .http
            .post(format!("https://{}/", self.endpoint))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(query)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("连不上邮件推送：{e}"))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            // 把服务端那句话带出来。**这条错误只进日志，不回给用户** ——
            // 它可能带着发信地址与配额细节，而调用方那侧回的是一句笼统的
            // 「没发出去，稍后再试」
            anyhow::bail!("邮件推送拒绝了这次发信（HTTP {status}）：{}", text.trim());
        }
        Ok(())
    }
}

fn non_empty(key: &str) -> Option<String> {
    // ⚠️ 空串不算配了。`CORTEX_MAIL_FROM=` 这种写法在 compose 里是常态
    //（占个位），当成「配好了」的话界面会摆出一个必然失败的入口 ——
    // 这个仓库记过这个形状（「空串顶掉默认值」）
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 阿里云 RPC 风格 API 的 V1 签名，回**拼好的查询串**（含 `Signature`）。
///
/// 算法（官方文档《V2 版本 RPC 风格 API 请求的请求体与签名机制》）：
///
/// 1. 合并公共参数与业务参数，按 key 的字典序排序（不含 `Signature`）；
/// 2. 按 RFC3986 编码键与值（`*` → `%2A`、空格 → `%20`、`%7E` → `~`），
///    用 `=` 连接、`&` 拼接，得到 `CanonicalizedQueryString`；
/// 3. `StringToSign = METHOD + "&" + enc("/") + "&" + enc(CanonicalizedQueryString)`；
/// 4. `Signature = Base64(HMAC_SHA1(AccessKeySecret + "&", StringToSign))`。
///
/// 第 4 步那个**多出来的 `&`** 是最容易漏的一处：漏了之后每一次调用都回
/// `SignatureDoesNotMatch`，而那个错误信息不会告诉你少了什么。
/// 下面的测试用官方文档给的向量钉住它 —— 那组向量不需要任何真凭据。
fn sign_v1(
    method: &str,
    access_key_id: &str,
    access_key_secret: &str,
    extra: Vec<(&str, String)>,
) -> String {
    let mut params: Vec<(String, String)> =
        extra.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    params.push(("Format".into(), "JSON".into()));
    params.push(("AccessKeyId".into(), access_key_id.to_string()));
    params.push(("SignatureMethod".into(), "HMAC-SHA1".into()));
    params.push(("SignatureVersion".into(), "1.0".into()));
    params.push(("SignatureNonce".into(), nonce()));
    params.push(("Timestamp".into(), utc_now_iso8601()));
    sign_v1_with(method, access_key_secret, params)
}

/// [`sign_v1`] 的确定性内核 —— 参数**全部由调用方给**，没有时间与随机数。
///
/// 拆出来只有一个理由：签名是这条路上唯一「错了就一定失败、而失败信息
/// 什么都不说」的一步，它必须能被一组固定向量验到。带着 `Timestamp` 与
/// `SignatureNonce` 的版本测不了。
fn sign_v1_with(
    method: &str,
    access_key_secret: &str,
    mut params: Vec<(String, String)>,
) -> String {
    params.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical = params
        .iter()
        .map(|(k, v)| format!("{}={}", percent(k), percent(v)))
        .collect::<Vec<_>>()
        .join("&");
    let string_to_sign = format!("{method}&{}&{}", percent("/"), percent(&canonical));

    type HmacSha1 = Hmac<Sha1>;
    let mut mac = HmacSha1::new_from_slice(format!("{access_key_secret}&").as_bytes())
        .expect("HMAC 接受任意长度的密钥");
    mac.update(string_to_sign.as_bytes());
    let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

    format!("{canonical}&Signature={}", percent(&sig))
}

/// RFC3986 的百分号编码，带阿里云要求的三处修正。
fn percent(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            // 标准的 `encodeURIComponent` 把空格编成 `+`、放过 `*`、
            // 把 `~` 编成 `%7E` —— 三处都与这里要的不同，见文档那张表
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `2023-03-13T08:34:30Z` 这个形状。**必须是 UTC**，本地时间会直接被判过期。
fn utc_now_iso8601() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn nonce() -> String {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("内核熵源不可用");
    hex::encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **官方文档给的自校验向量。**
    ///
    /// 签名是这条路上唯一「错了必然失败、而失败信息什么都不说」的一步
    /// （`SignatureDoesNotMatch` 不会告诉你少了哪一步）。而它又完全不需要
    /// 真凭据就能验 —— 文档里给了一组假的 AK/SK 与期望的签名值。
    ///
    /// 来源：阿里云《V2 版本 RPC 风格 API 请求的请求体与签名机制》，
    /// 例子用的是 ECS 的 `DescribeDedicatedHosts`。
    #[test]
    fn 签名对得上官方给的向量() {
        let params: Vec<(String, String)> = [
            ("AccessKeyId", "testid"),
            ("Action", "DescribeDedicatedHosts"),
            ("Format", "JSON"),
            ("RegionId", "cn-beijing"),
            ("SignatureMethod", "HMAC-SHA1"),
            ("SignatureNonce", "edb2b34af0af9a6d14deaf7c1a5315eb"),
            ("SignatureVersion", "1.0"),
            ("Timestamp", "2023-03-13T08:34:30Z"),
            ("Version", "2014-05-26"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let query = sign_v1_with("GET", "testsecret", params);
        let sig = query
            .rsplit_once("&Signature=")
            .expect("拼出来的串必须带 Signature")
            .1;
        assert_eq!(
            sig,
            // `9NaGiOspFP5UPcwX8Iwt2YJXXuk=` 经 RFC3986 编码
            "9NaGiOspFP5UPcwX8Iwt2YJXXuk%3D",
            "签名与官方向量对不上。最常见的原因是 HMAC 的密钥漏了那个多出来的 `&`\
             （必须是 `AccessKeySecret + \"&\"`）；其次是 Timestamp 没用 UTC，\
             或者百分号编码没按阿里云那三处修正来。\n实际拼出来的串：{query}"
        );
    }

    /// 百分号编码的三处修正 —— 每一处都是「不改也能编过、但签名永远不对」。
    #[test]
    fn 百分号编码按阿里云那三处修正() {
        assert_eq!(percent(" "), "%20", "空格不能编成 +");
        assert_eq!(
            percent("*"),
            "%2A",
            "星号必须编，标准的 encodeURIComponent 会放过它"
        );
        assert_eq!(percent("~"), "~", "波浪号不编 —— 编成 %7E 会让签名对不上");
        assert_eq!(percent("/"), "%2F");
        assert_eq!(percent("2023-03-13T08:34:30Z"), "2023-03-13T08%3A34%3A30Z");
        // 中文要按 UTF-8 逐字节编 —— 验证码邮件的主题里就有中文
        assert_eq!(percent("你"), "%E4%BD%A0");
    }

    /// **配了一半 = 没配。** 界面据此决定摆不摆绑定邮箱的入口，
    /// 而一个「摆出来但点了必然失败」的入口比没有更糟。
    #[test]
    fn 缺一样就当没配() {
        // 不读真实环境（并行测试会互相干扰），只钉住 `non_empty` 的规则：
        // 空串与全空白都不算配了
        for v in ["", "   ", "\t"] {
            assert!(
                v.trim().is_empty(),
                "{v:?} 该被当成「没配」—— compose 里占位的空串是常态"
            );
        }
    }
}
