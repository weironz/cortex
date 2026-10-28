use super::*;

/// `shell` 的描述**随本机的沙箱成色变**。
///
/// 仓库的硬约束之一是「提示词与工具目录只能写当下真的成立的能力」。
/// 它的反面同样成立：一个**写着能用、实际不能用**的工具会让模型一次次
/// 撞同一堵墙 —— 它看到的只是一句「拒绝访问」，于是把它当成仓库或命令
/// 有问题，换十种写法重试，一轮就这么烧完。
///
/// Windows 的 AppContainer 沙箱里 `git`（任何子命令）与 `dir` / `vol`
/// 跑不起来，根因见 [`crate::sandbox::windows`] 的模块文档。这不是我们
/// 选的，但**是模型必须知道的**。
///
/// 判据带上 `capability().is_available()`：降级或有人在场时命令跑在宿主上，
/// git 好好的 —— 那时候写那句话就成了另一种「说得不对」。
pub(super) fn shell_description() -> std::borrow::Cow<'static, str> {
    shell_description_for(crate::sandbox::capability())
}

/// 描述是**能力的纯函数**。
///
/// 拆出来是因为原来那版直接读 `capability()`，于是测试只能验「本机此刻这一
/// 档」。而 `cargo test --lib` 里 helper 二进制多半不存在，探测报不可用 ——
/// 测试就一路落到「无沙箱」那一支，两个 Windows 后端的断言**一条都没跑过**，
/// 而且不会红。故障注入当场证实了这一点（把描述里的关键句删掉，测试照样绿）。
///
/// 判据不该由环境决定：把能力做成参数，三支就都能在同一次运行里验到。
fn shell_description_for(cap: &crate::sandbox::Capability) -> std::borrow::Cow<'static, str> {
    // 用 `concat!` 逐行拼，不用 `\` 续行 —— 续行会把缩进留成字面空格，
    // 有一条测试专门盯着这个（`工具描述里没有被压扁的痕迹`）
    const BASE: &str = concat!(
        "在工作区内执行一条 shell 命令。命令运行在 OS 级沙箱里：",
        "文件访问受工作区与构建缓存规则约束，网络策略由执行环境决定",
    );
    // AppContainer 那一档：读默认拒绝，但 dir/vol 用不了，主目录下的工作区
    // git 也用不了。这些是模型必须知道的，否则它会换十种写法反复撞。
    const APPCONTAINER: &str = concat!(
        "在工作区内执行一条 shell 命令。命令运行在 OS 级沙箱里：",
        "文件访问受工作区与构建缓存规则约束，网络策略由执行环境决定。",
        "⚠ 本机的沙箱是 Windows AppContainer，其中 **`dir` 与 `vol` 用不了**",
        "（会报「拒绝访问」）—— 那是沙箱的限制，不是命令写错了，换写法重试",
        "没有用。要看目录内容请用 `list_dir` 工具，或 cmd 的 `for %f in (*)`。",
        "另外，工作区如果在用户主目录底下（`C:/Users/…`），`git` 也用不了。",
        "`cargo build`（含依赖、含 build script）**能出可运行的 .exe** —— 沙箱替你",
        "换了链接器（MSVC link.exe 在这一档起不来）。但 `cc` crate 那类要调 `cl.exe`",
        "的 C/C++ 构建步骤仍然跑不了（同一个 link.exe 族），要编那种依赖换受限令牌档。",
    );
    // 受限令牌那一档：工具链全可用，但**不挡读**。后者同样要说 ——
    // 模型据此判断「这里能不能放心处理敏感内容」，说反了比不说更糟。
    const RESTRICTED: &str = concat!(
        "在工作区内执行一条 shell 命令。命令运行在 OS 级沙箱里。",
        "本机这一档是 Windows 受限令牌：**写只落在工作区**（写工作区以外会被拒），",
        "工具链可用 —— `cargo`（含 `add` / `build` 与拉取新依赖）、`dir`、",
        "`git`（含 `clone` / `fetch` / `push`）都正常。",
        "⚠ cargo 在这一档里用的是**沙箱专用的 `CARGO_HOME`**，不是用户的 `~/.cargo`：",
        "第一次构建会重新下载依赖，之后各工作区共用这份缓存。不要为此去改 `CARGO_HOME`。",
        "`curl` 也可用（HTTPS 正常，沙箱备了自己的一份）。",
        "⚠ PowerShell 的 `Invoke-WebRequest` / .NET 的 HTTPS 走不了 —— 用 `curl` 或 `git` 代替。",
        "⚠ 这一档**不限制读取**：它读得到当前用户能读的任何文件；明文 HTTP 也出得去。",
        "所以不要把它当成「关住不受信代码」的边界；处理敏感文件时照常谨慎。",
    );
    if !cfg!(windows) || !cap.is_available() {
        return BASE.into();
    }
    match cap {
        crate::sandbox::Capability::Available { backend, .. }
            if backend.as_str() == "restricted-token" =>
        {
            RESTRICTED.into()
        }
        _ => APPCONTAINER.into(),
    }
}

/// shell 命令的默认超时。
///
/// 必须有一个：一条 `sleep 1d` 或者一个等着读 stdin 的交互式命令会把整轮
/// 对话永远挂住，而用户那边看到的只是「没有反应」——最难排查的一种失败。
pub(super) const DEFAULT_SHELL_TIMEOUT_MS: u64 = 120_000;
/// 上限。模型给的参数不可信，不封顶等于没有超时。
pub(super) const MAX_SHELL_TIMEOUT_MS: u64 = 600_000;

/// 降级执行的告警。**这一行同时进模型上下文与用户可见的结果** ——
/// 「有没有被保护」不该只有翻日志才知道。
pub(crate) const UNENFORCED_MARK: &str = "⚠ 本次执行没有 OS 沙箱保护";

/// 把一条命令的输出拼成工具结果的正文。
///
/// # 为什么单独拆出来
///
/// 里面那句 [`UNENFORCED_MARK`] 是**安全可见性**的东西，而它只在本机
/// 没有沙箱时才出现 —— 三个平台如今都有沙箱（Windows 2026-08-28 接上
/// AppContainer 之后是最后一个），于是它在 CI 与开发机上**都跑不到**。
///
/// 一段谁也执行不到的安全提示，删掉它不会有任何测试红。拆成一个纯函数
/// 之后，它至少有一条测试钉着「`enforced == false` 时必须带这句话」。
fn shell_body(enforced: bool, stdout: &[u8], stderr: &[u8]) -> String {
    let mut body = String::new();
    if !enforced {
        body.push_str(UNENFORCED_MARK);
        body.push('\n');
    }
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    if !stdout.is_empty() {
        body.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&stderr);
    }
    body
}

/// 在 OS 沙箱里跑一条 shell 命令。
pub(super) async fn run_shell(sandbox: &Sandbox, command: &str, timeout_ms: u64) -> ToolResult {
    let argv = match shell_argv(command) {
        Ok(v) => v,
        Err(e) => return ToolResult::err(e),
    };

    // 封闭沙箱里没有 cwd 可用，也不该有：一条命令总要在某个目录里跑，而
    // 「随便挑一个」正是这一整套改动要消除的东西。在这里挡住而不是让
    // `prepare` 拿个假路径去下内核规则 —— 后者会变成一个能跑但围栏是别处的进程
    let Some(cwd) = sandbox.root() else {
        return ToolResult::err(
            "本会话没有绑定工作区，不能执行命令：\
             未绑定的会话可访问的文件范围是空集，没有可用的工作目录。",
        );
    };

    let prepared = match crate::sandbox::prepare(sandbox.exec_policy(), &argv, cwd) {
        Ok(p) => p,
        // 沙箱不可用且未显式降级 —— 这是设计上的主要出口。错误原文会原样
        // 回给模型，让它知道「不是命令写错了，是这台机器上不许执行」
        Err(e) => return ToolResult::err(e.to_string()),
    };

    let mut cmd = tokio::process::Command::from(prepared.command);
    // 超时后 kill 掉；不设的话 Drop 只是丢掉句柄，进程会变孤儿继续跑
    cmd.kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // 命令**开始之前**取时间，用来把这一条与之前的拒绝记录切开。
    // 取晚了（比如失败之后再取）会把这条命令自己的拒绝也滤掉
    let started = crate::egress::now_secs();

    let fut = cmd.output();
    let out = match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), fut).await {
        Err(_) => {
            return ToolResult::err(format!("命令超时（{timeout_ms} ms）已被终止：{command}"));
        }
        Ok(Err(e)) => {
            return ToolResult::err(format!("启动命令失败：{e}"));
        }
        Ok(Ok(o)) => o,
    };

    let mut body = shell_body(prepared.enforced, &out.stdout, &out.stderr);
    // ── 被出网代理拦了的话，把理由拼进来 ──────────────────
    //
    // 只在**非零退出**时问，且只在配了代理时才真的发请求（桌面端两条都不成立，
    // 整段不介入）。要解决的是：https 走 CONNECT，而 curl 丢弃失败 CONNECT
    // 的响应体 —— 代理写好的那几句「换哪个源」到不了模型，它看到的只有
    // `CONNECT tunnel failed, response 403`。详见 egress.rs 顶部。
    if !out.status.success()
        && let Some(why) = crate::egress::denials_since(started).await
    {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[出网被拦]\n");
        body.push_str(&why);
        body.push('\n');
    }

    let code = out.status.code();
    if out.status.success() {
        if body.is_empty() {
            body.push_str("（无输出）");
        }
        ToolResult::ok(body)
    } else {
        // 失败也把输出带上：模型要靠 stderr 判断是命令写错了还是被沙箱拦了
        let signal = killing_signal(&out.status);
        // 标注拼在**第一行**，理由见 `sigkill_note` 的文档
        let note = sigkill_note(code, signal, memory_ceiling_bytes())
            .map(|n| format!(" —— {n}"))
            .unwrap_or_default();
        ToolResult::err(format!("{}{note}\n{body}", failure_head(code, signal)))
    }
}

/// 失败那一行的开头。**被信号杀掉时要把信号号说出来。**
///
/// 原先这里对「没有退出码」一律印 `退出码 signal`，而那是个死胡同：模型
/// 既不知道是几号信号，也就无从判断该不该重试。而这条路径远不是边角 ——
/// 见 [`killing_signal`] 里那段实测。
fn failure_head(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(c), _) => format!("退出码 {c}"),
        (None, Some(s)) => format!("被信号 {s} 杀掉"),
        // Unix 上两者必有其一，Windows 上 `code()` 恒为 `Some` —— 这一支
        // 理论上到不了，但宁可印一句实话，也不要在这里 unwrap
        (None, None) => "异常终止（既没有退出码也没有信号）".to_string(),
    }
}

/// 把它杀掉的那个信号（若有）。非 Unix 一律 `None`。
///
/// # 为什么必须看这个，而不是只看退出码 137
///
/// 直觉上「被 OOM killer 杀掉 = 退出码 137」，**实测不成立**。
/// `shell_argv` 起的是 `sh -c '<命令>'`，而 shell 对最后一条命令会直接
/// `exec` 掉自己 —— 于是被杀的就是 shell 这个进程本身，父进程（也就是这里）
/// 看到的是 `WIFSIGNALED`，`ExitStatus::code()` 返回 **`None`**。
///
/// 在一个 `--memory=80m` 的容器里逐个试过：
///
/// | 命令形状 | `/bin/bash` | `/bin/sh`（dash / busybox） |
/// |---|---|---|
/// | 单条命令（会被 exec 掉） | 信号 9 | 信号 9 |
/// | 两条命令（shell 存活） | **信号 9**（bash 会把信号转发给自己） | 137 |
///
/// 四格里三格根本没有 137，而 `shell_argv` **优先挑的正是 bash**。
/// 只认 137 的话，这个标注在最常见的形状上一次都不会触发 ——
/// 而它不触发的样子和「本来就没有内存问题」完全一样。
#[cfg(unix)]
fn killing_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn killing_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// `SIGKILL` 的信号号。
///
/// 写死 9 而不是取 `libc::SIGKILL`：`libc` 只在 Linux 目标上是依赖
/// （见 Cargo.toml，macOS 走 `sandbox-exec`，不引 libc），为一个常量把它
/// 提升成全平台依赖不划算。而 9 是 POSIX 最早钉死、至今没有任何 Unix
/// 敢动的那三个号之一（1/9/15）。
const SIGKILL: i32 = 9;

/// shell **代为转述**同一件事时用的退出码：128 + 信号号。
const EXIT_CODE_SIGKILL: i32 = 128 + SIGKILL;

/// cgroup v2 里「当前这个 cgroup 的内存上限」。
const CGROUP_V2_MEMORY_MAX: &str = "/sys/fs/cgroup/memory.max";

/// 把「进程被 SIGKILL 了」翻译成模型能据以**换个做法**的信息。
/// 返回的串**拼在第一行**（`退出码 137 —— …` / `被信号 9 杀掉 —— …`）。
///
/// # 这一条修的是什么
///
/// 子进程被 OOM killer 杀掉时，工具结果里只有一个 `137`（或者更糟：一句
/// `退出码 signal`）。模型看不出这是内存问题 —— 沙箱的上限是宿主给的，
/// 容器自己不声张 —— 于是**原样重试**，再被杀一次，一整轮就这么没了。
/// 这段话只要做成一件事：让下一步换成「减小批量」而不是「再来一遍」。
///
/// # 为什么两个入口都要认
///
/// `code == 137` 与 `signal == 9` 是**同一件事的两种转述**，取决于 shell
/// 有没有把自己 exec 掉。实测四格里三格走的是信号那一路，
/// 见 [`killing_signal`] 的表 —— 只认 137 等于这个标注基本不会触发。
///
/// # 为什么措辞是「疑似」而不是断言
///
/// OOM killer 只是 `SIGKILL` **最常见**的来源，一条 `kill -9` 同样是它。
/// 退出状态本身分不出这两者（真要分得靠宿主侧的 Docker `/events` 流，
/// 而那在另一个进程里）。把猜测写成断言的代价是模型照着一个错误的原因去
/// 改代码 —— 比不给标注更糟。
///
/// `SIGTERM`（信号 15 / 退出码 143）刻意**不在此列**：那是「有人礼貌地
/// 要求它退出」，在这条路径上最可能的来源就是我们自己（超时那一支其实
/// 更早就返回了，见上面 `timeout` 那一处），套一句内存猜测纯属误导。
///
/// # 为什么拼在第一行而不是追加在末尾
///
/// [`crate::turn`] 的 `truncate` 保头截尾，而一条被 OOM 杀掉的命令往往
/// 恰好吐了一屏输出 —— 追加在末尾的标注会被截掉，正好在最需要它的那次。
/// 同一行还顺带解决第二件事：界面的一行摘要取的是 `content` 的**首行**，
/// 拼在这里，用户在界面上也一眼看到「疑似内存超限」。
///
/// # 为什么不区分容器与桌面端
///
/// 想过只在容器里加。但**决定要不要重试的是模型，不是用户**：桌面端用户
/// 确实看得见系统监视器，可等他看见的时候模型已经重试完了。而
/// 「这个进程是被 SIGKILL 掉的」在两边同样为真、同样值得换个做法。
///
/// 真正只属于容器的是那个**数字**，所以它单独由 `ceiling` 带进来：有上限
/// 就报出来，没有就只说形状。这样桌面端不会读到一个凭空捏造的内存上限。
fn sigkill_note(code: Option<i32>, signal: Option<i32>, ceiling: Option<u64>) -> Option<String> {
    if code != Some(EXIT_CODE_SIGKILL) && signal != Some(SIGKILL) {
        return None;
    }
    let cap = ceiling.map_or_else(String::new, |b| {
        format!("（本进程的内存上限是 {}）", mib(b))
    });
    Some(format!(
        "疑似内存超限被杀{cap}。\n\
         SIGKILL（信号 9，shell 转述时报成退出码 137）最常见的原因是内存耗尽触发 \
         OOM killer，但一条 `kill -9` 也是它，所以只是疑似。\n\
         若确是内存问题，原样重试还会被杀一次 —— 请减小批量、改成分批或流式处理，\
         或者换一个更省内存的做法（例如逐行读而不是整个读进内存）。"
    ))
}

/// 本进程实际受哪个内存上限约束。拿不到就是 `None` —— **不猜**。
///
/// # 为什么不硬编码 512 MiB
///
/// 那个数字的权威在 `cortexd` 的 `sandbox_runner`（`--memory`），而
/// cortex-agent 依赖不到它（依赖方向是 agent ← cortexd），照抄一份的下场是
/// 哪天它改了、这边还在报旧数。而报错的上限比不报更坏：模型会照着它去分批。
///
/// # 为什么只读这一个路径，不去解析 `/proc/self/cgroup` 拼出真正的那个
///
/// 因为这条路径的「不准」恰好准在需要的地方：容器里 cgroupns 是 private，
/// `/sys/fs/cgroup` 就是这个容器自己的根，`memory.max` 正是 `docker run
/// --memory` 那个值（实测 `--memory=512m` → `536870912`）。而在桌面上，
/// 这个文件要么不存在（Windows / macOS），要么是宿主根 cgroup 的字面量
/// `max`（实测），两种都 parse 不出数 —— 于是**自动**退化成「不报数字」，
/// 正是桌面端该有的行为：那边我们本来就不知道这个数。
///
/// 换句话说，多写的那段 `/proc/self/cgroup` 解析只会让桌面端开始报一个
/// systemd slice 的上限，而那个数与「这条命令为什么被杀」基本无关。
fn memory_ceiling_bytes() -> Option<u64> {
    // 无上限时文件内容是字面量 `max` 而不是一个大数 —— parse 失败即 None，
    // 这不是巧合，是这里唯一要处理的特殊值
    std::fs::read_to_string(CGROUP_V2_MEMORY_MAX)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// 字节数说成人话。上限本来就是按整 MiB 配出来的，不必要小数。
fn mib(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} 字节")
    }
}

/// 挑一个 shell 解释器。
///
/// 不写死 `bash`：精简容器镜像里常常只有 `sh`。也不查 PATH ——
/// PATH 是被沙箱防着的那一侧能影响的东西。
pub(crate) fn shell_argv(command: &str) -> std::result::Result<Vec<String>, String> {
    #[cfg(unix)]
    {
        for sh in ["/bin/bash", "/bin/sh"] {
            if Path::new(sh).exists() {
                return Ok(vec![sh.to_string(), "-c".to_string(), command.to_string()]);
            }
        }
        Err("本机既没有 /bin/bash 也没有 /bin/sh".to_string())
    }
    #[cfg(windows)]
    {
        Ok(vec![
            "cmd.exe".to_string(),
            "/C".to_string(),
            command.to_string(),
        ])
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = command;
        Err(format!("{} 上没有可用的 shell", std::env::consts::OS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// shell 在拿不到 cwd 的时候必须**在起进程之前**就拒绝。
    ///
    /// 这条与上面那条不是重复：文件工具走 `resolve`，shell 根本不走它 ——
    /// 它只要一个工作目录。围栏上的这两个口子是分开的。
    #[tokio::test]
    async fn shell_refuses_before_spawning_when_sealed() {
        let r = execute(
            &Sandbox::sealed(),
            &ToolCall {
                name: "shell".into(),
                arguments: serde_json::json!({"command": "echo hi"}),
            },
        )
        .await;
        assert!(!r.ok, "封闭沙箱里 shell 竟然跑起来了：{}", r.content);
        assert!(
            r.content.contains("没有绑定工作区"),
            "拒绝理由必须是「没绑工作区」，而不是「沙箱不可用」之类的巧合，实际：{}",
            r.content
        );
    }

    /// **工具目录不许写「造好了没人接」的能力**（仓库硬约束第 2 条）。
    ///
    /// 这里守的是它的反面：Windows 沙箱里 `dir` / `vol` 跑不起来，工作区在
    /// 主目录底下时 `git` 也不行。`shell` 的描述如果不说，模型就会一次次撞
    /// 同一堵墙 —— 它看到的只有「拒绝访问」，于是当成命令写错了，换写法重试，
    /// 一轮烧完。
    ///
    /// **两档各自断言自己那句要紧的话**：AppContainer 档要说清 dir/vol 与
    /// 完整构建用不了；受限令牌档要说清**不挡读**。后者尤其不能漏 ——
    /// 一个「说自己能关住不受信代码、实际读什么都能读」的沙箱描述，
    /// 比不描述危险得多。
    ///
    /// 判据跟着 `capability()` 走：降级或有人在场时命令跑在宿主上，
    /// git 是好的，那时候写那句话反而成了另一种「说得不对」。
    #[cfg(windows)]
    #[test]
    fn windows_上_shell_的描述要跟着后端翻面() {
        use crate::sandbox::{Backend, Capability};

        let 受限 = Capability::Available {
            backend: Backend::RestrictedToken,
            detail: "测试".into(),
        };
        let 容器 = Capability::Available {
            backend: Backend::AppContainer,
            detail: "测试".into(),
        };
        let 没有 = Capability::Unavailable {
            reason: "测试".into(),
        };

        let d = shell_description_for(&受限);
        assert!(
            d.contains("不限制读取"),
            "受限令牌档**不挡读**，而描述没说 —— 模型会以为这里能关住不受信代码，
             那正是最危险的那种说反。实际：{d}"
        );
        assert!(
            !d.contains("`dir` 与 `vol` 用不了"),
            "这一档 dir/vol 是好的，描述却写着用不了 —— 模型会绕开本来能用的命令。实际：{d}"
        );
        // 这一档的 HTTPS 现状被实测**推翻过两次**，所以断言钉的是「此刻还剩
        // 哪一条不通」，不是一句笼统的「HTTPS 坏的」：第一版写「git 正常」→
        // git over https 当场 fatal；第二版写「cargo 拉不了新依赖」→ 接了
        // 明文回环镜像之后能拉了。现在只剩 `curl https://`（自己链了 Schannel）。
        // 第三次翻面：连 curl 也通了（沙箱备了 LibreSSL 版）。此刻还剩的
        // 是 PowerShell / .NET 的 HTTPS（SSPI，换不掉）—— 断言钉住这一条，
        // 并且反向钉住「curl 不通」的旧话不许回来。
        assert!(
            d.contains("Invoke-WebRequest"),
            "PowerShell 的 HTTPS 在这一档走不了，描述不说，模型会把它当成
             网络故障反复重试。实际：{d}"
        );
        assert!(
            !d.contains("仍然不通"),
            "curl 现在通了（sandbox::windows_curl），描述还留着旧话。实际：{d}"
        );
        assert!(
            !d.contains("拉不了新依赖"),
            "cargo 现在拉得到新依赖（走 sandbox::windows_cargo_mirror），描述还写着
             拉不了 —— 模型会绕开本来能用的路，去请用户在沙箱外手工 `cargo fetch`。实际：{d}"
        );
        assert!(
            d.contains("CARGO_HOME"),
            "这一档把 CARGO_HOME 指到了沙箱专用的一份（用户的 ~/.cargo 写不进）。
             不说的话，模型看到「第一次构建重新下载依赖」会以为环境坏了，反手把
             CARGO_HOME 改回去 —— 那一改就又是「拒绝访问」。实际：{d}"
        );

        let d = shell_description_for(&容器);
        assert!(
            d.contains("`dir` 与 `vol` 用不了"),
            "AppContainer 档 dir/vol 确实用不了，不说模型就会一直撞。实际：{d}"
        );
        // 曾断言「AppContainer 档跑不了完整构建」—— 2026-08-29 推翻了
        // （换 lld + 授工具链 + 注 LIB，见 sandbox::windows_rust_build）。
        // 现在钉住的是**新的边界**：cargo build 能出 .exe，但 cl.exe 那族不行。
        assert!(
            d.contains("cargo build") && d.contains(".exe"),
            "AppContainer 档现在能 cargo build 出 .exe，描述要说 —— 不说模型会
             以为这一档跑不了构建，白白退回受限令牌档。实际：{d}"
        );
        assert!(
            d.contains("cl.exe") || d.contains("C/C++"),
            "cargo build 能跑不等于什么都能编：cl.exe（cc crate）那族仍挂。
             不说清这条边界，模型撞上 ring/openssl-sys 会反复重试。实际：{d}"
        );
        assert!(
            !d.contains("不限制读取"),
            "AppContainer 档读是默认拒绝的，描述却说不限制读取 —— 说反了。实际：{d}"
        );

        let d = shell_description_for(&没有);
        assert!(
            !d.contains("`dir` 与 `vol` 用不了") && !d.contains("不限制读取"),
            "没有沙箱时命令跑在宿主上，两档的限制一条都不成立，
             写上去就是另一种「说得不对」。实际：{d}"
        );
    }

    /// 137 必须被翻译成一句模型能据以**换做法**的话，而不只是个数字。
    ///
    /// 这条同时钉住措辞里的三件事，少任何一件这个标注就白加了：
    /// 「疑似」（不能把猜测说成结论）、内存上限那个数、以及「别原样重试」
    /// 之后的替代动作。
    #[test]
    fn exit_code_137_is_translated_into_something_actionable() {
        let note = sigkill_note(Some(137), None, Some(512 * 1024 * 1024))
            .expect("137 = 128+9(SIGKILL)，必须给出标注 —— 光一个数字模型只会原样重试");

        assert!(
            note.contains("疑似"),
            "137 也可能来自一条 kill -9，退出码本身分不出来。\
             把猜测写成断言会让模型照着一个错误的原因去改代码。实际：{note}"
        );
        assert!(
            note.contains("512 MiB"),
            "拿得到上限就必须报出来 —— 「省点内存」是空话，\
             「上限 512 MiB」才决定得了批量该切多大。实际：{note}"
        );
        assert!(
            note.contains("重试"),
            "整段话要改掉的行为就是原样重试，不明说这一条等于没说。实际：{note}"
        );
        assert!(
            note.contains("分批") || note.contains("流式"),
            "只说「别重试」会把模型堵死在原地，得给出下一步。实际：{note}"
        );
    }

    /// **被信号杀掉这一路同样要认。** 这条守的是实测出来的那个反直觉事实。
    ///
    /// `sh -c '<命令>'` 会把自己 exec 掉，于是被 OOM killer 杀掉的就是 shell
    /// 本身，父进程看到的是 `WIFSIGNALED`、`code()` 返回 `None`。在
    /// `--memory=80m` 的容器里实测：bash 单条命令、bash 两条命令、
    /// sh 单条命令三种形状都走信号那一路，只有 sh + 两条命令才报 137。
    /// 而 `shell_argv` 优先挑的正是 bash —— 只认 137 的话这个标注几乎不触发，
    /// 且**不触发的样子和「本来就没有内存问题」一模一样**。
    #[test]
    fn a_signalled_process_gets_the_same_note_as_exit_code_137() {
        let by_signal = sigkill_note(None, Some(9), Some(512 * 1024 * 1024))
            .expect("被 SIGKILL 直接杀掉时也必须给标注 —— 这恰恰是最常见的那一路");
        assert!(
            by_signal.contains("疑似内存超限"),
            "两条路必须给出同一句结论，否则模型在 bash 那一路上什么都得不到。实际：{by_signal}"
        );
        assert_eq!(
            by_signal,
            sigkill_note(Some(137), None, Some(512 * 1024 * 1024)).unwrap(),
            "137 与信号 9 是同一件事的两种转述，措辞不该因为哪个 shell 而变"
        );

        // 头一行也得说清楚。原先这里印的是「退出码 signal」——
        // 模型既不知道几号信号，也就无从判断该不该重试
        let head = failure_head(None, Some(9));
        assert!(
            head.contains('9'),
            "被信号杀掉时必须把信号号说出来，「signal」三个字等于没说。实际：{head}"
        );
        assert_eq!(failure_head(Some(2), None), "退出码 2");
    }

    /// 只认 SIGKILL 那两种转述，且拿不到上限时**绝不**编一个数字出来。
    ///
    /// 143 / 信号 15（SIGTERM）单列：它是「有人礼貌地要求退出」，
    /// 在这条路径上最可能就是我们自己的超时 —— 给它套一句内存猜测是误导。
    #[test]
    fn other_exit_codes_get_no_memory_guess() {
        for code in [0, 1, 2, 126, 127, 130, 139, 143] {
            assert!(
                sigkill_note(Some(code), None, Some(512 * 1024 * 1024)).is_none(),
                "退出码 {code} 与内存无关，却拿到了内存标注 —— \
                 尤其 143(SIGTERM) 通常是超时/停容器，把它说成 OOM 会让模型\
                 去优化一段根本不占内存的代码"
            );
        }
        for sig in [1, 2, 11, 15] {
            assert!(
                sigkill_note(None, Some(sig), Some(512 * 1024 * 1024)).is_none(),
                "信号 {sig} 不是 SIGKILL。尤其 11(SIGSEGV) 是崩溃、15(SIGTERM) 是\
                 被要求退出，两者都会被「疑似内存超限」带偏排查方向"
            );
        }
        let no_cap = sigkill_note(Some(137), None, None).expect("没有上限也该说清 137 是怎么回事");
        assert!(
            !no_cap.contains("上限"),
            "桌面端读不到 cgroup 上限，此时报任何数字都是编的 —— \
             模型会照着它去切批量。实际：{no_cap}"
        );
        assert!(
            no_cap.contains("疑似"),
            "没有数字时那句「疑似被 SIGKILL 了」仍然成立且仍然有用。实际：{no_cap}"
        );
    }

    /// 上限只认 cgroup v2 那个文件里的**数字**，`max` 与读不到都算没有。
    ///
    /// 这一条守的是「宁可不报，也不报错的数」：无上限时文件里是字面量 `max`
    /// （已在容器里实测），把它当成 0 或某个默认值都会让模型收到一个假上限。
    #[test]
    fn the_memory_ceiling_is_never_invented() {
        // 本机（Windows 开发机 / 无 cgroup 的 CI）读不到就该是 None；
        // 在有限额的容器里跑同一条测试则应拿到一个正数。两种都合法，
        // 不合法的只有「读不到却给出了一个数」
        match memory_ceiling_bytes() {
            None => {} // 桌面端 / 无限额容器：正确的答案就是「不知道」
            Some(b) => assert!(
                b > 0,
                "cgroup 报了 0 字节上限，这不可能 —— 多半是把 `max` 之类的\
                 非数字当成了 0，而 0 会让模型以为自己一个字节都不能用"
            ),
        }
        assert_eq!(mib(512 * 1024 * 1024), "512 MiB");
        assert_eq!(
            mib(1024),
            "1024 字节",
            "不足 1 MiB 时按字节说，别四舍五入成 0 MiB"
        );
    }

    /// 标注必须落在**第一行**。
    ///
    /// 两个理由都只有在真出事那次才暴露：`turn::truncate` 保头截尾，
    /// 而被 OOM 杀掉的命令往往正好吐满一屏；界面的一行摘要取的也是首行。
    #[test]
    fn the_note_rides_on_the_first_line_where_truncation_cannot_reach_it() {
        // 与 `run_shell` 的失败分支同一套拼法
        let (code, signal) = (Some(137), None);
        let note = sigkill_note(code, signal, Some(512 * 1024 * 1024)).expect("137 必须有标注");
        let rendered = format!(
            "{} —— {note}\n{}",
            failure_head(code, signal),
            "输出".repeat(10_000)
        );
        let first = rendered.lines().next().expect("总有第一行");
        assert!(
            first.contains("疑似内存超限"),
            "标注被挤出了首行。追加在末尾会被 truncate 截掉，\
             而那恰好发生在输出很长、也就是最需要这句话的那一次。首行是：{first}"
        );
        assert!(
            first.chars().count() <= 120,
            "首行还要当界面那句一行摘要（`turn::first_line` 只取 120 字符），\
             太长的话「疑似内存超限」会被切掉。首行有 {} 字符：{first}",
            first.chars().count()
        );
    }
}
