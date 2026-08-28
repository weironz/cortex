//! 「有人在场」时无沙箱执行的边界（D0）。
//!
//! Windows 上没有 landlock / Seatbelt 的对等物，`shell` 从第一天起就是硬拒的。
//! 这让桌面端「能改代码、不能跑测试」。D0 换了一种保证：**用户当场批准**
//! 替代内核隔离 —— 前提是那句话必须**当真**。
//!
//! 这一组测试守的就是「当真」：
//!
//! 1. 没声明有人在场 → 照旧硬拒（谁都别想靠一个默认值溜进来）
//! 2. 声明了有人在场 → 沙箱层放行（本地 agent 才能工作）
//! 3. **`Execute` 是最高风险档** —— 这是整件事安全的全部依据
//! 4. `attended()` 真的把标记接到了沙箱策略上
//!
//! 第三条最要紧。`decide` 只在 `risk < confirm_at` 时放行，而 `Execute`
//! 是最大值，所以「无沙箱 + 无人值守」这个组合**构造不出来** ——
//! 保证来自类型系统，不是来自哪一道运行期检查。有人给 `Risk` 加一个更高的
//! 档位时，那条测试会先红，并在注释里说清为什么不能那么加。

use cortex_agent::sandbox::{self, Attended, Capability, SandboxPolicy};
use cortex_agent::{ApprovalPolicy, Gate, Risk, Turn};

/// 「这台机器上没有沙箱」—— 由测试**指定**，不靠碰运气。
///
/// # 这几条曾经在哪儿都不跑
///
/// 上一版的判据是 `if capability().is_available() { 跳过 }`。而三个平台
/// 如今都有沙箱：Linux landlock、macOS seatbelt、Windows AppContainer
/// （2026-08-28 接上）。Windows 是它们**最后一个还能执行的地方** ——
/// 那天之后这一组在整个仓库里失去了执行环境，**而没有任何东西变红**。
///
/// 一条「本机恰好如此才跑」的测试，迟早会变成一条谁也没跑过的测试。
/// 所以现在用 [`sandbox::prepare_with`] 把那个世界直接构造出来。
fn 无沙箱() -> Capability {
    Capability::Unavailable {
        reason: "测试构造：假装这台机器上没有进程级沙箱".into(),
    }
}

/// 默认（没人在场）必须继续硬拒。
///
/// 这条是 D0 的**反向**保证：新开的这个口子不能因为一个默认值就对
/// cortexd 也生效 —— 那边的批准者可能在另一个城市，而命令跑在服务器上。
#[test]
fn without_attended_the_refusal_stands() {
    let dir = tempfile::tempdir().expect("临时目录");
    let policy = SandboxPolicy::workspace(dir.path());
    assert_eq!(policy.attended, Attended::No, "默认必须是「没人在场」");

    let err = sandbox::prepare_with(
        &无沙箱(),
        &policy,
        &["echo".into(), "hi".into()],
        dir.path(),
    )
    .expect_err("没人在场且本机无沙箱时必须拒绝");
    let msg = err.to_string();
    assert!(
        msg.contains("拒绝执行") && msg.contains(sandbox::UNSANDBOXED_VALUE),
        "拒绝理由必须说清「怎么才能显式降级」，实际：{msg}"
    );
}

/// 声明了有人在场 → 放行，且明确标记**没有**内核保护。
///
/// `enforced` 那个字段是给上层看的：它是 false 就意味着这条命令能读工作区
/// 以外的东西。放行不等于假装有围栏。
#[test]
fn attended_execution_is_allowed_but_marked_unenforced() {
    let dir = tempfile::tempdir().expect("临时目录");
    let policy = SandboxPolicy::workspace(dir.path()).attended();

    let prepared = sandbox::prepare_with(
        &无沙箱(),
        &policy,
        &["echo".into(), "hi".into()],
        dir.path(),
    )
    .expect("有人在场时应当放行 —— 否则桌面端的终端那半永远用不了");
    assert!(
        !prepared.enforced,
        "放行不等于有围栏。enforced 谎报成 true 会让上层以为这条命令被内核限制住了"
    );
}

/// **探测说有沙箱、装配却失败时：必须报错，不许降级裸跑 —— 人在场也不行。**
///
/// 这是 roadmap 里那条「配置失败时不能让用户在不知情的情况下从『有沙箱』
/// 掉回『逐条确认』」的测试。守的是 `prepare_with` 里
/// `prepare_backend(...)?` 那一个问号：哪天有人「好心」把它改成
/// `Err(_) => 裸跑并标记 enforced=false`，用户看到的仍是那套熟悉的确认
/// 弹窗，完全不知道这一次与往常的区别 —— 往常是「这台机器上根本没有
/// 沙箱」（一开始就知道），这一次是「有沙箱、但它坏了」（该修，而不是绕）。
///
/// 特意在 `attended` 打开的情况下断言：`attended` 放的是「本机**没有**
/// 沙箱能力」那条路，不是「有能力但装配失败」这条 —— 两者的区别正是
/// 这条测试存在的理由。
///
/// 失败注入用 `CORTEX_WIN_SANDBOX_HELPER` 指向不存在的文件（`helper_path`
/// 对此显式报错），所以只在 Windows 上跑 —— 但被守的那行代码是三个平台
/// 共用的。
#[cfg(windows)]
#[test]
fn 装配失败必须报错_不许降级裸跑() {
    use cortex_agent::sandbox::Backend;

    // SAFETY: 本测试二进制里没有别的测试读写这个变量（其余测试全走
    // `无沙箱()`，在碰到后端之前就返回了）
    unsafe {
        std::env::set_var("CORTEX_WIN_SANDBOX_HELPER", r"C:\不存在\cortex-helper.exe");
    }
    let dir = tempfile::tempdir().expect("临时目录");
    let policy = SandboxPolicy::workspace(dir.path()).attended();
    let cap = Capability::Available {
        backend: Backend::AppContainer,
        detail: "测试构造：能力在、装配会失败".into(),
    };
    let r = sandbox::prepare_with(
        &cap,
        &policy,
        &["cmd".into(), "/C".into(), "echo hi".into()],
        dir.path(),
    );
    unsafe {
        std::env::remove_var("CORTEX_WIN_SANDBOX_HELPER");
    }
    assert!(
        r.is_err(),
        "探测说有沙箱、装配失败 —— 必须报错。放行（无论 enforced 标成什么）         都是把「沙箱坏了」伪装成「一切如常」"
    );
}

/// **「有人在场」安全的全部依据：`Execute` 是最高风险档。**
///
/// [`ApprovalPolicy::decide`] 只在 `risk < confirm_at` 时放行。`Execute` 是
/// 最大值，所以不存在任何一个 `confirm_at` 能让它自动放行 —— 也就是说
/// 「无沙箱 + 无人值守」这个组合**构造不出来**。保证来自类型系统，
/// 不是来自哪一道运行期检查。
///
/// 这条测试把那个前提钉死。有人往 [`Risk`] 里加一个比 `Execute` 更高的档时，
/// 它会**先于任何别的东西变红**，并在这里说清为什么不能那么加：
/// 那一刻，`confirm_at` 设到新档位就会让 Execute 悄悄绕过确认回路，
/// 而本地 agent 的沙箱已经因为「有人在场」放开了。
#[test]
fn execute_is_the_highest_risk_which_is_what_makes_attended_safe() {
    assert!(
        Risk::Execute > Risk::Write && Risk::Write > Risk::Safe,
        "风险等级的序被改了 —— 确认回路的门槛判断建立在这个序上"
    );

    // 穷举**全部**取值。加了新变体时这里不会自动覆盖到它，
    // 但上面那条断言会先失败，把人引到这段注释
    for confirm_at in [Risk::Safe, Risk::Write, Risk::Execute] {
        let policy = ApprovalPolicy {
            confirm_at,
            bypass: false,
        };
        assert_eq!(
            policy.decide("shell", Risk::Execute),
            Gate::Ask,
            "confirm_at={confirm_at:?} 时 Execute 竟然不用确认。\
             这会让本地 agent 变成「无沙箱且无人值守」—— \
             如果你刚给 Risk 加了一个比 Execute 更高的档，先读 Turn::attended 的注释"
        );
    }
}

/// `Turn::attended()` 必须真的把标记接到沙箱策略上。
///
/// 这是整条链上最容易断而**最不容易发现**的一节：builder 少接一行，
/// 本地 agent 照常启动、照常问「准不准」、用户照常点允许，
/// 然后 `shell` 依旧被拒 —— 而所有日志都显示一切正常。
/// 症状是「D0 做了但没生效」，且只有真去跑一条命令才看得出来。
#[test]
fn the_attended_flag_actually_reaches_the_sandbox_policy() {
    let dir = tempfile::tempdir().expect("临时目录");

    let plain = Turn::on_local_machine(dir.path()).expect("临时目录是合法沙箱根");
    assert_eq!(
        plain.exec_policy().attended,
        Attended::No,
        "默认必须是「没人在场」—— cortexd 走的就是这条路"
    );

    let attended = Turn::on_local_machine(dir.path())
        .expect("临时目录是合法沙箱根")
        .attended();
    assert_eq!(
        attended.exec_policy().attended,
        Attended::Yes,
        "attended() 没把标记传到 SandboxPolicy 上：本地 agent 会照常问「准不准」，\
         用户点了允许，然后命令依旧被拒 —— 而日志里看不出任何异常"
    );

    // 顺序无关：先 with_policy 再 attended，或反过来，结果必须一样
    let reordered = Turn::on_local_machine(dir.path())
        .expect("临时目录是合法沙箱根")
        .with_policy(ApprovalPolicy {
            confirm_at: Risk::Write,
            bypass: false,
        })
        .attended();
    assert_eq!(
        reordered.exec_policy().attended,
        Attended::Yes,
        "builder 的调用顺序不该影响结果"
    );
}
