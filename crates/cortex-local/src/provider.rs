//! 把 cortexd 的 `/llm/stream` 包成一个 goose [`Provider`]。
//!
//! # 为什么是 `Provider` 而不是别的形状
//!
//! goose 的 `Provider` trait 只有**两个必需方法**（`get_name` 与 `stream`），
//! 其余全有默认实现。实现它之后，`LlmClient`、`Turn::run`、整个 agent 循环
//! 一行都不用改 —— 它们看到的是一个普普通通的供应商，只不过那个供应商
//! 恰好讲 HTTP 而不是讲 OpenAI 协议。
//!
//! 另一条路是给 `Turn::run` 加一层「LLM 来源」的抽象。那会为了一个部署选择
//! 去改核心循环的签名，而收益完全一样。

use std::pin::Pin;

use async_trait::async_trait;
use cortex_llm::{
    Message, MessageStream, ModelConfig, Provider, ProviderError, ProviderUsage, Tool,
};
use cortex_proto::llm::{LlmStreamChunk, LlmStreamError, LlmStreamRequest, ModelTier};
use futures::StreamExt as _;

use crate::remote::Remote;

/// goose `MessageStream` 的一项。
///
/// 起个别名不只是为了让 clippy 闭嘴：这个三层嵌套的类型在
/// [`sse_to_messages`] 与 [`parse_frame`] 的签名里各出现一次，
/// 而它们必须**逐字一致** —— 写成两个略有出入的形状，编译器会
/// 报一段谁也读不下去的错。
type StreamItem = Result<(Option<Message>, Option<ProviderUsage>), ProviderError>;

/// 经 cortexd 转发的供应商。
pub struct RemoteProvider {
    remote: Remote,
    /// 展示用的名字。带上远端地址，因为「我到底在跟谁说话」是
    /// 这条路上最容易搞错的一件事（本地直连与经代理长得一模一样）
    name: String,
}

impl RemoteProvider {
    #[must_use]
    pub fn new(remote: Remote) -> Self {
        let name = format!("cortexd-proxy({})", remote.base());
        Self { remote, name }
    }

    /// 模型档位。
    ///
    /// 本地这侧拿不到、也**不该**拿到服务端的模型名 —— 请求里只能报档位
    /// （见 `cortex_proto::llm` 的模块注释）。所以这里靠 `ModelConfig` 的
    /// 名字反推：`cheap` 那个 `ModelConfig` 是我们自己在
    /// [`crate::llm::build`] 里造的，名字固定。
    fn tier_of(model_config: &ModelConfig) -> ModelTier {
        if model_config.model_name == crate::llm::CHEAP_MODEL_NAME {
            ModelTier::Cheap
        } else {
            ModelTier::Main
        }
    }
}

#[async_trait]
impl Provider for RemoteProvider {
    fn get_name(&self) -> &str {
        &self.name
    }

    async fn stream(
        &self,
        model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        let req = LlmStreamRequest {
            tier: Self::tier_of(model_config),
            system: system.to_string(),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
        };

        let resp = self
            .remote
            .llm_stream(&req)
            .await
            // 建流失败是普通 HTTP 失败。归到 RequestFailed 而不是 NetworkError：
            // 后者在 goose 内部会被当成「值得重试」，而 4xx 重试多少次都一样
            .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

        Ok(Box::pin(sse_to_messages(resp)))
    }
}

/// 把 SSE 字节流还原成 goose 的 `MessageStream`。
///
/// 三种帧：
/// - `data:` + 无 `event:` → 一项数据（[`LlmStreamChunk`]）
/// - `event: error` → 一项错误（[`LlmStreamError`]，还原成 `ProviderError`）
/// - `:` 开头 → keep-alive 注释，丢弃
fn sse_to_messages(
    resp: reqwest::Response,
) -> Pin<Box<dyn futures::Stream<Item = StreamItem> + Send>> {
    let mut buf = String::new();
    let stream = resp.bytes_stream().flat_map(move |chunk| {
        let mut out = Vec::new();
        match chunk {
            Ok(bytes) => {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                // 一帧以空行结束；不完整的尾部留在缓冲里等下一块。
                // **必须缓冲** —— TCP 不保证一次 read 正好是一帧，
                // 按块解析会在长消息上偶发地把 JSON 切成两半
                while let Some(pos) = buf.find("\n\n") {
                    let raw = buf[..pos].to_string();
                    buf.drain(..pos + 2);
                    if let Some(item) = parse_frame(&raw) {
                        out.push(item);
                    }
                }
            }
            Err(e) => out.push(Err(ProviderError::NetworkError(format!(
                "读 /llm/stream 的响应体失败：{e}"
            )))),
        }
        futures::stream::iter(out)
    });
    Box::pin(stream)
}

/// 解一帧。返回 `None` 表示这一帧不携带数据（keep-alive、空帧）。
fn parse_frame(raw: &str) -> Option<StreamItem> {
    let mut event = None::<&str>;
    let mut data = None::<&str>;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') {
            continue;
        }
        if let Some(v) = line.strip_prefix("event: ") {
            event = Some(v);
        } else if let Some(v) = line.strip_prefix("data: ") {
            data = Some(v);
        }
    }
    let data = data?;

    if event == Some("error") {
        return Some(Err(match serde_json::from_str::<LlmStreamError>(data) {
            Ok(e) => e.into_provider(),
            // 连错误都解不出来 —— 报出**原始 payload**。这时候两侧版本
            // 大概率不一致，而那段原文是唯一的线索
            Err(e) => ProviderError::ExecutionError(format!(
                "cortexd 报了一个解析不了的错误（{e}）；原文：{data}"
            )),
        }));
    }

    Some(match serde_json::from_str::<LlmStreamChunk>(data) {
        Ok(c) => Ok((c.message, c.usage)),
        Err(e) => Err(ProviderError::ExecutionError(format!(
            "解析 /llm/stream 的数据帧失败（{e}）；原文：{data}"
        ))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// keep-alive 注释不能被当成数据。
    ///
    /// cortexd 每 15 秒发一次 `: ping`。把它解成数据帧的话，
    /// 一次「模型想得久」的推理会在中间收到几条空消息 ——
    /// 而空 `Message` 会被 agent 循环当成「模型没话说了」，提前收工。
    #[test]
    fn a_keepalive_comment_carries_no_data() {
        assert!(parse_frame(": ping").is_none(), "keep-alive 不是数据帧");
        assert!(parse_frame("").is_none(), "空帧不是数据帧");
    }

    /// `event: error` 那一帧必须还原成对应的 `ProviderError` 变体。
    ///
    /// 塌成一句泛泛的失败，本地就判断不了「该不该重试」——
    /// 限流会变成直接报错给用户，而它本来只要等一会儿。
    #[test]
    fn an_error_frame_restores_the_original_variant() {
        let frame = "event: error\ndata: {\"kind\":\"rate_limit_exceeded\",\
                     \"message\":\"慢点\",\"retry_delay_ms\":2000}";
        let got = parse_frame(frame).expect("这一帧带数据");
        match got {
            Err(ProviderError::RateLimitExceeded {
                details,
                retry_delay,
            }) => {
                assert_eq!(details, "慢点");
                assert_eq!(retry_delay, Some(std::time::Duration::from_millis(2000)));
            }
            other => panic!("限流帧必须还原成 RateLimitExceeded，实际：{other:?}"),
        }
    }

    /// 只带用量的尾项要能解出来。goose 的流末尾就是这个形状。
    #[test]
    fn a_usage_only_frame_parses() {
        let frame = "data: {\"usage\":{\"model\":\"m\",\"usage\":\
                     {\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}";
        let (msg, usage) = parse_frame(frame).expect("带数据").expect("不是错误帧");
        assert!(msg.is_none(), "尾项本来就没有 message");
        assert!(usage.is_some(), "用量必须解出来");
    }

    /// 解不出来的帧要把**原文**带进错误消息。
    ///
    /// 这是两侧版本不一致时唯一的线索。只报一句「解析失败」的话，
    /// 排查就得去服务端翻日志，而那边看到的是一次完全正常的响应。
    #[test]
    fn an_unparseable_frame_reports_the_raw_payload() {
        let got = parse_frame("data: {不是 json}").expect("带数据");
        let msg = got.expect_err("解不出来必须是错误").to_string();
        assert!(
            msg.contains("不是 json"),
            "解析失败必须带上原文，实际：{msg}"
        );
    }
}
