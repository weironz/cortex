// Copyright (c) 2025 Cua AI, Inc. Licensed under MIT.
// Adapted from cua-driver-core/src/daemon.rs at
// 85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19; see NOTICE and third_party/cua/LICENSE.
//! 沿用官方逐行 JSON framing；在实际 RPC 句柄上检查原 Job，检查后不重连。

use crate::computer_owned_job::OwnedJob;
use serde_json::{Value, json};
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

const MAX_REPLY: u64 = 24 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct RpcFailure {
    pub payload: Value,
    pub message: String,
    pub exit_code: i32,
}
impl std::fmt::Display for RpcFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("owned daemon 返回结构化失败")
    }
}
impl std::error::Error for RpcFailure {}

pub(super) fn verify_peer(pipe: BorrowedHandle<'_>, job: &OwnedJob) -> io::Result<()> {
    let mut pid = 0;
    // SAFETY: 句柄来自本次已连接的 named-pipe client，且在整个请求期间持有。
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    if !job.contains_process(process.as_handle())? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "实际 RPC peer 不属于原 Job",
        ));
    }
    Ok(())
}

pub(super) async fn send<G: Send>(
    socket: &str,
    job: &OwnedJob,
    request: Value,
    before_write: impl FnOnce() -> io::Result<G> + Send,
) -> io::Result<Value> {
    let connect_deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut pipe = loop {
        match ClientOptions::new().open(socket) {
            Ok(pipe) => break pipe,
            Err(error)
                if matches!(error.raw_os_error(), Some(2 | 231))
                    && tokio::time::Instant::now() < connect_deadline =>
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    };
    verify_peer(pipe.as_handle(), job)?;
    let mut bytes = serde_json::to_vec(&request).map_err(io::Error::other)?;
    if bytes.len() > 256 * 1024 {
        return Err(io::Error::other("RPC 参数超过预算"));
    }
    bytes.push(b'\n');
    let send_guard = before_write()?;
    pipe.write_all(&bytes).await?;
    pipe.flush().await?;
    drop(send_guard);
    let mut response = Vec::new();
    BufReader::new(pipe.take(MAX_REPLY + 1))
        .read_until(b'\n', &mut response)
        .await?;
    if response.len() as u64 > MAX_REPLY || response.last() != Some(&b'\n') {
        return Err(io::Error::other("RPC 响应超限或缺少完整帧"));
    }
    let response: Value = serde_json::from_slice(&response).map_err(io::Error::other)?;
    if response["ok"] != true {
        return Err(io::Error::other(RpcFailure {
            payload: response.get("result").cloned().unwrap_or(Value::Null),
            message: response["error"]
                .as_str()
                .unwrap_or("owned daemon 拒绝 RPC")
                .to_owned(),
            exit_code: response["exit_code"]
                .as_i64()
                .and_then(|code| i32::try_from(code).ok())
                .unwrap_or(1),
        }));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| io::Error::other("RPC 缺少 result"))
}

pub(super) fn call_request(name: &str, mut args: Value) -> Value {
    if let Some(object) = args.as_object_mut() {
        object.retain(|key, _| !key.starts_with('_'));
    }
    json!({"method":"call","name":name,"args":args,
        "session_id":"cortex-agent-owned","observation_origin":"direct","client_kind":"unknown"})
}

/// 与官方 CLI 相同的 structuredContent + image 映射，AX-only 仍由既有输出过滤器约束。
pub(super) fn tool_result(result: Value) -> io::Result<Value> {
    if result["isError"] == true {
        return Err(io::Error::other(RpcFailure {
            payload: result
                .get("structuredContent")
                .cloned()
                .unwrap_or_else(|| result.clone()),
            message: "owned daemon 工具返回失败".into(),
            exit_code: 1,
        }));
    }
    let mut structured = if let Some(value) = result.get("structuredContent") {
        value.clone()
    } else {
        let text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::from_str(&text).map_err(io::Error::other)?
    };
    for image in result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "image")
    {
        if let (Some(data), Some(object)) = (image["data"].as_str(), structured.as_object_mut()) {
            object.insert("screenshot_png_b64".into(), Value::String(data.into()));
            object.insert(
                "screenshot_mime_type".into(),
                image.get("mimeType").cloned().unwrap_or(json!("image/png")),
            );
        }
    }
    Ok(structured)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn foreign_actual_connection_receives_no_request_bytes() {
        let socket = format!(r"\\.\pipe\cortex-rpc-foreign-{}", cortex_core::Id::new());
        let mut server = tokio::net::windows::named_pipe::ServerOptions::new()
            .create(&socket)
            .expect("自有测试pipe");
        let job = OwnedJob::new().expect("独立空 Job");
        let call = send(
            &socket,
            &job,
            json!({"method":"call"}),
            || -> io::Result<()> { panic!("外部peer不能到写边界") },
        );
        let observer = async {
            server.connect().await.expect("观察实际连接");
            let mut bytes = [0; 128];
            match server.read(&mut bytes).await {
                Ok(0) | Err(_) => {}
                Ok(size) => panic!("未授权peer收到{size}字节"),
            }
        };
        let (result, ()) = tokio::join!(call, observer);
        assert_eq!(
            result.expect_err("必须拒绝实际外部peer").kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    #[test]
    fn framing_keeps_session_and_drops_only_reserved_args() {
        let request = call_request(
            "get_window_state",
            json!({"session":"run-a","_session_id":"injected","window_id":"w"}),
        );
        assert_eq!(request["args"]["session"], "run-a");
        assert!(request["args"].get("_session_id").is_none());
        assert_eq!(
            tool_result(json!({"structuredContent":{"active":true},"content":[]}))
                .expect("结构化回执"),
            json!({"active":true})
        );
    }
}
