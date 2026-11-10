//! 仅子进程启用全局 test hook，避免影响并行运行的其他测试。
use super::*;

#[test]
#[ignore = "仅父测试启动的独立 cfg(test) 子进程；无 GUI/真实 driver"]
fn scope_worker() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let subject = super::super::host::fixture_namespace();
            let fixture = http_fixture::provision(subject.clone()).await.unwrap();
            let again = http_fixture::provision(subject).await.unwrap();
            assert_eq!(fixture.path(), again.path(), "重复 hook 不允许换根清空历史");
            let other =
                PrincipalNamespace::from_verified("another-deployment", "another-user").unwrap();
            assert!(
                http_fixture::provision(other.clone()).await.is_err(),
                "不允许改绑其他主体"
            );
            assert!(
                http_fixture::dispatch_source(&other).is_none(),
                "其他主体不能借用 fixture 根"
            );
            let root = fixture.path().to_owned();
            // health 的只读探针有 80ms 预算；线程调度慢时允许暂时不可用，不能将它
            // 当成 hook 丢失。只重试这一状态，不吞掉缺权威、fence 或其他真实拒绝。
            let observed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    let observed = tokio::spawn(readiness()).await.unwrap();
                    if observed != Readiness::ProbeUnavailable {
                        break observed;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("新任务的只读探针必须在有界等待后完成");
            assert_eq!(
                observed,
                Readiness::Ready,
                "新任务可见 hook，不依赖 task-local 传播根"
            );
            tokio::spawn(super::super::host::fixture_turn(root.clone()))
                .await
                .unwrap();
            assert_eq!(
                sql(&root, vec![]).await,
                ["verified"],
                "真实 Turn/确认簿后的默认派发须持久化"
            );
            use axum::{
                body::{Body, to_bytes},
                http::Request as HttpRequest,
            };
            use tower::ServiceExt;
            let state = crate::local_workspace_routes_test::state(
                root.parent().unwrap(),
                "http://127.0.0.1:9",
                None,
            );
            let task = tokio::spawn(crate::routes::TEST_COMPUTER_SURFACE.scope(
                (true, true, true),
                async move {
                    let router = crate::routes::router(state);
                    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
                    loop {
                        let response = router
                            .clone()
                            .oneshot(HttpRequest::get("/health").body(Body::empty()).unwrap())
                            .await
                            .unwrap();
                        assert_eq!(response.status(), 200, "真实 health 路由必须可用");
                        let body: Value = serde_json::from_slice(
                            &to_bytes(response.into_body(), 65536).await.unwrap(),
                        )
                        .unwrap();
                        if body["computer_capabilities"]["write_readiness"] != "probe_unavailable"
                            || tokio::time::Instant::now() >= deadline
                        {
                            break body;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                },
            ));
            let body = task.await.unwrap();
            assert_eq!(
                body["computer_capabilities"]["write_readiness"], "ready",
                "Axum 任务可见同一临时权威"
            );
        });
}
#[tokio::test]
async fn process_scoped_hook_crosses_http_and_turn_tasks_without_environment_override() {
    use std::os::windows::process::CommandExt;
    let status = tokio::task::spawn_blocking(|| {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "computer_ledger::tests::provisioning::http_hook::scope_worker",
                "--nocapture",
            ])
            .creation_flags(0x08000000)
            .status()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(status.success(), "必须实际验证子进程 hook，不只做编译");
}
