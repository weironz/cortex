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
/// # Windows：直接问内核，**不许起子进程**
///
/// `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` 拿得到句柄就算活着。
/// 不用 `GetExitCodeProcess`：那个对已退出但句柄仍被持有的进程返回
/// `STILL_ACTIVE`(259)，与「退出码恰好是 259」无法区分。
///
/// ## 这里从前跑的是 `tasklist`，那是个真实的生产 bug
///
/// 原来的实现每 2 秒 `Command::new("tasklist").output()`，理由写着「为一个
/// 每 2 秒一次的探测引入 windows-sys 不划算，进程创建的开销完全无感」。
/// 开销确实无感，**但它有一个没被算进去的失败模式**：
///
/// 2026-08-30 在生产上撞到 —— 其中一次 `tasklist` 的 `conhost.exe` 卡住，
/// 而 `.output()` 是**没有超时的阻塞调用**，于是整个循环永久停在那一行，
/// 再也不去看第二眼。表现：用户退出桌面端 12 分钟之后，`cortex-local`
/// 仍然 `LISTENING`、仍然与生产保持着 TLS 连接、仍然握着用户的 token，
/// 而 `/health` 照答 200（runtime 是好的，只有看门狗那条腿断了）。
///
/// 判据本身一直是对的（对一个已死的 pid，tasklist 确实报「无匹配」）——
/// **坏的是取得判据的方式**：一次外部进程调用，没有超时，也没有第二次机会。
/// 一个每 2 秒重复一次的检查，只要有一次能永久卡住，它就等于不存在。
///
/// 换成一次系统调用之后，这条路上不再有进程、没有控制台、没有可卡的东西。
#[cfg(windows)]
fn is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
    };

    /// 标准访问权之一，`WaitForSingleObject` 要它。
    ///
    /// 写字面量而不是从 windows-sys 里 use：那个常量被归在
    /// `Win32_Storage_FileSystem` 这个 feature 下（它在那儿的类型是
    /// `FILE_ACCESS_RIGHTS`），为一个数字把整个文件系统模块拉进来不值。
    /// 这个值三十年没变过，且 winnt.h 里就是这么写的。
    const SYNCHRONIZE: u32 = 0x0010_0000;

    // SAFETY: 只传一个 pid 进去，拿回来的句柄要么为空要么有效。
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        // 打不开有两种：**没有这个 pid**（真的死了），或者有但我们无权查看
        // （换个用户跑的 GUI）。后者判成死的会让本地 agent 在监护人好好跑着
        // 的时候自杀 —— 那是个没有任何提示的功能消失。
        //
        // SAFETY: 紧跟在失败的那次调用之后读线程最后的错误码。
        let err = unsafe { GetLastError() };
        return err != ERROR_INVALID_PARAMETER;
    }

    // ★ **拿到句柄不等于它还活着，所以必须再问一次。**
    //
    // 一个已经退出、但句柄仍被别人持有的进程，PID 条目会一直留着，
    // `OpenProcess` 对它**照样成功** —— 第一版就是这么写的，实测里
    // 「杀掉父进程 8 秒后 agent 还在」，因为测试脚本自己握着那个句柄。
    // 这与文档里为 `GetExitCodeProcess` 记下的是同一个坑，只是换了个门进来。
    //
    // `WaitForSingleObject(…, 0)` 没有这个歧义：进程对象在**退出那一刻**
    // 被激发，与谁还握着句柄无关。超时（还没激发）才是「活着」。
    //
    // SAFETY: 上面刚拿到的有效句柄；0 毫秒超时，不阻塞。
    let signaled = unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0;
    // SAFETY: 同上，用完即关，不外泄。
    unsafe { CloseHandle(handle) };
    !signaled
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    // `pid_t` 是 i32。**不能直接 `as`**：一个装不下的 u32 会绕成负数，
    // 而 `kill` 对非正数有完全不同的含义 ——
    //   `kill(-1, …)` = 「我有权发信号的**所有**进程」
    //   `kill(0,  …)` = 「我自己的进程组」
    //   `kill(-n, …)` = 「进程组 n」
    // 三种都会成功返回，于是任何越界的 pid 都被判成「活着」，
    // 而那意味着孤儿永远不会被回收。CI 上是 `u32::MAX` 撞出来的。
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }

    // 信号 0：不真的发，只做「进程存在且我有权给它发信号」的检查
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // EPERM = **进程存在**，只是我没权限给它发信号（换了用户跑的 GUI）。
    // 判成死的会让本地 agent 在监护人好好跑着的时候自杀，
    // 而用户看到的是「聊天突然连不上了」，没有任何别的线索
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(any(windows, unix)))]
fn is_alive(_pid: u32) -> bool {
    true
}

/// 清掉状态目录里**主人已死**的存活指针。
///
/// # 为什么需要它 —— 「靠探活不靠删文件」只对了一半
///
/// `live_file` 的文档说陈旧靠读的一方探活兜底，那保证了**正确性**；
/// 但没人删文件，指针就只增不减 —— 实测一台开发机上攒到 265 个，
/// 每个发现方每次都要把它们全部扫一遍。这里补上**回收**：启动时凭
/// 文件名里的 pid 查一次进程表，主人不在了就删。
///
/// # 为什么按 pid 判而不是逐个探活
///
/// 探活一个死指针要等满 800ms 超时，265 个就是三分半；进程表按平台
/// 各取最便宜的问法（Windows 一次 tasklist 快照，unix 逐个 kill(0)，
/// 后者不起进程、纳秒级）。pid 复用导致的误留（新进程碰巧顶了旧 pid）
/// 由读方的探活兜底 —— 两层各管一半，谁也不用做到完美。
///
/// 任何一步失败都只记日志或按「活着」处理：回收是卫生工作，
/// 误删一个活指针的代价（发现不了 agent）远大于多留几个死文件。
pub fn reap_stale_live_files(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // 先收集再判活：Windows 的进程表快照只该拍一次
    let mut candidates: Vec<(std::path::PathBuf, Option<u32>)> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some(cortex_core::LIVE_FILE_EXT) {
            continue;
        }
        // 文件名形如 agent-{pid}.live；解析不出 pid 的按坏文件处理，一并清掉
        let pid = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("agent-"))
            .and_then(|s| s.parse::<u32>().ok());
        candidates.push((path, pid));
    }
    if candidates.is_empty() {
        return;
    }

    #[cfg(windows)]
    let snapshot = alive_pid_snapshot();
    let me = std::process::id();
    let mut removed = 0u32;
    for (path, pid) in candidates {
        let dead = match pid {
            Some(p) if p == me => false,
            #[cfg(windows)]
            Some(p) => match &snapshot {
                Some(set) => !set.contains(&p),
                // 快照拍不下来就一个都不删 —— 与 is_alive 的失败方向一致
                None => false,
            },
            #[cfg(not(windows))]
            Some(p) => !is_alive(p),
            None => true,
        };
        if dead && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(removed, "清掉了主人已退出的存活指针");
    }
}

/// 一次拍下**当前所有 cortex-local 进程**的 pid。拍不下来回 `None`，
/// 调用方按「都活着」处理。
///
/// # 为什么要按进程名过滤，只看 pid 在不在是不够的
///
/// Windows 回收 pid 很勤：实测 265 个死指针里有 245 个的 pid 已经被
/// **无关进程**顶上 —— 只判「pid 存在」的收割器对它们永远无能为力，
/// 目录照样只增不减。指针文件的主人只可能是 cortex-local，pid 对应的
/// 映像不是它，指针就必然是陈旧的（pid 复用给了别人）。
#[cfg(windows)]
fn alive_pid_snapshot() -> Option<std::collections::HashSet<u32>> {
    use std::process::Command;
    let out = Command::new("tasklist")
        .args(["/NH", "/FO", "CSV"])
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            tracing::warn!(status = %o.status, "tasklist 失败，这次不回收存活指针");
            return None;
        }
        Err(e) => {
            tracing::warn!(error = %e, "起不了 tasklist，这次不回收存活指针");
            return None;
        }
    };
    // CSV 每行形如 "cortex-local.exe","1234","Console",… —— 名在第一列、
    // pid 在第二列。行数为 0（tasklist 本地化输出变了形）时回 None：
    // 拿空集合去删会把所有指针清光，包括活的
    let text = String::from_utf8_lossy(&out.stdout);
    if text.lines().filter(|l| l.contains("\",\"")).count() == 0 {
        tracing::warn!("tasklist 输出不是预期的 CSV，这次不回收存活指针");
        return None;
    }
    let set = text
        .lines()
        .filter_map(|l| {
            let mut cols = l.trim_start_matches('"').split("\",\"");
            let name = cols.next()?;
            if !name.to_ascii_lowercase().starts_with("cortex-local") {
                return None;
            }
            cols.next()?.parse::<u32>().ok()
        })
        .collect::<std::collections::HashSet<_>>();
    Some(set)
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

    /// **已退出、但句柄还被别人握着的进程，不算活着。**
    ///
    /// 这一条钉住的是 2026-08-30 修这个 bug 时**当场栽过的那一版**：
    /// 头一个修法只调 `OpenProcess`，而一个已退出的进程只要还有人握着它的
    /// 句柄，PID 条目就一直留着，`OpenProcess` 对它照样成功 —— 于是「父进程
    /// 死了」永远判不出来，孤儿照活不误。真机验证里它一次都没退。
    ///
    /// 这与文档里为 `GetExitCodeProcess` 记下的是**同一个坑**，只是换了扇门
    /// 进来。所以这条测试不测 API，测的是那个状态本身：`Child` 在作用域里
    /// 一直握着句柄，而进程已经 `wait` 过了。
    #[cfg(windows)]
    #[test]
    fn 已退出但句柄还被握着的进程不算活着() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .expect("起得来一个立刻退出的进程");
        let pid = child.id();
        child.wait().expect("等得到它退出");
        // ⚠️ `child` **故意留在作用域里**：Rust 的 `Child` 直到被 drop 才关掉
        // 句柄，而句柄没关正是要复现的那个状态。
        assert!(
            !is_alive(pid),
            "把一个已经退出的进程判成了活着 —— 只要还有人握着它的句柄，             PID 条目就在，光靠 OpenProcess 是分不出来的。             这会让本地 agent 在桌面端退出之后继续监听端口、继续握着 token"
        );
        drop(child);
    }

    /// 不存在的 pid 必须判成死的。
    ///
    /// 只判「活着」是不够的：一个恒返回 true 的实现能通过上一条，
    /// 而它意味着孤儿永远不会被回收。
    ///
    /// `u32::MAX` 这个取值是**特意**留着的：它在 unix 上 `as pid_t` 会绕成
    /// `-1`，而 `kill(-1, 0)` 的语义是「我能发信号的所有进程」并且**成功返回**。
    /// CI 上就是它把那个 bug 撞出来的（见 [`is_alive`] 的注释）。
    #[test]
    fn an_absent_pid_is_detected_as_dead() {
        for pid in [u32::MAX, u32::MAX - 1, 0x8000_0000] {
            assert!(
                !is_alive(pid),
                "pid {pid} 被判成活着 —— 那样父进程死了也不会有人收掉这个\
                 握着 token 的进程。unix 上要当心 `as pid_t` 绕成负数"
            );
        }
        // 一个大到不可能、但**没有**绕成负数的 pid。
        // 上面几个是溢出路径，这个走的是「真的查了一遍，没有」
        assert!(!is_alive(4_194_303), "Linux 默认 pid 上限附近，不该有进程");
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
