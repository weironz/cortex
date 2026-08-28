//! Windows 后端：**AppContainer**。
//!
//! # 为什么是 AppContainer，不是别的两条
//!
//! 选型是**测出来的**，证据在 `tests/win_sandbox_probe.rs`（默认
//! `#[ignore]`，可复跑）。三条候选各自的死穴不同：
//!
//! | 机制 | 要管理员 | PowerShell / .NET | |
//! |---|---|---|---|
//! | 受限令牌 `CreateRestrictedToken` | 不要 | **起不来** | ✗ |
//! | 独立本地用户（codex 的做法） | **要**（一次） | 正常 | 可行但贵 |
//! | **AppContainer** | **不要** | **正常** | ✓ |
//!
//! 受限令牌那条的边界是成立的（用户目录挡得住、`cmd` 与 `git` 都跑得动），
//! 死在 **.NET** 上：CLR 在受限令牌下起不来（PowerShell 5.1 `0xFFFF0000`、
//! pwsh 7 `0xE0434352`）。
//!
//! ⚠️ 这里曾经写着「Windows 的 `shell` 工具走 PowerShell」，**那是错的** ——
//! `shell_argv` 在 Windows 上回的是 `cmd.exe /C`（右栏那个交互终端才是
//! PowerShell，而它不走沙箱）。结论不变但理由不同：挡住 .NET 挡掉的是
//! **用户命令里**的 `dotnet`、`.ps1`、以及一大票 .NET 写的 CLI，
//! 不是 `shell` 工具自己。写错的理由留在这儿，免得下次又照着它推。
//!
//! ⚠️ 这也解释了 codex 为什么宁可要那一次管理员配置：独立用户拿的是正常
//! 令牌。**他们的约束不是我们的约束** —— 照抄会让我们白付一次提权。
//!
//! # 这一层挡住什么、挡不住什么
//!
//! 挡住：子进程**读不到用户目录里的东西** —— `~/.ssh`、`~/.aws`、
//! 浏览器 profile、文档。那正是模块头里
//! 「`bash -c 'cat ~/.ssh/id_rsa'`」说的威胁。
//!
//! **挡不住，一条都不许含糊：**
//!
//! 1. ~~网络~~ —— **现在挡住了**，见下面「网络」那一节。留一句在这里是因为
//!    它挡的方式与 Linux 那侧**不一样**：这边没有白名单，只有全开与全关。
//! 2. **`ALL APPLICATION PACKAGES` 可读的系统路径**（`C:\Windows`、
//!    `Program Files` 的大部分）。那里没有用户的秘密，但也不是「什么都
//!    碰不到」。
//! 3. **把工作区里的东西发出去** —— 那是网络那一层的事。
//!
//! # 网络：一个 capability 的事
//!
//! 实测（2026-08-28，同一条 `curl -m 8 https://example.com` 两跑）：
//!
//! | | HTTP | DNS |
//! |---|---|---|
//! | 不给 capability | 退出码 6，域名都解析不出来 | 失败 |
//! | 给 `internetClient`（`S-1-15-3-1`） | **200** | 通 |
//!
//! 所以 [`NetworkPolicy`] 在这一层是个干净的二元开关。
//!
//! ⚠️ **两边的 `Allowed` 不是同一个东西。** Linux 侧 `Allowed` 走
//! `cortex-egress-proxy` 的 CONNECT 白名单；这边一放就是整个互联网。
//! 把它们当成一回事，就会以为工作区里的东西发不出去。
//!
//! 顺带：loopback 仍然断着（AppContainer 一贯如此），所以沙箱里的命令
//! **连不上本机跑着的开发服务器**。那是这一层免费送的，不是我们设的 ——
//! 逃逸测试里有一条盯着它，为的是「它哪天变了我们要知道」。
//!
//! # git 能用，但有一条位置上的前提
//!
//! git 每条子命令开头都要 `getcwd`。Windows 上那是 `GetLongPathNameW`，
//! 它要**在每一级的上级目录里把这一级的名字查出来** —— 也就是要
//! `FILE_LIST_DIRECTORY`。少一级，整条调用 `ERROR_ACCESS_DENIED`，git 报
//! `fatal: unable to get current working directory: Permission denied`。
//!
//! 所以 [`grant_ancestors`] 会把工作区的每一级上级目录都授上「可列举」。
//! 卷根（`C:/`、`D:/`）**不需要**，实测 `D:/` 一条 ACE 都没有时 git 全通。
//!
//! **改不动的那一级就是天花板。** `C:/Users` 归 SYSTEM，普通用户没有
//! `WRITE_DAC` —— 所以工作区放在 `C:/Users/<你>/…` 底下时，沙箱里的 git
//! 用不了。放在别处（用户自己建的目录）则零提权可用。
//!
//! 另外 git 还要读 `~/.gitconfig`，而那在主目录里、沙箱按设计读不到，
//! 且 git 把「读不了全局配置」当**致命错**。所以 [`synth_git_config`]
//! 合成一份只带身份的配置交给它 —— 见那个函数为什么不是「放行原文件」。
//!
//! # 还挡掉的两样
//!
//! `dir` 与 `vol` 要卷信息，那要**卷根上一条 ACE**，而卷根归 SYSTEM ——
//! 那一条是唯一需要管理员的东西，且**只买这两个命令**（`git` 不需要它，
//! `list_dir` 工具、cmd 的 `for` 通配、.NET 的 `Directory.GetFiles` 都不需要）。
//! 没有它时 PowerShell 的当前位置也会回落成 `C:/`。
//!
//! 排除掉的两条，写下来免得重查：
//!
//! - **不是祖先目录「穿不过去」。** 只给 `FILE_TRAVERSE` 不给
//!   `FILE_LIST_DIRECTORY` 时 git 的错误一个字不变 —— 它要的是查名字。
//! - **git 的回落那条路走不通。** `GetLongPathNameW` 失败后 git 会试
//!   `GetFinalPathNameByHandleW(..., VOLUME_NAME_DOS)`，而把设备路径映射回
//!   盘符对 AppContainer 是拒绝的（同一个句柄换 `VOLUME_NAME_NT` 立刻就通）。
//!   那是对象管理器的东西，文件 ACL 够不着。
//!
//! # 授权的代价：一次性的，但那一次可能很贵
//!
//! AppContainer 的访问检查要求对象的 DACL 上有容器 SID 的 ACE。Landlock 的
//! 授权是**进程的**、随进程消失、加一条不要钱；这里的授权是**磁盘上的**，
//! 而且带继承标志写 DACL 会把 ACE **递归铺到整棵已有子树**。
//!
//! 实测（2026-08-28，`D:/codes/cortex`，793 059 个文件，`target/` 占 762k）：
//!
//! | | 耗时 |
//! |---|---|
//! | 首次授权（铺满子树） | **153.8 s** |
//! | 之后每条命令 | **201 ms** |
//! | 空工作区每条命令 | **60 ms** |
//!
//! 三条设计因此固定下来：
//!
//! 1. **只授工作区子树**，不授 `SandboxPolicy` 里那些工具链缓存目录
//!    （见 [`writable_under_cwd`]）。
//! 2. **授过就不再授**（见 [`already_granted`]）—— ACL 跨进程跨重启都在。
//! 3. **授权在父进程做，不在 helper 里做**（见 [`prepare`]）—— helper 跑在
//!    命令超时里面，放那儿的话大仓库上第一条命令会以「命令超时」收场。
//!
//! 还没解决的：首次绑定一个**已经建过**的大仓库仍然要等一两分钟，且期间
//! 没有任何进度。`TreeSetNamedSecurityInfoW` 带进度回调，是那一步的解法。
//!
//! # 为什么要一个 helper 进程
//!
//! AppContainer 靠 `STARTUPINFOEXW` 上的
//! `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` 生效，而
//! **`std::process::Command` 在 stable Rust 上设不了 proc-thread 属性**。
//! 所以 [`prepare`] 回的不是「真命令」，是「本程序自己 + `--win-sandbox-exec`」，
//! 由 [`exec_in_container`] 在那个模式下用 `CreateProcessW` 起真命令。
//!
//! stdio 不用自己铺管道：helper **继承**父进程给它的那三个句柄，
//! 真命令再继承一层，于是父进程接的管道原样通到底。

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::path::Path;

use cortex_core::{CortexError, Result};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, FreeSid, GetAce,
    SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::{Backend, Capability, NetworkPolicy, SandboxPolicy};

/// 这个装机用哪个 AppContainer。
///
/// **固定名字，不随会话变**：每个 profile 在系统里是一条持久记录
/// （`%LOCALAPPDATA%\Packages\<name>`），一个会话一个的话，用户的磁盘上
/// 会攒出成千上万个目录 —— 这个仓库今晚刚数过两处「只增不删」。
///
/// 反过来的代价是**同一台机器上所有会话共用一个容器身份**：会话 A 授权
/// 过的目录，会话 B 的子进程也进得去。这一版接受它 —— 路径围栏
/// （`tools::Sandbox::resolve`）仍然逐会话把关，而这一层要防的是
/// 「命令跑起来之后越过围栏」，不是会话之间互相隔离。
///
/// 要做到逐会话隔离，得一会话一 profile 加一套回收，那是另一件事。
const CONTAINER_NAME: &str = "cortex.agent.sandbox";

/// helper 模式的开关。见模块文档「为什么要一个 helper 进程」。
///
/// 名字里带 `win`：它在别的平台上不存在，写成通用名字会让人以为
/// 三个平台都有这条路。
pub const HELPER_FLAG: &str = "--win-sandbox-exec";

/// 去掉 Windows 的 \\?\ verbatim 前缀。
///
/// Rust 的 `canonicalize` 在 Windows 上回的是 verbatim 形式，而**很多程序
/// 不认它** —— 实测 PowerShell 拿它当 cwd 时报「路径不存在」，而那个路径
/// 就在那儿。
///
/// `workspace::display` 里为「入库与回显」做过同一件事；这里是为
/// 「传给 CreateProcessW」再做一次。两处用途不同，合成一个公共函数会让
/// 那边的文档说不清自己在讲什么。
fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    s.strip_prefix(r"\\?\").unwrap_or(s).to_string()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> CortexError {
    // SAFETY: GetLastError 无参数、不解引用任何东西
    let e = unsafe { GetLastError() };
    CortexError::Invalid(format!("{what} 失败（Win32 错误 {e}）"))
}

/// 这台机器的 AppContainer SID。**调用方负责 [`FreeSid`]**。
///
/// 先 `Create` 后 `Derive`：profile 已存在时 `Create` 回
/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`，那不是失败 —— 第二次以后
/// 的每一次运行都会走到那里。
///
/// # Errors
/// 两条都拿不到 SID 时报错。那通常意味着这个 Windows 版本不支持
/// AppContainer（Server Core 的某些安装形态），而不是配置问题。
unsafe fn container_sid() -> Result<*mut c_void> {
    let name = wide(CONTAINER_NAME);
    let display = wide("Cortex agent sandbox");
    let mut sid: *mut c_void = std::ptr::null_mut();

    let hr = CreateAppContainerProfile(
        name.as_ptr(),
        display.as_ptr(),
        display.as_ptr(),
        std::ptr::null(),
        0,
        &mut sid,
    );
    if hr == 0 {
        return Ok(sid);
    }

    let hr2 = DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid);
    if hr2 == 0 {
        return Ok(sid);
    }
    Err(CortexError::Invalid(format!(
        "拿不到 AppContainer 身份（create={hr:#x} derive={hr2:#x}）—— \
         这个 Windows 版本可能不支持 AppContainer"
    )))
}

/// 把一个目录授给容器身份（完全控制 + 继承给子项）。
///
/// # 为什么是「加一条」而不是「换掉整张 DACL」
///
/// `SetEntriesInAclW` 的第三个参数传 NULL 时**是从空表开始建**，那会把
/// 目录原有的权限全抹掉 —— 包括用户自己的。这里必须先读旧 DACL 再合并。
///
/// # Errors
/// 目录不存在、或调用方没有改这个目录 DACL 的权限（改 DACL 要
/// `WRITE_DAC`，目录的属主天然就有）。
/// `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`。不给继承的话，目录本身进得去
/// 而里面新建的文件进不去。
const INHERIT_ALL: u32 = 3;
/// 不继承：这条 ACE 只作用在这个目录本身，一个子项都不碰。
const INHERIT_NONE: u32 = 0;
/// `FILE_ALL_ACCESS`。
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
/// 「能穿过去，也能看见这一层有哪些名字」——
/// `FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE`。
///
/// **不含读文件内容**：拿到它的进程能列出这个目录里有哪些条目，但打不开
/// 其中任何一个（那些各自有自己的 ACL，上面没有容器 SID）。
const LIST_ONLY: u32 = 1 | 0x20 | 0x80 | 0x2_0000 | 0x10_0000;

/// `ACCESS_ALLOWED_ACE_TYPE`。`windows-sys` 没有导出它（那一族 ACE 类型常量
/// 都没导），而它是 winnt.h 里钉死的 0，不是版本相关的东西。
const ACE_TYPE_ALLOWED: u8 = 0;

/// 这条 ACE 是不是已经在了。
///
/// 省掉的**不是一次系统调用，是一整趟子树遍历**：带继承标志写 DACL 会把
/// ACE 铺到每一个已有子项，工作区越大越贵（一个带 `target/` 的 Rust 仓库
/// 是十万量级的文件）。而 ACL 住在磁盘上，跨进程跨重启都在 —— 所以从第二条
/// 命令开始本来就不需要再写一次。
///
/// 判据必须**连继承标志一起看**。只在目录本身有权限、子项没有的话，命令能
/// `cd` 进去却读不了里面任何文件 —— 那种「授过了」等于没授，而如果这里只比
/// SID 就会把它当成已完成，症状是「第一次能用，之后莫名其妙读不到文件」。
///
/// 拿不准时一律回 `false`：多写一次只是慢，漏写一次是功能坏掉。
unsafe fn already_granted(dacl: *const ACL, sid: *mut c_void, inherit: u32, mask: u32) -> bool {
    // DACL 为空指针意味着「没有 DACL」= 人人可访问。那种目录不该出现在
    // 工作区里，但真出现时也轮不到这里处理，照常走授权那条路
    if dacl.is_null() {
        return false;
    }
    let count = unsafe { (*dacl).AceCount };
    for i in 0..u32::from(count) {
        let mut ace: *mut c_void = std::ptr::null_mut();
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
            return false;
        }
        let header = ace.cast::<ACE_HEADER>();
        // 只认「允许」型；拒绝型 ACE 在这里不构成「已经授过」
        if unsafe { (*header).AceType } != ACE_TYPE_ALLOWED {
            continue;
        }
        // 继承标志必须**两个都在**，理由见函数头
        if u32::from(unsafe { (*header).AceFlags }) & inherit != inherit {
            continue;
        }
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        if unsafe { (*allowed).Mask } & mask != mask {
            continue;
        }
        let ace_sid = unsafe { std::ptr::addr_of!((*allowed).SidStart) }.cast::<c_void>();
        if unsafe { EqualSid(ace_sid.cast_mut(), sid) } != 0 {
            return true;
        }
    }
    false
}

unsafe fn grant_to_container(
    dir: &Path,
    sid: *mut c_void,
    inherit: u32,
    mask: u32,
) -> Result<bool> {
    use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;

    let mut path = wide(&dir.display().to_string());

    // 先取旧的 DACL —— 见上面那段：不取的话是从空表重建
    let mut old_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: *mut c_void = std::ptr::null_mut();
    let rc = GetNamedSecurityInfoW(
        path.as_ptr(),
        SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut old_dacl,
        std::ptr::null_mut(),
        &mut sd,
    );
    if rc != 0 {
        return Err(CortexError::Invalid(format!(
            "读不到 {} 的权限表（错误 {rc}）",
            dir.display()
        )));
    }

    // 已经授过就直接回 —— 见 `already_granted`：省掉的是一整趟子树遍历
    if already_granted(old_dacl, sid, inherit, mask) {
        return Ok(false);
    }

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: inherit,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: sid.cast::<u16>(),
        },
    };
    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let rc = SetEntriesInAclW(1, &ea, old_dacl, &mut new_dacl);
    if rc != 0 {
        return Err(CortexError::Invalid(format!("合并权限表失败（错误 {rc}）")));
    }

    let rc = SetNamedSecurityInfoW(
        path.as_mut_ptr(),
        SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        new_dacl,
        std::ptr::null_mut(),
    );
    if rc != 0 {
        return Err(CortexError::Invalid(format!(
            "写不回 {} 的权限表（错误 {rc}）—— 需要对这个目录有 WRITE_DAC",
            dir.display()
        )));
    }
    Ok(true)
}

/// 探测：这台机器上 AppContainer 能不能用。
///
/// # 它**真的跑一次 helper**，不只是问一下 SID
///
/// 第一版只验了「拿不拿得到 AppContainer SID」，于是它在 helper 根本
/// 不存在的进程里也报「已启用」—— 而那时每一条命令都会以
/// 「Unrecognized option」失败。更糟的是两条逃逸测试因此变绿：
/// 命令没跑，当然什么都没泄露。
///
/// **一次探测只有走完用户那条路才算数**（这个仓库记过 2 次
/// 「验证走的不是用户那条路」）。所以这里真的起一次 helper，让它在容器里
/// 跑一条最短的命令。代价是一次进程启动，而 [`super::capability`] 用
/// `OnceLock` 缓存，整个进程只付一次。
pub(super) fn detect() -> Capability {
    // SAFETY: 只调两个查询/创建 API，拿到的指针立刻交回 FreeSid
    let sid = match unsafe { container_sid() } {
        Ok(s) => s,
        Err(e) => {
            return Capability::Unavailable {
                reason: e.to_string(),
            };
        }
    };
    unsafe { FreeSid(sid) };

    let helper = match helper_path() {
        Ok(p) => p,
        Err(e) => {
            return Capability::Unavailable {
                reason: e.to_string(),
            };
        }
    };

    // 最短的一条：起 cmd，什么都不做就退。用 cwd = 系统临时目录，
    // 那里本来就人人可写，探测不该依赖任何工作区
    let probe = HelperPlan {
        argv: vec!["cmd.exe".into(), "/c".into(), "exit".into(), "0".into()],
        cwd: std::env::temp_dir().display().to_string(),
        // 探针不需要网 —— 而且给了的话，「能不能起进程」这个问题就掺进了
        // 「能不能出网」，探测失败时分不清是哪一个
        network: false,
    };
    let Ok(json) = serde_json::to_string(&probe) else {
        return Capability::Unavailable {
            reason: "装配沙箱探针失败".into(),
        };
    };

    match std::process::Command::new(&helper)
        .arg(HELPER_FLAG)
        .arg(json)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(out) if out.status.success() => Capability::Available {
            backend: Backend::AppContainer,
            detail: format!("AppContainer（{CONTAINER_NAME}）· 文件与网络边界；桌面未隔离"),
        },
        Ok(out) => Capability::Unavailable {
            reason: format!(
                "AppContainer 探针没跑通（helper={}，退出码 {:?}）：{}。\
                 helper 必须是实现了 {HELPER_FLAG} 的那个二进制，\
                 可用 {HELPER_ENV} 指定",
                helper.display(),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        },
        Err(e) => Capability::Unavailable {
            reason: format!("起不动沙箱 helper（{}）：{e}", helper.display()),
        },
    }
}

/// 覆盖 helper 的路径。**给测试用**，不是配置项。
///
/// 见 [`helper_path`]：本进程未必就是那个实现了 helper 模式的二进制。
pub const HELPER_ENV: &str = "CORTEX_WIN_SANDBOX_HELPER";

/// `internetClient` —— AppContainer 里「可以往外拨」的那一个 capability。
///
/// 写字面量而不是查名字：这个 SID 是 Windows 钉死的众所周知值，
/// 而按名字派生要多一次可能失败的调用，失败时的回落是「静默无网」。
const INTERNET_CLIENT_SID: &str = "S-1-15-3-1";

/// 哪个二进制来当 helper。
///
/// # 为什么不能直接用 `current_exe()`
///
/// 第一版就是那么写的，而它在测试里当场炸了：跑测试的是测试二进制，
/// 它不认识 `--win-sandbox-exec`，于是 helper 起来就报
/// 「Unrecognized option」——**而两条逃逸测试因此"通过"了**（命令根本
/// 没跑，当然什么都没泄露）。那是最坏的一种绿。
///
/// 同一个坑在生产上也成立：`cortex-agentd` 也链接这个 crate，而实现
/// helper 模式的是 `cortex-local`。
///
/// 顺序：显式覆盖 → 本程序旁边的 `cortex-local.exe` → 本程序自己。
fn helper_path() -> Result<std::path::PathBuf> {
    if let Some(p) = std::env::var_os(HELPER_ENV) {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        return Err(CortexError::Invalid(format!(
            "{HELPER_ENV} 指向的不是一个文件：{}",
            p.display()
        )));
    }
    let me = std::env::current_exe()
        .map_err(|e| CortexError::Invalid(format!("拿不到本程序路径：{e}")))?;
    if let Some(dir) = me.parent() {
        let sibling = dir.join("cortex-local.exe");
        if sibling.is_file() {
            return Ok(sibling);
        }
        // cargo 把测试二进制放在 `target/debug/deps/`，而 `cortex-local.exe`
        // 在上一层 `target/debug/`。不认这一层的话，**这个后端唯一的逃逸测试
        // 要手设 `CORTEX_WIN_SANDBOX_HELPER` 才跑得起来** —— 而一个要手设
        // 环境变量的测试，就是一个迟早没人跑的测试。
        //
        // 只在目录名恰好是 `deps` 时才往上找一层：这是 cargo 的布局，不是
        // 一条泛泛的「找不到就往上翻」。而且就算翻错了也不会造出假信号 ——
        // `detect` 会真的把它跑一遍，跑不通就是 `Unavailable`
        if dir.file_name().is_some_and(|n| n == "deps")
            && let Some(up) = dir.parent()
        {
            let sibling = up.join("cortex-local.exe");
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }
    Ok(me)
}

/// 挑出**落在工作区里面**的可写根 —— Windows 上只授这些。
///
/// # 为什么不照 `SandboxPolicy` 全授
///
/// Landlock 的授权是**这个进程的**，随进程一起消失，多加一条几乎不要钱。
/// AppContainer 没有这种东西：唯一的办法是把容器 SID 写进目录的 ACL，
/// 那是**磁盘上的持久改动**；而带继承标志的 `SetNamedSecurityInfoW` 还会
/// 把这条 ACE **递归铺到整棵已有子树**。
///
/// 实测（2026-08-28，一台装了完整工具链的开发机，一条 `type a.txt`）：
///
/// | 目录 | 授权耗时 |
/// |---|---|
/// | 临时工作区 | 0.6 ms |
/// | `~/.cargo/registry` | 6.97 s |
/// | `~/.gradle/caches` | 5.01 s |
/// | `~/.cache` | 3.61 s |
/// | `~/.bun/install/cache` | 1.00 s |
///
/// 合计 **23.4 秒，每条命令一次**。而慢还是次要的：它把「容器可写」
/// **永久**刻进了用户真实的 cargo 仓库与 gradle 缓存，沙箱退出之后仍然留着。
/// 一个为了「限制」而存在的机制，反过来在宿主机上放宽了权限 —— 这一条
/// 单独就足以否掉全授。
///
/// 顺带修掉的两处：`SandboxPolicy` 里那些 Unix 路径在 Windows 上会被当成
/// **盘符相对路径**解析（`/tmp` → `C:\tmp`，这台机器上它真的存在，于是真被
/// 授了出去）；`~/.cargo/registry` 因为同时来自 `HOME_WRITABLE_SUBDIRS` 与
/// `CARGO_HOME` 而被授了两遍，白花 6.7 秒。
///
/// **代价要说清楚**：沙箱里的 `cargo build` 写不了 `~/.cargo/registry`，
/// 需要下载依赖的构建会失败。解法不是把它授出去，而是给容器一份工作区内的
/// `CARGO_HOME`（阶段 2），那样缓存跟着工作区走、也不碰宿主机。
fn writable_under_cwd(policy: &SandboxPolicy, cwd: &Path) -> Vec<String> {
    // 与 `existing_writable` 用同一种规范化形式，否则 `starts_with` 会在
    // 「verbatim 前缀 vs 普通路径」上永远不相等，结果是一个都不授
    let base = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    // 走 `existing_writable` 而不是直接读 `writable_roots`：它顺手把不存在的
    // 路径滤掉、把路径规范化、并且**去重**。这三件事都不是可有可无的 ——
    // 直接读 `writable_roots` 时实测 `~/.cargo/registry` 被授了两遍
    // （`HOME_WRITABLE_SUBDIRS` 一次、`CARGO_HOME` 一次，一个写 `/` 一个写
    // `\`，字符串上不相等），白跑一整趟子树遍历
    policy
        .existing_writable()
        .into_iter()
        .filter(|p| p.starts_with(&base))
        .map(|p| strip_verbatim(&p.display().to_string()))
        .collect()
}

/// 装配一条待执行的命令。
///
/// 回的是**本程序 + [`HELPER_FLAG`] + 一段 JSON**，不是真命令 ——
/// 理由见模块文档。
pub(super) fn prepare(
    policy: &SandboxPolicy,
    argv: &[String],
    cwd: &Path,
) -> Result<std::process::Command> {
    let me = helper_path()?;

    // **授权在这里做，不在 helper 里做。**
    //
    // 它是一次性的（见 `already_granted`），但那一次可能很贵：实测一个
    // 793 059 个文件的仓库（`target/` 占 762k）首次授权 **153.8 秒**，
    // 之后每条命令 201 毫秒。而 helper 是跑在 `run_shell` 的命令超时**里面**
    // 的 —— 放在那儿的话，大仓库上的第一条命令会以「命令超时（60000 ms）
    // 已被终止」收场，把「正在准备工作区」谎报成「你的命令有问题」，
    // 而重试一次又会从头再来。
    //
    // `prepare` 跑在超时开始**之前**，所以这笔账花在它该花的地方。
    let git_cfg = grant_workspace(policy, cwd)?;

    let plan = HelperPlan {
        argv: argv.to_vec(),
        cwd: strip_verbatim(&cwd.display().to_string()),
        network: matches!(policy.network, NetworkPolicy::Allowed),
    };
    let json = serde_json::to_string(&plan)
        .map_err(|e| CortexError::Invalid(format!("装配沙箱计划失败：{e}")))?;

    let mut cmd = std::process::Command::new(me);
    cmd.arg(HELPER_FLAG).arg(json);
    // helper 继承这份环境，真命令再继承一层 —— 于是 git 看到的就是这里设的
    if let Some(cfg) = git_cfg {
        cmd.env("GIT_CONFIG_GLOBAL", cfg);
    }
    Ok(cmd)
}

/// helper 模式收到的那份计划。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct HelperPlan {
    argv: Vec<String>,
    cwd: String,
    /// 这条命令允不允许出网。**默认 false** —— serde 的默认值在这里是
    /// 安全的那一侧：旧计划（没有这个字段）落到「不给网」，而不是「给网」
    #[serde(default)]
    network: bool,
}

/// helper 模式的主体：在容器里起真命令，用它的退出码结束本进程。
///
/// **不返回** —— 它就是这个进程存在的全部理由。
///
/// # Panics
/// 起不来时打一句到 stderr 并以 127 退出（与 shell 的「命令找不到」同码）。
pub fn exec_in_container(plan_json: &str) -> ! {
    let code = match run_helper(plan_json) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("沙箱起不来：{e}");
            127
        }
    };
    std::process::exit(i32::try_from(code).unwrap_or(1));
}

fn run_helper(plan_json: &str) -> Result<u32> {
    let plan: HelperPlan = serde_json::from_str(plan_json)
        .map_err(|e| CortexError::Invalid(format!("读不懂沙箱计划：{e}")))?;

    // SAFETY: 下面整段是原始 Win32。指针要么来自刚拿到的 SID，
    // 要么来自本函数栈上的缓冲，生命周期都覆盖调用
    unsafe {
        let sid = container_sid()?;
        let rc = spawn_in_container(sid, &plan.argv, &plan.cwd, plan.network);
        FreeSid(sid);
        rc
    }
}

/// 合成一份最小的 git 全局配置，回 `(GIT_CONFIG_GLOBAL 的路径)`。
///
/// # 为什么必须有这一步
///
/// git 每次启动都要读 `~/.gitconfig`。那个文件在**主目录**里，而不让容器
/// 看见主目录正是这个沙箱存在的理由 —— 于是 git 拿到 `Permission denied`，
/// 并且**把它当致命错**：
///
/// ```text
/// warning: unable to access 'C:/Users/x/.gitconfig': Permission denied
/// fatal: unknown error occurred while reading the configuration files
/// ```
///
/// # 为什么是「合成」而不是「把 .gitconfig 放行」
///
/// 放行一条 ACE 就完事，但 `~/.gitconfig` 里可以有
/// `[credential]` 的 helper、`url.*.insteadOf` 里嵌的令牌、
/// `user.signingkey`。沙箱里的进程拿到那些，等于把凭据交出去了。
///
/// 所以这里**只抄身份**：`user.name` / `user.email`，外加两个影响正确性的
/// （换行处理、默认分支名）。别的一律不带 —— 拿不准的就是不带。
///
/// 读法走 `git config --get` 而不是自己解析文件：git 自己的优先级
/// （system → global → include）比我们复现的任何版本都准，而复现错了
/// 的症状是「提交的作者名跟平时不一样」，几乎不会有人归因到这里。
///
/// # 放在哪
///
/// `%LOCALAPPDATA%/cortex/sandbox/` 下，并**只把那两个文件**授给容器。
/// 不需要给目录列举权：git 是按完整路径打开它的，不用先列目录。
fn synth_git_config(sid: *mut c_void) -> Option<std::path::PathBuf> {
    // **进程内只做一次。** 里面要起四个 `git config` 子进程，而这条路每条
    // 命令都走 —— 不缓存的话，为了一份整会话不变的配置，每条命令多付四次
    // 进程启动（Windows 上不便宜），把 60 毫秒的开销直接翻几倍。
    //
    // 代价说清楚：agent 跑着的时候你改了 `~/.gitconfig`，要重启才生效。
    // 那是「改身份」这种一年一次的事，换每条命令都快得多，值。
    static CACHE: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| synth_git_config_uncached(sid)).clone()
}

fn synth_git_config_uncached(sid: *mut c_void) -> Option<std::path::PathBuf> {
    fn get(key: &str) -> Option<String> {
        let out = std::process::Command::new("git")
            .args(["config", "--get", key])
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let v = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (!v.is_empty()).then_some(v)
    }

    let dir = dirs_local_app_data()?.join("cortex").join("sandbox");
    std::fs::create_dir_all(&dir).ok()?;
    let cfg = dir.join("gitconfig");
    let ignore = dir.join("gitignore");

    let mut body =
        String::from("# 由 Cortex 沙箱生成：只抄身份，不带你的 credential helper 与令牌\n");
    let name = get("user.name");
    let email = get("user.email");
    if name.is_none() && email.is_none() {
        // 连 git 都没有，或者用户没配身份。那就不写配置、也不设环境变量 ——
        // 设了反而会把「git 不存在」变成一条更难懂的错
        return None;
    }
    body.push_str("[user]\n");
    if let Some(v) = name {
        body.push_str(&format!("\tname = {v}\n"));
    }
    if let Some(v) = email {
        body.push_str(&format!("\temail = {v}\n"));
    }
    body.push_str("[core]\n");
    if let Some(v) = get("core.autocrlf") {
        body.push_str(&format!("\tautocrlf = {v}\n"));
    }
    // 指一份空的忽略文件。不指的话 git 每条命令都要抱怨一次读不到
    // `~/.config/git/ignore` —— 那是警告不是错误，但它会跟着每一条命令进
    // 模型上下文，而模型会试图去「修」一个根本不该它管的问题
    body.push_str(&format!(
        "\texcludesFile = {}\n",
        ignore.display().to_string().replace('\\', "/")
    ));
    if let Some(v) = get("init.defaultBranch") {
        body.push_str(&format!("[init]\n\tdefaultBranch = {v}\n"));
    }

    // 内容没变就不重写：这条路每条命令都走一遍
    if std::fs::read_to_string(&cfg).ok().as_deref() != Some(body.as_str()) {
        std::fs::write(&cfg, &body).ok()?;
    }
    if !ignore.exists() {
        std::fs::write(&ignore, "").ok()?;
    }
    // FILE_GENERIC_READ。**只读**，且只给这两个文件 —— 目录不授
    const READ_ONLY: u32 = 0x0012_0089;
    for f in [&cfg, &ignore] {
        // SAFETY: sid 来自调用方，活过整个循环
        let _ = unsafe { grant_to_container(f, sid, INHERIT_NONE, READ_ONLY) };
    }
    Some(cfg)
}

/// `%LOCALAPPDATA%`。不引 `dirs` crate：这一处的需求就是读一个环境变量。
fn dirs_local_app_data() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
}

/// 给工作区的**每一级祖先**授「能穿过去、能看见这一层的名字」。
///
/// # 为什么必须有这一步：git 没有它一步都走不了
///
/// git 每条子命令开头都要 `getcwd`。Windows 上那是
/// `GetLongPathNameW(当前目录)` —— 而它要**在每一级的父目录里把这一级的
/// 名字查出来**，也就是要 `FILE_LIST_DIRECTORY`。少一级，整条调用就
/// `ERROR_ACCESS_DENIED`，git 报的是
/// `fatal: unable to get current working directory: Permission denied`，
/// 或者 `fatal: Invalid path '<父目录>': Permission denied`。
///
/// 实测（2026-08-28）：`D:/cortex-probe/ws` 这个工作区，**只**给
/// `D:/cortex-probe` 加上这一条之后，`git init` / `add` / `commit` / `log`
/// 全部通过；不加则第一条就死。
///
/// # 排除掉的三条，写下来免得重查
///
/// - **不是卷根。** 卷根那条 ACE（要管理员）只影响 `dir` / `vol` /
///   PowerShell 的当前位置 —— D 盘卷根没授权时 git 照样全通。
/// - **不是「穿行权」不够。** 只给 `FILE_TRAVERSE` 不给
///   `FILE_LIST_DIRECTORY` 时 git 的错误一个字不变：它要的是**查名字**，
///   不是「路过」。
/// - **回落那条路走不通。** git 在 `GetLongPathNameW` 失败后会回落到
///   `GetFinalPathNameByHandleW(..., VOLUME_NAME_DOS)`，而那一步要把设备
///   路径映射回盘符，对 AppContainer 是拒绝的（同一个句柄换
///   `VOLUME_NAME_NT` 立刻就通）。那是对象管理器的东西，文件 ACL 够不着。
///
/// # 它放宽了什么
///
/// [`LIST_ONLY`] **不含读文件内容**。容器能列出工作区上面每一级有哪些
/// 条目，但打不开其中任何一个 —— 逃逸测试里「读不到 / 写不进用户目录」
/// 那两条守的就是这一点。
///
/// ⚠️ 代价最实的一处：工作区在 `C:/Users/<你>/…` 底下时，这一步会给**你的
/// 主目录**加上列举权，也就是容器能看见你主目录里有哪些文件名（读不到内容）。
/// 换来的是 git 能用。工作区在别的盘上时完全不涉及主目录。
///
/// # 改不动的一律跳过
///
/// `C:/`、`C:/Users` 归 SYSTEM，普通用户没有 `WRITE_DAC`（实测错误 5）。
/// 为它们失败的话每条命令都会以「写不回权限表」收场，而那既不是用户的错、
/// 也不是我们能修的。跳过是安全的：**授不上只会更严，不会更松**。
fn grant_ancestors(workspace: &Path, sid: *mut c_void) -> Result<()> {
    let mut chain: Vec<&Path> = workspace.ancestors().skip(1).collect();
    chain.reverse();
    // 卷根（`C:/`、`D:/`）不授也不算断链 —— 实测 `D:/` 一条 ACE 都没有时
    // git 照样全通。它只影响 `dir` / `vol`（那两个要卷信息），而卷根归
    // SYSTEM，授它要管理员
    let from_root = if chain.first().is_some_and(|p| p.parent().is_none()) {
        1
    } else {
        0
    };
    for a in chain.into_iter().skip(from_root) {
        if !a.is_dir() {
            continue;
        }
        // SAFETY: sid 来自调用方刚拿到的容器 SID，活过整个循环
        match unsafe { grant_to_container(a, sid, INHERIT_NONE, LIST_ONLY) } {
            Ok(true) => tracing::info!(
                dir = %a.display(),
                "给工作区的上级目录加了「可列举」—— git 的 getcwd 需要它。容器由此能看见这一层有哪些名字，但读不到其中任何一个文件"
            ),
            Ok(false) => {}
            // ⚠️ **一断即止，不是「跳过继续」。**
            //
            // 祖先链是一条**链**：中间少一级，`GetLongPathNameW` 就整条失败，
            // 下面授得再多也没用。而「授了却没用」不是白干，是**白付代价** ——
            // 每一级列举权都让容器多看见一层文件名。
            //
            // 这个错误真实发生过：`D:/codes` 归一个解析不出名字的 SID，
            // 授不上；旧写法继续往下授，于是这台机器的**主目录**被加上了
            // 列举权（`.ssh`、`.git-credentials` 的文件名都露出来了），
            // 而 git 依然一步都走不了。
            Err(e) => {
                tracing::warn!(
                    dir = %a.display(),
                    error = %e,
                    "这一级的权限表改不动（多半不归当前用户），                     沙箱里的 git 用不了 —— 更下面几级就不授了，授了也没用"
                );
                return Ok(());
            }
        }
    }
    Ok(())
}

/// 把工作区授给容器 —— 在**父进程**里做，理由见 `prepare` 里那段。
///
/// 每次执行都调，靠 `already_granted` 短路：ACL 住在磁盘上，跨进程跨重启
/// 都在，所以第二条命令开始这里就是一次读权限表的开销。
fn grant_workspace(policy: &SandboxPolicy, cwd: &Path) -> Result<Option<std::path::PathBuf>> {
    let dirs = writable_under_cwd(policy, cwd);
    // SAFETY: 同上，指针来自刚拿到的 SID 与栈上缓冲
    unsafe {
        let sid = container_sid()?;
        let mut err = None;
        // 祖先链先授 —— 没有它 git 一步都走不了，见 `grant_ancestors`
        let _ = grant_ancestors(cwd, sid);
        let git_cfg = synth_git_config(sid);
        for dir in &dirs {
            // ⚠️ **这里可能要几分钟。**
            //
            // 只在第一次绑定一个**已经建过**的大仓库时发生（实测 793k 文件
            // 的仓库 153.8 秒），之后 `already_granted` 直接短路。但那几分钟
            // 里外面看不到任何动静 —— 模型在等一条 `type a.txt`，用户在等
            // 模型。所以真的要写的时候吼一声，别让它成为一段无法解释的停顿。
            let t = std::time::Instant::now();
            match grant_to_container(Path::new(dir), sid, INHERIT_ALL, FILE_ALL_ACCESS) {
                Ok(true) => tracing::info!(
                    dir = %dir,
                    elapsed_ms = t.elapsed().as_millis(),
                    "首次为工作区授权沙箱访问（跨重启只做一次；大仓库要几分钟，因为 Windows 会把权限铺满整棵子树）"
                ),
                Ok(false) => {}
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        FreeSid(sid);
        match err {
            Some(e) => Err(e),
            None => Ok(git_cfg),
        }
    }
}

/// 拼命令行。**只在需要时加引号**，并按 Windows 的规则转义。
///
/// # 为什么不能一律加引号
///
/// 第一版是「每个参数都套一对引号」，看着更安全，实际当场炸：
/// `cmd.exe /c exit 0` 变成 `"cmd.exe" "/c" "exit" "0"`，而 `cmd` 把
/// `"exit"` 当成了程序名 —— 报「'"exit"' 不是内部或外部命令」。
///
/// `cmd.exe` 不走 `CommandLineToArgvW`，它有自己的一套解析；多余的引号
/// 对它是有意义的字符。所以规则是**含空格、制表符或引号才加引号**。
///
/// # 反斜杠那条规则
///
/// 引号前面的反斜杠要成对翻倍（`a\` → `"a\\"`），否则末尾那个反斜杠会
/// 把收尾的引号转义掉，命令行就此串行。这是 `CommandLineToArgvW` 的
/// 规定，多数命令行解析器（含 Rust 自己的 `Command`）都照它来。
fn command_line(argv: &[String]) -> String {
    // `cmd.exe /C <整条命令>`：`/C` 后面那段**原样**拼进去，一个引号都不加。
    // 理由与 [`crate::sandbox::command_from_argv`] 完全相同 —— cmd 不认
    // `CommandLineToArgvW` 那套转义，替它加引号只会把命令弄坏。
    //
    // 这一条是被沙箱的逃逸测试逼出来的：加引号的那一版里，「读不到用户目录」
    // 那条测试**把沙箱整个关掉也照样绿** —— 因为命令压根没跑起来
    if let [prog, flag, rest] = argv
        && flag.eq_ignore_ascii_case("/c")
    {
        return format!("{} {flag} {rest}", quote_arg(prog));
    }
    argv.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                out.push(ch);
            }
            '"' => {
                // 引号前的反斜杠翻倍，再转义这个引号本身
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push('\\');
                out.push('"');
            }
            _ => {
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    // 收尾引号前的反斜杠也要翻倍，否则它会把那个引号转义掉
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

unsafe fn spawn_in_container(
    sid: *mut c_void,
    argv: &[String],
    cwd: &str,
    network: bool,
) -> Result<u32> {
    // **网络就是这一个 capability。** 实测（2026-08-28，同一条命令两跑）：
    //
    // | | `curl -m 8 https://example.com` | DNS |
    // |---|---|---|
    // | 不给 | 退出码 6 —— 域名都解析不出来 | 失败 |
    // | 给 `internetClient` | **200** | 通 |
    //
    // 所以这一层的网络是个**干净的二元开关**，与 `NetworkPolicy` 一一对应。
    // 上一版把「默认没有 capability 于是恰好断网」写成副作用，那是对的
    // 描述但不是设计；现在它是设计。
    //
    // ⚠️ **只有全开与全关两档，没有白名单。** Linux 那侧的
    // `NetworkPolicy::Allowed` 走 `cortex-egress-proxy` 的 CONNECT 白名单，
    // 这边一旦放开就是整个互联网。**两边的 `Allowed` 不是同一个东西** ——
    // 谁把它们当成一回事，谁就会以为工作区里的东西发不出去。
    //
    // 顺带记一笔：loopback 仍然断着（AppContainer 的老规矩），所以沙箱里
    // 的命令连不上本机的开发服务器 —— 那是这一层免费送的，不是我们设的。
    let mut internet_client: *mut c_void = std::ptr::null_mut();
    let mut cap_attrs: Vec<SID_AND_ATTRIBUTES> = Vec::new();
    if network {
        let w = wide(INTERNET_CLIENT_SID);
        if ConvertStringSidToSidW(w.as_ptr(), &mut internet_client) != 0 {
            cap_attrs.push(SID_AND_ATTRIBUTES {
                Sid: internet_client,
                // SE_GROUP_ENABLED —— 不置位的话它在令牌里但不生效
                Attributes: 4,
            });
        }
    }
    let mut caps = SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: if cap_attrs.is_empty() {
            std::ptr::null_mut()
        } else {
            cap_attrs.as_mut_ptr()
        },
        CapabilityCount: u32::try_from(cap_attrs.len()).unwrap_or(0),
        Reserved: 0,
    };

    let mut size: usize = 0;
    InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
    let mut buf = vec![0u8; size];
    let attrs: LPPROC_THREAD_ATTRIBUTE_LIST = buf.as_mut_ptr().cast();
    if InitializeProcThreadAttributeList(attrs, 1, 0, &mut size) == 0 {
        return Err(last_error("InitializeProcThreadAttributeList"));
    }

    // PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES
    const ATTR_SECURITY_CAPABILITIES: usize = 0x0002_0009;
    if UpdateProcThreadAttribute(
        attrs,
        0,
        ATTR_SECURITY_CAPABILITIES,
        std::ptr::from_mut(&mut caps).cast::<c_void>(),
        std::mem::size_of::<SECURITY_CAPABILITIES>(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    ) == 0
    {
        DeleteProcThreadAttributeList(attrs);
        return Err(last_error("UpdateProcThreadAttribute"));
    }

    let mut si: STARTUPINFOEXW = std::mem::zeroed();
    si.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).unwrap_or(0);
    si.lpAttributeList = attrs;
    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
    let mut cl = wide(&command_line(argv));
    let cwd_w = wide(&strip_verbatim(cwd));

    // `bInheritHandles = TRUE`：本进程的 stdin/stdout/stderr 就是父进程
    // 接过来的那三个管道，真命令再继承一层，于是输出原样通到底 ——
    // 不必自己铺管道（codex 那套铺了，因为它还要做别的事）
    let ok = CreateProcessW(
        std::ptr::null(),
        cl.as_mut_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        1,
        EXTENDED_STARTUPINFO_PRESENT,
        std::ptr::null(),
        cwd_w.as_ptr(),
        std::ptr::from_ref(&si).cast(),
        &mut pi,
    );
    if ok == 0 {
        let e = last_error("CreateProcessW");
        DeleteProcThreadAttributeList(attrs);
        return Err(e);
    }

    WaitForSingleObject(pi.hProcess, INFINITE);
    let mut code: u32 = 0;
    GetExitCodeProcess(pi.hProcess, &mut code);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    DeleteProcThreadAttributeList(attrs);
    if !internet_client.is_null() {
        LocalFree(internet_client);
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **只授工作区子树。**
    ///
    /// 这一条同时是性能闸和边界闸：`SandboxPolicy` 无条件把
    /// `~/.cargo/registry`、`~/.cache`、`~/.gradle/caches` 这些放进可写根
    /// （Landlock 下不要钱），而在 Windows 上每一个都是一次**持久的、递归的**
    /// ACL 重写 —— 实测合计 23.4 秒，且沙箱退出之后那份权限还留在用户的
    /// cargo 仓库上。放开这一条不会有任何测试红，只会让每条命令慢二十几秒、
    /// 同时在宿主机上悄悄放宽权限。
    #[test]
    fn 用户目录下的缓存不会被授出去() {
        let dir = tempfile::tempdir().expect("临时目录");
        let policy = SandboxPolicy::workspace(dir.path());
        let got = writable_under_cwd(&policy, dir.path());

        let base = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|_| dir.path().to_path_buf());
        let base = strip_verbatim(&base.display().to_string());
        for p in &got {
            assert!(
                p.starts_with(&base),
                "授出去了工作区以外的目录：{p}（工作区是 {base}）"
            );
        }
        assert!(
            !got.is_empty(),
            "工作区本身必须在里面，否则沙箱里什么都写不了"
        );
    }

    /// 授权前后 [`already_granted`] 的回答必须翻面。
    ///
    /// 它是「首次 153.8 秒、之后 201 毫秒」里的那个 201 毫秒 —— 但**不能靠
    /// 计时来测**（那种测试在忙的机器上必红）。测的是它的语义：没授过时说
    /// 「没有」，授过之后说「有」。
    ///
    /// 两个方向都要测。只测「授过之后说有」的话，一个永远回 `true` 的实现
    /// 也能过，而那意味着第一次就跳过授权 —— 沙箱里什么都读不了。
    #[test]
    fn 授权前后的判断会翻面() {
        let dir = tempfile::tempdir().expect("临时目录");
        // SAFETY: SID 与 DACL 指针都在本作用域内使用，用完就释放
        unsafe {
            let sid = container_sid().expect("拿得到容器 SID");
            assert!(
                !read_dacl_and_check(dir.path(), sid, INHERIT_ALL, FILE_ALL_ACCESS),
                "刚建的目录不该被认为已经授过"
            );
            let 第一次 = grant_to_container(dir.path(), sid, INHERIT_ALL, FILE_ALL_ACCESS)
                .expect("授得下去");
            assert!(第一次, "第一次必须真的写下去");

            assert!(
                read_dacl_and_check(dir.path(), sid, INHERIT_ALL, FILE_ALL_ACCESS),
                "授过之后必须认得出来，否则每条命令都要重铺一遍整棵子树"
            );
            // 回值就是「这次是不是真的写了」—— 那条「首次授权可能要几分钟」
            // 的日志靠它分辨该不该吼。恒真的话每条命令都吼一遍，
            // 而那种日志三天之内就会被人当噪音略过
            let 第二次 = grant_to_container(dir.path(), sid, INHERIT_ALL, FILE_ALL_ACCESS)
                .expect("再授一次不该失败");
            assert!(!第二次, "第二次不该再写 —— 那意味着又铺了一遍整棵子树");
            FreeSid(sid);
        }
    }

    /// 只给上面那条测试用：读一次 DACL 再问 [`already_granted`]。
    unsafe fn read_dacl_and_check(dir: &Path, sid: *mut c_void, inherit: u32, mask: u32) -> bool {
        use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
        let path = wide(&dir.display().to_string());
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        assert_eq!(rc, 0, "读不到权限表（错误 {rc}）");
        unsafe { already_granted(dacl, sid, inherit, mask) }
    }

    /// **没有继承标志的 ACE 不算「已经授过」。**
    ///
    /// 这是 [`already_granted`] 里那句 `AceFlags & 3 != 3` 的存在理由。
    /// 只在目录本身有权限、子项没有的话，命令能 `cd` 进去却读不了里面任何
    /// 文件；而如果这里只比 SID，就会把这种目录当成已完成、再也不补授 ——
    /// 症状是「目录在，文件读不到」，离真正的原因非常远。
    ///
    /// 这种目录不是假想的：用户自己 `icacls` 过（不带 `/T`）、或早一版的
    /// 本代码授的，都长这样。
    #[test]
    fn 没有继承标志的授权不算数() {
        let dir = tempfile::tempdir().expect("临时目录");
        // SAFETY: SID 与 DACL 指针都在本作用域内使用，用完就释放
        unsafe {
            let sid = container_sid().expect("拿得到容器 SID");
            grant_raw(dir.path(), sid, 0, FILE_ALL_ACCESS);
            assert!(
                !read_dacl_and_check(dir.path(), sid, INHERIT_ALL, FILE_ALL_ACCESS),
                "只授了目录本身、没授子项，不能算「已经授过」                 —— 认了的话里面的文件永远读不到"
            );
            FreeSid(sid);
        }
    }

    /// **权限不够的 ACE 也不算「已经授过」。**
    ///
    /// 对应 [`already_granted`] 里那句 `Mask & FILE_ALL_ACCESS != FILE_ALL_ACCESS`。
    /// 一条只读的 `(OI)(CI)R` 满足「SID 对得上、继承标志齐」，但沙箱里的
    /// 命令写不了任何文件；把它当成已完成的话，`echo x > a.txt` 会一直失败
    /// 而权限表看上去「已经授过了」。
    #[test]
    fn 只读的授权不算数() {
        let dir = tempfile::tempdir().expect("临时目录");
        // SAFETY: SID 与 DACL 指针都在本作用域内使用，用完就释放
        unsafe {
            let sid = container_sid().expect("拿得到容器 SID");
            // FILE_GENERIC_READ | FILE_GENERIC_EXECUTE，没有写
            grant_raw(dir.path(), sid, 3, 0x0012_01BF);
            assert!(
                !read_dacl_and_check(dir.path(), sid, INHERIT_ALL, FILE_ALL_ACCESS),
                "只读的 ACE 不能算「已经授过」——沙箱里的命令要写得进工作区"
            );
            FreeSid(sid);
        }
    }

    /// 只给上面那条测试用：按指定的继承标志写一条 ACE。
    ///
    /// 不把 `grant_to_container` 改成带参数的：那会在生产代码里留一个
    /// 只有测试会传别的值的旋钮，而这里要的恰恰是「生产代码只会写
    /// `3` 与 `FILE_ALL_ACCESS`，别的组合都是外面来的」。
    unsafe fn grant_raw(dir: &Path, sid: *mut c_void, inherit: u32, mask: u32) {
        use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
        let mut path = wide(&dir.display().to_string());
        let mut old: *mut ACL = std::ptr::null_mut();
        let mut sd: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut old,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        assert_eq!(rc, 0, "读不到权限表（错误 {rc}）");
        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: mask,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: inherit,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: sid.cast::<u16>(),
            },
        };
        let mut new: *mut ACL = std::ptr::null_mut();
        let rc = unsafe { SetEntriesInAclW(1, &ea, old, &mut new) };
        assert_eq!(rc, 0, "合并权限表失败（错误 {rc}）");
        let rc = unsafe {
            SetNamedSecurityInfoW(
                path.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "写不回权限表（错误 {rc}）");
    }

    /// **`cmd /C` 后面那段一个引号都不加。**
    ///
    /// 加了的话（第一版就是）Windows 上任何带引号路径的命令都跑不起来，
    /// 而那在 Windows 上是常态。更糟的是它**不会让任何测试变红** ——
    /// 逃逸测试反而会因为「命令根本没跑」而绿得更彻底。
    #[test]
    fn cmd后面的命令原样传() {
        let line = command_line(&[
            "cmd.exe".into(),
            "/C".into(),
            r#"type "C:\a b\c.txt""#.into(),
        ]);
        assert_eq!(
            line, r#"cmd.exe /C type "C:\a b\c.txt""#,
            "cmd 不认 `\"` 这种转义，替它加引号会把命令弄坏：{line}"
        );
    }

    /// 带空格的路径必须被引起来 —— Windows 上「Program Files」到处都是。
    #[test]
    fn 带空格的参数会被引起来() {
        let line = command_line(&[
            "C:\\Program Files\\Git\\bin\\git.exe".into(),
            "status".into(),
        ]);
        assert!(
            line.starts_with('"') && line.contains("Program Files"),
            "路径没被引起来，会被当成两个参数：{line}"
        );
        assert!(
            line.ends_with(" status"),
            "不含空格的参数不该被加引号：{line}"
        );
    }

    /// **不含空格的参数一律不加引号。**
    ///
    /// 一律加引号的第一版当场炸了：`cmd.exe /c exit 0` 变成
    /// `"cmd.exe" "/c" "exit" "0"`，而 `cmd` 把 `"exit"` 当成了程序名。
    /// `cmd` 不走 `CommandLineToArgvW`，多余的引号对它是有意义的字符。
    #[test]
    fn 不含空格的参数不加引号() {
        let line = command_line(&["cmd.exe".into(), "/c".into(), "exit".into(), "0".into()]);
        assert_eq!(
            line, "cmd.exe /c exit 0",
            "多余的引号会让 cmd 解析错：{line}"
        );
    }

    /// 参数里自带引号时要转义，否则命令行提前结束。
    #[test]
    fn 参数里的引号会被转义() {
        let line = command_line(&["echo".into(), "a\"b".into()]);
        assert!(line.contains("a\\\"b"), "引号没转义：{line}");
    }

    /// 收尾引号前的反斜杠要翻倍，否则它把那个引号转义掉、命令行串行。
    #[test]
    fn 结尾的反斜杠会被翻倍() {
        let line = command_line(&["x".into(), "C:\\path with space\\".into()]);
        assert!(
            line.ends_with("\\\\\""),
            "结尾反斜杠没翻倍，收尾引号会被它吃掉：{line}"
        );
    }
}
