//! 模型只发工具调用时，真实流、续写上下文和归档回复都应先包含动作说明。
//! 模型替身只控制输出形状，不访问外部供应商，也不真的启动应用。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cortex_agent::{AgentEvent, Approval, ConfirmRequest, ToolHost, ToolResult};
use serde_json::json;

struct Host(AtomicUsize);

#[async_trait::async_trait]
impl ToolHost for Host {
    async fn confirm(&self, _: &ConfirmRequest<'_>) -> Approval {
        Approval::Allow
    }

    async fn open_application(&self, _: &serde_json::Value) -> ToolResult {
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolResult::ok("requested")
    }
}

/// 开场与工具同帧、分帧及漏写都穿过真实的流解析、循环和工具分派。
#[tokio::test]
async fn opening_precedes_tools_without_duplicates_and_survives_history() {
    for (opening, tool_rounds, combined) in [
        (None, 2, false),
        (Some("我来打开 Chrome。"), 1, false),
        (Some("我来打开 Chrome。"), 1, true),
        (None, 0, false),
    ] {
        let rounds = Arc::new(AtomicUsize::new(0));
        let requests = Arc::clone(&rounds);
        let router = axum::Router::new().fallback(axum::routing::post(
            move |axum::Json(request): axum::Json<serde_json::Value>| {
                let round = requests.fetch_add(1, Ordering::SeqCst);
                async move {
                    if round > 0 {
                        let messages = request["messages"].to_string();
                        let expected = opening.unwrap_or("我先尝试打开应用");
                        assert!(messages.contains(expected),
                            "下一次模型调用必须看得到已发给用户的开场：round={round}, opening={opening:?}, tool_rounds={tool_rounds}, messages={messages}");
                    }
                    let delta = if round < tool_rounds {
                        let mut delta = json!({"role":"assistant","tool_calls":[{
                            "index":0,"id":format!("open_{round}"),"type":"function",
                            "function":{"name":"open_application","arguments":"{\"application\":\"Chrome\"}"}
                        }]});
                        if combined && round == 0 {
                            delta["content"] = json!(opening);
                        }
                        delta
                    } else {
                        json!({"role":"assistant","content":"处理结束。"})
                    };
                    let frame = json!({"id":"preamble", "object":"chat.completion.chunk",
                        "created":0, "model":"gpt-4o","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                    let end = json!({"id":"preamble", "object":"chat.completion.chunk",
                        "created":0, "model":"gpt-4o","choices":[{
                            "index":0,"delta":{},"finish_reason":if round < tool_rounds {"tool_calls"} else {"stop"}
                        }]});
                    let preamble = if round == 0 && tool_rounds > 0 && !combined {
                        opening.map(|text| {
                            let frame = json!({"id":"preamble", "object":"chat.completion.chunk",
                                "created":0, "model":"gpt-4o","choices":[{
                                    "index":0,"delta":{"content":text},"finish_reason":null
                                }]});
                            format!("data: {frame}\n\n")
                        }).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    ([("content-type","text/event-stream")],
                        format!("{preamble}data: {frame}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("回环端口");
        let endpoint = format!("http://{}", listener.local_addr().expect("回环地址"));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("模型替身启动");
        });
        let llm = cortex_llm::LlmClient::from_config(
            &cortex_core::config::LlmConfig {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                cheap_model: "gpt-4o".into(),
                base_url: Some(endpoint),
            },
            "local-test-placeholder",
        )
        .expect("本机替身不需要真实凭据");
        let turn = crate::turn_assemble::chat_turn_for(
            3,
            &[],
            crate::turn_assemble::Caps {
                can_open_application: true,
                ..Default::default()
            },
            &[],
        );
        let host = Host(AtomicUsize::new(0));
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let mut messages = vec![cortex_llm::Message::user().with_text("打开 Chrome")];
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            turn.run(&llm, "测试沟通顺序", &mut messages, &host, &tx),
        )
        .await;
        server.abort();
        let outcome = outcome.expect("流不应卡住").expect("真实循环成功完成");
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        let streamed: String = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Delta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(streamed, outcome.reply, "屏幕上的正文与归档回复必须一致");
        assert_eq!(
            host.0.load(Ordering::SeqCst),
            tool_rounds,
            "补开场不能重试或增加工具调用"
        );
        assert_eq!(
            rounds.load(Ordering::SeqCst),
            tool_rounds + 1,
            "补开场不能增加模型调用"
        );
        if tool_rounds == 0 {
            assert_eq!(outcome.reply, "处理结束。", "纯聊天不能被塞入工具开场");
        } else {
            let expected = opening.unwrap_or("我先尝试打开应用，再查看系统返回的结果。\n\n");
            assert!(
                matches!(&events[0], AgentEvent::Delta(text) if text == expected),
                "第一条用户可见内容必须是开场，而不是工具"
            );
            assert!(
                matches!(&events[1], AgentEvent::ToolCall { .. }),
                "开场之后才开始工具调用"
            );
            assert_eq!(
                outcome.reply,
                format!("{expected}处理结束。"),
                "只补缺失的开场，不能挤掉模型原话、重复补或丢掉最终回复"
            );
            assert!(outcome.reply.starts_with(expected), "归档回复不能丢掉开场");
        }
    }
}
