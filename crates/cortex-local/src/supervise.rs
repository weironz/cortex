//! 跟着父进程一起死。
//!
//! # 为什么必须有
//!
//! 桌面端把本地 agent 作为子进程拉起来。GUI 正常退出时会收掉它，但
//! **GUI 崩溃、被任务管理器结束、或者在 Windows 上被直接关掉窗口时不会** ——
//! 那些路径上根本没有代码跑得到。
//!
//! 留下的孤儿不是「多一个闲置进程」那么轻：它绑着 loopback 上的一个端口、
//! 握着用户的 token、并且**能执行命令**。用户以为自己已经退出了。
//! 下次开 GUI 还会因为端口被占而拉不起新的。
//!
//! # 为什么是轮询而不是别的
//!
//! Windows 有 Job Object（父进程一死内核连坐杀掉整棵树），那是最干净的做法，
//! 但要从 **Dart 那一侧**建 Job 并把子进程加进去 —— Dart 的 `Process.start`
//! 不暴露这个。所以由被监护的一方来看着监护人。
//!
//! 轮询间隔取 2 秒：孤儿多活两秒没有代价，而更密的轮询在一个常驻进程上
//! 是纯粹的浪费。

use std::time::Duration;

/// 多久看一眼父进程还在不在。
const POLL: Duration = Duration::from_secs(2);

/// 父进程一消失就结束本进程。
///
/// `pid` 为 `None`（没人传 `--parent-pid`，比如手工从命令行跑）时直接返回，
/// 不起任务 —— 那种场景下本进程就是最外层，没有监护人可看。
pub fn exit_with_parent(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    tracing::info!(parent_pid = pid, "将跟随父进程退出");
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(POLL).await;
            if !is_alive(pid) {
                tracing::info!(parent_pid = pid, "父进程已退出，本地 agent 一并收工");
                // 直接退进程而不是优雅关闭 HTTP 服务：监护人都没了，
                // 已经没有客户端在等任何一条响应，而多一条优雅路径
                // 就多一个「卡在那里没退掉」的可能
                std::process::exit(0);
            }
        }
    });
}

/// 这个 pid 还活着吗。
///
/// # Windows
///
/// `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` 拿得到句柄就算活着。
/// 不用 `GetExitCodeProcess`：那个对已退出但句柄仍被持有的进程返回
/// `STILL_ACTIVE`(259) 与「退出码恰好是 259」无法区分 —— 一个真实存在的坑。
///
/// 这里靠 `tasklist` 而不是直接调 Win32：为一个每 2 秒一次的探测引入
/// `windows-sys` 依赖（连同它在别的平台上的 cfg 分叉）不划算。
/// 进程创建的开销在 2 秒周期上完全无感。
#[cfg(windows)]
fn is_alive(pid: u32) -> bool {
    use std::process::Command;
    // /NH 去表头、/FI 过滤。命中时输出里有那个 pid；不命中时
    // tasklist 打的是「没有运行的任务匹配指定标准」——两者都不是错误码，
    // 所以判断只能看 stdout 里有没有那个数字
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()),
        // 查不了就当它活着。**方向是刻意的**：误判为「死了」会让本地 agent
        // 在监护人好好的时候自杀，而那是个没有任何提示的功能消失
        Err(e) => {
            tracing::warn!(error = %e, "查不到父进程状态，按仍在运行处理");
            true
        }
    }
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    // kill(pid, 0)：不发信号，只做「进程存在且我有权给它发信号」的检查
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(any(windows, unix)))]
fn is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自己一定是活的 —— 这条守的是探测本身没写反。
    ///
    /// 写反的后果很具体：本地 agent 在 GUI 好好跑着的时候自己退掉，
    /// 而用户看到的是「聊天突然连不上了」，没有任何别的线索。
    #[test]
    fn the_current_process_is_detected_as_alive() {
        assert!(
            is_alive(std::process::id()),
            "探测把自己判成了死的 —— 那会让本地 agent 在监护人正常时自杀"
        );
    }

    /// 一个几乎不可能存在的 pid 必须判成死的。
    ///
    /// 只判「活着」是不够的：一个恒返回 true 的实现能通过上一条，
    /// 而它意味着孤儿永远不会被回收。
    #[test]
    fn an_absent_pid_is_detected_as_dead() {
        // Windows 的 pid 是 DWORD，Linux 默认上限 4194304。
        // u32::MAX 在两边都不会是一个活着的进程
        assert!(
            !is_alive(u32::MAX),
            "探测恒真 —— 那样父进程死了也不会有人收掉这个握着 token 的进程"
        );
    }

    /// 没传 pid 时不该起任何任务，也不该 panic。
    #[tokio::test]
    async fn no_parent_pid_means_no_watchdog() {
        exit_with_parent(None);
        // 走到这里就说明没 panic、没退进程。给一点时间让「万一真起了任务」
        // 的那种实现有机会出错
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
