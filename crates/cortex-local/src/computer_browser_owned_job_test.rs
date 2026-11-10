//! 默认仅自有测试进程 + 假 CDP；真实 headless 空页验收必须显式运行 ignored。
use super::*;
use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

#[tokio::test]
#[ignore = "真实 Chrome/Edge 的独立 headless about:blank 启动/观察/Job关闭；不导航或输入"]
async fn real_owned_browser_blank_observe_and_job_close_smoke() {
    assert_eq!(
        std::env::var("CORTEX_OWNED_BROWSER_SMOKE").as_deref(),
        Ok("owned-headless-about-blank"),
        "仅显式授权的独立空页验收"
    );
    let driver = BrowserDriver::new(BrowserConfig {
        executable: browser_executable_from_env().expect("本机已签名 Chrome/Edge"),
        allowlist: Arc::new(SiteAllowlist::parse("example.com").expect("固定测试 allowlist")),
        headless: true,
    });
    let window = tokio::time::timeout(Duration::from_secs(30), driver.launch())
        .await
        .expect("真实浏览器启动必须有上界")
        .expect("严格 Job 内启动 Chromium");
    driver
        .observe(window.id())
        .await
        .expect("真实结构化观察应成功");
    let profile = {
        let mut slot = driver.session.lock().await;
        let session = slot.as_mut().expect("自有 session 必须存在");
        let location = session
            .client
            .call(
                "Runtime.evaluate",
                json!({
                    "expression":"location.href","returnByValue":true
                }),
            )
            .await
            .expect("只读检查空页地址");
        assert_eq!(
            location["result"]["value"], "about:blank",
            "不能访问外部站点或用户页面"
        );
        session
            ._profile
            .as_ref()
            .expect("独立临时 profile")
            .path()
            .to_path_buf()
    };
    assert_eq!(driver.dispatch_epoch(), 0, "观察不能发送输入或导航");
    tokio::time::timeout(Duration::from_secs(10), driver.close())
        .await
        .expect("关闭有上界")
        .expect("必须取得原 Job 整树排空证明");
    driver.close().await.expect("重复关闭复用已确认的结果");
    assert!(!profile.exists(), "已证明关闭后应清理独立 profile");
}

async fn fixture(
    block_close: bool,
) -> (
    Arc<BrowserDriver>,
    tempfile::TempDir,
    OwnedHandle,
    Arc<tokio::sync::Notify>,
    JoinHandle<()>,
    PathBuf,
) {
    let root = tempfile::tempdir().expect("自有测试根目录");
    let profile = tempfile::tempdir_in(root.path()).expect("自有 profile");
    let profile_path = profile.path().to_path_buf();
    let allowlist = Arc::new(SiteAllowlist::parse("example.com").expect("测试 allowlist"));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("假 CDP 监听");
    let address = listener.local_addr().expect("假 CDP 地址");
    let entered = Arc::new(tokio::sync::Notify::new());
    let server = tokio::spawn({
        let entered = entered.clone();
        async move {
            let (stream, _) = listener.accept().await.expect("假 CDP 接入");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("假 CDP 握手");
            if let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).expect("关闭 CDP JSON");
                assert_eq!(
                    request["method"], "Browser.close",
                    "只能接收到自有 session 的关闭命令"
                );
                entered.notify_one();
                if block_close {
                    std::future::pending::<()>().await;
                }
                socket
                    .send(Message::Text(
                        json!({"id":request["id"],"result":{}}).to_string().into(),
                    ))
                    .await
                    .expect("假 CDP 返回关闭回执");
            }
        }
    });
    let job = Arc::new(crate::computer_owned_job::OwnedJob::new().expect("严格原 Job"));
    let mut command = Command::new(std::env::current_exe().expect("自有测试二进制"));
    command
        .args([
            "--exact",
            "computer_owned_job::tests::owned_job_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("CORTEX_OWNED_JOB_TEST_ROOT", &profile_path)
        .env("CORTEX_OWNED_JOB_TEST_ROLE", "root")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = job
        .spawn_contained(&mut command)
        .expect("挂起后纳入 Job 再执行测试树");
    let handle = child
        .as_handle()
        .try_clone_to_owned()
        .expect("保留精确原进程句柄");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while !profile_path.join("root.ready").exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "测试子孙树必须实际就绪"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let driver = Arc::new(BrowserDriver::new(BrowserConfig {
        executable: ValidatedBrowserExecutable::test_fixture(),
        allowlist: allowlist.clone(),
        headless: true,
    }));
    let app = AppIdentity::new(
        AppIdentityKind::WindowsExecutable,
        "owned-test-browser",
        "Owned Test Browser",
    )
    .expect("测试 app");
    let window = WindowRef::new(
        WindowId::new("owned-test-window").expect("测试 window"),
        app.clone(),
    );
    let client = CdpClient::connect(format!("ws://{address}/owned-test"), allowlist.clone())
        .await
        .expect("只连接假 CDP");
    let proxy = BrowserProxyHandle::start(allowlist.clone())
        .await
        .expect("自有临时代理");
    *driver.close_state.lock().expect("关闭状态") = BrowserCloseState::NeedsProof;
    *driver.session.lock().await = Some(BrowserSession {
        child,
        job,
        shutdown_proven: false,
        _profile: Some(profile),
        proxy,
        client,
        app,
        window,
        context_id: None,
        elements: BTreeMap::new(),
        frame_sequence: 0,
        nonce: "owned-test".into(),
        allowlist,
    });
    (driver, root, handle, entered, server, profile_path)
}

#[tokio::test]
async fn owned_browser_close_drains_actual_tree_and_is_idempotent() {
    let (driver, _root, handle, _entered, server, profile) = fixture(false).await;
    driver
        .close()
        .await
        .expect("真实 BrowserSession 必须取得原 Job 排空证明");
    driver
        .close()
        .await
        .expect("只复用已有成功结论，不重新签发证明");
    // SAFETY: 只等待本测试刚创建的精确原进程句柄。
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
        WAIT_OBJECT_0,
        "原进程必须退出"
    );
    assert!(!profile.exists(), "只有可靠关闭后才清理 profile");
    assert!(driver.session.lock().await.is_none(), "关闭消费原 session");
    assert!(driver.launch().await.is_err(), "关闭后不能重新创建旧运行时");
    server.await.expect("假 CDP 任务完成");
}

#[tokio::test]
async fn cancelled_owned_browser_close_never_turns_empty_slot_into_success() {
    let (driver, _root, handle, entered, server, profile) = fixture(true).await;
    let closing = tokio::spawn({
        let driver = driver.clone();
        async move { driver.close().await }
    });
    entered.notified().await;
    closing.abort();
    let _ = closing.await;
    let error = driver
        .close()
        .await
        .expect_err("取消只触发 Drop 清理，不产生排空证明");
    assert!(
        matches!(error, DriverError::OutcomeUnknown(_)),
        "必须传播为已有 Unknown 隔离语义"
    );
    assert!(
        driver.close().await.is_err(),
        "第二次关闭也不能把丢失的证明变成成功"
    );
    assert!(
        profile.exists(),
        "未取得证明时保留 profile，不在进程树仍可能运行时删除"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Drop 必须尽力清理本测试的 Job 树"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    server.abort();
}

#[tokio::test]
async fn never_started_browser_close_does_not_need_a_fabricated_job_proof() {
    let driver = BrowserDriver::new(BrowserConfig {
        executable: ValidatedBrowserExecutable::test_fixture(),
        allowlist: Arc::new(SiteAllowlist::parse("example.com").expect("测试 allowlist")),
        headless: true,
    });
    driver.close().await.expect("从未启动时无需编造 Job 证明");
}
