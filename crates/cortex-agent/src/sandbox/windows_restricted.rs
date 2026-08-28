//! Windows 第二后端：**受限令牌（restricted token）**。
//!
//! # 为什么要第二个后端
//!
//! AppContainer（[`super::windows`]）在文件读与网络上是**默认拒绝**的强边界，
//! 但它跑不了完整的工具链：`cargo build` 的链接步在容器里挂
//! （MSVC `link.exe` 的 `0xC0000142`、`rust-lld` 从主目录树 mmap 自身镜像被拒），
//! 而这两个都是 AppContainer LowBox 令牌对对象命名空间的隔离所致，ACL 修不了。
//! 实测：AppContainer 里 `cargo check` 通、`cargo build` 不通。
//!
//! 受限令牌是另一种取舍，也是 OpenAI codex 在 Windows 上唯一用的机制：
//!
//! | | AppContainer | 受限令牌 |
//! |---|---|---|
//! | 读 | 默认拒绝（allow-list，强） | **不设界**（读什么都能读，等同用户本人） |
//! | 写 | 只有工作区 | 只有工作区（`WRITE_RESTRICTED` + 一个只在工作区被授的 SID） |
//! | `cargo build` 出 .exe | ✗ | **✓** |
//! | 网络 | 一个 capability，默认断 | **未隔离，且 HTTPS 实际不可用**（见下） |
//!
//! **读边界基本不存在，必须如实标注**（CLAUDE.md 约束 2）。
//!
//! `WRITE_RESTRICTED` 只让 restricting SID 参与**写**检查；**读**走普通令牌，
//! 而沙箱进程的读身份就是**用户本人**——没有能区分「沙箱 vs 用户」的 SID，
//! 于是无法只对沙箱拒读而不误伤用户（试过给秘密目录下 Everyone-DENY，
//! 结果把用户自己也挡了）。所以这个后端**不挡读**：能读用户能读的一切。
//!
//! 这划定了它的用途：**「防误写/防篡改地跑我信得过的工具链」**，不是
//! 「关不受信代码防它偷秘密」。要防偷读，用 AppContainer（读默认拒绝）——
//! 但它跑不了完整工具链。两者不可兼得，这是 Windows 原生沙箱的结构现实。
//!
//! # ⚠️ 网络：不隔离，而且 HTTPS 是坏的
//!
//! 实测（2026-08-28）：
//!
//! **根因（已定位）**：受限令牌下**证书系统存储只能只读打开**。
//! `CertOpenSystemStore`（读写）回 `ERROR_ACCESS_DENIED`，而同一个存储加
//! `CERT_STORE_READONLY_FLAG` 开得开 —— 而 schannel 内部按读写开，于是
//! `AcquireCredentialsHandle` 回 `SEC_E_NO_CREDENTIALS`。
//!
//! 这不是我们漏授：证书存储的注册表键上明写着
//! `NT AUTHORITY\RESTRICTED: ReadKey` —— **Windows 对受限令牌的既定语义**。
//! （试过把用户 SID 加进令牌默认 DACL，没用，已回滚。）
//!
//! **修法：不去要写权限，而是让程序绕开 schannel。** 现状：
//!
//! | | 结果 |
//! |---|---|
//! | `git`（`clone` / `fetch` / `push` / `ls-remote`） | ✅ **通** —— 注入 `http.sslBackend=openssl`，走 git 自带 ca-bundle |
//! | `cargo` 更新索引 | ✅ 通 —— `CARGO_NET_GIT_FETCH_WITH_CLI` + 索引走 git 协议 |
//! | `cargo` 下载 `.crate` | ❌ 仍挂 —— 走 cargo 内建 libcurl（只编了 Schannel），没有开关能换 |
//! | `curl https://` | ❌ 仍挂 —— 这个构建只编了 Schannel |
//! | `curl http://`（明文） | 通 |
//!
//! 规律是：**凡是能改走 git CLI 的都修好了；凡是进程自己链了 Schannel 的
//! 都换不掉**。给它们 CA 文件（`--cacert` / `CARGO_HTTP_CAINFO`）没用 ——
//! 那只换验证用的根，不换 TLS 后端。
//!
//! 所以这一档**不构成网络边界**（明文出得去），但可用性上已经能干活：
//! git 全通，依赖在本地缓存时构建正常，只有「拉新依赖」要在沙箱外先做。
//!
//! **零管理员做不到真封网**，这一点也实测过：`FwpmEngineOpen0` 能开（只读），
//! 但 `FwpmTransactionBegin0` 回 `ERROR_ACCESS_DENIED` —— WFP 写规则要管理员，
//! 所以 codex 的 WFP 封锁也只在它的提权档。Job object 的网络限速同样设不了
//! （`ERROR_INVALID_PARAMETER`）。要真封网，只能走一次管理员授权，或者把
//! 需要出网控制的场景交给云沙箱（那边有 `cortex-egress-proxy` 的白名单）。
//!
//! # 机制：`WRITE_RESTRICTED` 受限令牌 + 四件套
//!
//! `CreateRestrictedToken(DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED)`
//! 造一个令牌：**写**检查要求既过普通 DACL、又匹配一个 restricting SID；
//! **读**检查只走普通 DACL（所以读不受限）。
//!
//! restricting SID 列表是 `[logon SID, Everyone]`：
//!
//! - **logon SID 是写作用域的锚** —— 父进程只把**工作区**授给它，
//!   所以「有意的写」只落工作区。
//! - **Everyone 是不得不放的一环**：`link.exe`（带 `/DEBUG`）要在会话的
//!   `BaseNamedObjects` 里建命名对象跑 mspdbsrv IPC，那里授的是 Everyone。
//!   去掉它，`cargo build` 的链接步当场挂（实测）。代价是 **Everyone 可写的
//!   目录也写得进**（临时目录一类）—— 这是换「能跑工具链」付的账。
//!
//! 光有令牌不够：裸受限令牌起 `node`/`powershell`/`link.exe` 会 `0xC0000142`
//! （`STATUS_DLL_INIT_FAILED`）。codex 用「四件套」解掉，本模块照搬：
//!
//! 1. **令牌的默认 DACL** 设成 `[logon, Everyone]` 全权 —— 进程建的管道 /
//!    section 对象自己够得着。**这一件才是修 `0xC0000142` 的那个**：
//!    故障注入验证过，去掉它 `cargo build` 立刻挂；下面第 2 件去掉却不影响。
//! 2. 建一个**私有桌面** `CreateDesktopW`，把 logon SID 授上
//!    `DESKTOP_ALL_ACCESS`。它买的是**隔离**（沙箱里的进程看不见、也动不了
//!    你桌面上的窗口，挡屏幕抓取与输入注入），不是启动必需。
//! 3. 起进程时 `STARTUPINFOW.lpDesktop` 指向那个私有桌面。
//! 4. 重开 `SeChangeNotifyPrivilege`（`DISABLE_MAX_PRIVILEGE` 把它连同
//!    bypass-traverse 一起剥了；不重开，穿目录到处要显式授）。
//!
//! `%TEMP%` 也要重定向进工作区：真 `%TEMP%` 授的是用户本人、不是 logon SID，
//! 写不进，而 rustc / link 到处用它。
//!
//! # 为什么也要一个 helper 进程
//!
//! 与 AppContainer 同理：`std::process::Command` 不能用 `CreateProcessAsUserW`
//! 起进程，所以 [`super::prepare`] 回的是「本程序 + `--win-restricted-exec`」，
//! 由 [`exec_restricted`] 在那个模式下建令牌、建桌面、`CreateProcessAsUserW`。

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::path::Path;

use cortex_core::{CortexError, Result};

use super::policy::{NetworkPolicy, SandboxPolicy};
use super::windows::{
    FILE_ALL_ACCESS, INHERIT_ALL, grant_to_container, helper_path, strip_verbatim, wide,
    writable_under_cwd,
};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, SE_WINDOW_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, CopySid, CreateRestrictedToken, DACL_SECURITY_INFORMATION, GetLengthSid,
    GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ACCESS_MASK, TOKEN_PRIVILEGES, TokenDefaultDacl,
    TokenGroups,
};
use windows_sys::Win32::System::StationsAndDesktops::HDESK;
use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, CreateDesktopW};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken,
    PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW, WaitForSingleObject,
};

const DISABLE_MAX_PRIVILEGE: u32 = 0x01;
const LUA_TOKEN: u32 = 0x04;
const WRITE_RESTRICTED: u32 = 0x08;

/// `SE_GROUP_LOGON_ID`。
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;

/// 世界（Everyone）SID 的字符串形式。
const WORLD_SID: &str = "S-1-1-0";

/// 私有桌面名字前缀。每次随机后缀，避免撞名 —— 但**不能用随机数**
/// （`Math.random` 类不可用），用进程 id + 单调计数拼。
const DESKTOP_PREFIX: &str = "CortexSandboxDesktop-";

pub(super) const HELPER_FLAG: &str = "--win-restricted-exec";

/// helper 收到的计划。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct RestrictedPlan {
    argv: Vec<String>,
    cwd: String,
    #[serde(default)]
    network: bool,
}

/// 探测：这台机器上受限令牌后端能不能用。
///
/// **真的建一次令牌并起一个最短命令**，不是只看 API 在不在 —— 与
/// AppContainer 那侧同一条纪律：只查 API 会在 helper 坏掉时报「可用」，
/// 而那会让逃逸测试退化成「验证默认拒绝」。
pub fn detect() -> super::Capability {
    use super::{Backend, Capability};
    let helper = match helper_path() {
        Ok(p) => p,
        Err(e) => {
            return Capability::Unavailable {
                reason: e.to_string(),
            };
        }
    };
    let probe = RestrictedPlan {
        argv: vec!["cmd.exe".into(), "/c".into(), "exit".into(), "0".into()],
        cwd: std::env::temp_dir().display().to_string(),
        network: false,
    };
    let Ok(json) = serde_json::to_string(&probe) else {
        return Capability::Unavailable {
            reason: "装配受限令牌探针失败".into(),
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
            backend: Backend::RestrictedToken,
            detail: "受限令牌 · 写只限工作区；**不挡读**（读身份即用户本人）；网络与桌面未隔离"
                .into(),
        },
        Ok(out) => Capability::Unavailable {
            reason: format!(
                "受限令牌探针没跑通（helper={}，退出码 {:?}）：{}",
                helper.display(),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        },
        Err(e) => Capability::Unavailable {
            reason: format!("起不来受限令牌 helper（{}）：{e}", helper.display()),
        },
    }
}

/// 装配一条待执行的命令（父进程侧）。
///
/// 在父进程里把工作区授给 [`WRITE_SCOPE_SID`]、对秘密目录下 DENY，然后回
/// 「本程序 + [`HELPER_FLAG`] + JSON」。
pub(super) fn prepare(
    policy: &SandboxPolicy,
    argv: &[String],
    cwd: &Path,
) -> Result<std::process::Command> {
    grant_workspace_write(policy, cwd)?;

    let plan = RestrictedPlan {
        argv: argv.to_vec(),
        cwd: strip_verbatim(&cwd.display().to_string()),
        network: matches!(policy.network, NetworkPolicy::Allowed),
    };
    let json = serde_json::to_string(&plan)
        .map_err(|e| CortexError::Invalid(format!("装配受限令牌计划失败：{e}")))?;

    let me = helper_path()?;
    let mut cmd = std::process::Command::new(me);
    cmd.arg(HELPER_FLAG).arg(json);
    // CARGO_HOME 在父进程这一侧定：镜像要开在长命的进程里（helper 每条命令
    // 起一次，让它持有端口的话，先结束的那条会把还在下载的那条掐断），
    // 而 ACL 与配置文件也只该写一次。helper 把环境原样传给子进程。
    if let Some(home) = setup_cargo_home() {
        cmd.env("CARGO_HOME", home);
    }
    // 同理在父进程侧备 curl（下载与校验只该做一次，helper 每条命令一个）。
    // PATH 前缀让沙箱里裸敲的 `curl` 解析到 LibreSSL 版；CURL_CA_BUNDLE
    // 必须一起给 —— 这份构建默认走 Windows 证书存储验证（NativeCA），
    // 而那条路在沙箱里走不通，握手能过、验证挂在 local issuer 上。
    if let Some(bin) = super::windows_curl::ensure_curl() {
        if let Some(path) = std::env::var_os("PATH") {
            let mut joined = std::ffi::OsString::from(bin);
            joined.push(";");
            joined.push(path);
            cmd.env("PATH", joined);
        }
        cmd.env("CURL_CA_BUNDLE", bin.join("curl-ca-bundle.crt"));
    }
    Ok(cmd)
}

/// 沙箱专用 `CARGO_HOME` 的路径。**只算路径，不建目录、不授权。**
///
/// 单独拿出来是给测试用的：要证明「新依赖真的下载得下来」，就得先把这份
/// 缓存清掉，而测试里再写一遍这个路径就是两份会各自漂的常量。
#[must_use]
pub fn sandbox_cargo_home() -> Option<std::path::PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    Some(
        Path::new(&local)
            .join("Cortex")
            .join("win-sandbox")
            .join("cargo-home"),
    )
}

/// 备好一个**沙箱写得进**的 `CARGO_HOME`，并把 crates.io 指到本机镜像。
///
/// # 为什么必须换掉 CARGO_HOME
///
/// 用户真实的 `~/.cargo` 沙箱写不进（写只授了工作区），而 cargo 第一件事
/// 就是往 `registry/index/…` 里建目录。**这一步比 TLS 更靠前**：实测哪怕
/// 依赖全在缓存里，默认 `CARGO_HOME` 下也会当场
/// 「failed to create directory … 拒绝访问 (os error 5)」。
///
/// 那为什么不干脆把 `~/.cargo` 授给沙箱写？因为沙箱里的代码就能往用户的
/// crate 缓存里塞东西，而**下一次不在沙箱里的构建会照单编译它** ——
/// 那是一条真正的逃逸路径，不是洁癖。
///
/// # 为什么授的是 Everyone
///
/// `WRITE_RESTRICTED` 要求目标 DACL 授了某个 restricting SID，可选的只有
/// logon SID 与 Everyone。工作区那边用 logon SID（作用域正好是一次登录），
/// 这里不行：这个目录**跨登录会话长期存在**，按 logon SID 授会每登录一次
/// 攒一条永远不再匹配的 ACE。
///
/// 目录放在 `%LOCALAPPDATA%` 底下，靠父目录的 DACL 把别的**普通用户**挡在
/// 外面 —— 所以位置不能随便挪。
///
/// ⚠️ **这一条只到「普通用户」为止，别把它当成强边界。** 实测这台机器上
/// `%LOCALAPPDATA%` 还带着一条 `CodexSandboxUsers:(I)(OI)(CI)(RX)`（用户装的
/// codex 建的本地组），也就是说同机另一套沙箱够得到这里，而我们这条 Everyone
/// 让它还写得进。兜底是 cargo 自己的校验和：`.crate` 的 sha256 来自索引、
/// 有锁文件时来自锁文件，塞进来的东西过不了那一关。
///
/// 之所以仍然选它，是因为另外两条更差：授给用户真实的 `~/.cargo` 等于让沙箱
/// 污染**沙箱外**的构建（真正的逃逸）；按 logon SID 授则每登录一次攒一条
/// 永不再匹配的 ACE。
///
/// 出任何岔子都回 `None`：那时行为退回改动之前（cargo 照旧报它自己的错），
/// 而不是把命令拦下来 —— 绝大多数命令根本不碰 cargo。
fn setup_cargo_home() -> Option<std::path::PathBuf> {
    let home = sandbox_cargo_home()?;
    if let Err(e) = std::fs::create_dir_all(&home) {
        tracing::warn!(error = %e, "建不出沙箱专用的 CARGO_HOME");
        return None;
    }
    // SAFETY: SID 由 ConvertStringSidToSidW 造，用完 LocalFree；
    // grant_to_container 是已验证的封装，幂等（授过就不再写）。
    unsafe {
        let sid = sid_from_string(WORLD_SID).ok()?;
        let r = grant_to_container(&home, sid, INHERIT_ALL, FILE_ALL_ACCESS);
        LocalFree(sid.cast());
        if let Err(e) = r {
            tracing::warn!(error = %e, "沙箱专用 CARGO_HOME 授权失败");
            return None;
        }
    }

    // 镜像起不来（或端口被一个不是我们的服务占着）时，**不写那段配置**：
    // 指向一个来路不明的 registry 比下载失败危险得多。
    let cfg = home.join("config.toml");
    match super::windows_cargo_mirror::ensure_running() {
        Ok(true) => {
            let body = format!(
                concat!(
                    "# 这个文件由 Cortex 的 Windows 受限令牌沙箱生成，每次执行都会重写。\n",
                    "# 手工改它没有意义。为什么要有它，见 sandbox::windows_cargo_mirror。\n",
                    "[source.crates-io]\n",
                    "replace-with = \"cortex-mirror\"\n",
                    "\n",
                    "[source.cortex-mirror]\n",
                    "registry = \"{}\"\n",
                ),
                super::windows_cargo_mirror::registry_url()
            );
            if let Err(e) = std::fs::write(&cfg, body) {
                tracing::warn!(error = %e, "写不出沙箱的 cargo 配置");
            }
        }
        _ => {
            tracing::warn!("cargo 的回环镜像不可用，沙箱里拉新依赖会失败");
            let _ = std::fs::remove_file(&cfg);
        }
    }
    Some(home)
}

/// 把工作区可写子树授给**写作用域 SID = 当前登录会话的 logon SID**。
///
/// # 为什么是 logon SID
///
/// 受限令牌的写检查（`WRITE_RESTRICTED`）要求目标 DACL 授了某个 restricting
/// SID。父进程在这里把工作区授给 logon SID，helper 又把 logon SID 放进
/// restricting 列表 —— 于是进程**只**写得了工作区。两处是同一个 SID，
/// 因为 helper 是父进程在**同一个登录会话**里起的，logon SID 一致。
///
/// 不用 Everyone（虽然也是有效的 restricting SID）：那会让「任何 Everyone
/// 可写的目录」都能写。也不用 AppContainer 那个包 SID（S-1-15-2）——
/// 它**不被接受为 restricting SID**（`CreateRestrictedToken` 回 87）。
fn grant_workspace_write(policy: &SandboxPolicy, cwd: &Path) -> Result<()> {
    let dirs = writable_under_cwd(policy, cwd);
    // SAFETY: logon SID 拷贝随 Vec 生命周期；grant_to_container 是已验证的封装
    unsafe {
        let mut logon = current_logon_sid()?;
        let sid = logon.as_mut_ptr().cast::<c_void>();
        for dir in &dirs {
            grant_to_container(Path::new(dir), sid, INHERIT_ALL, FILE_ALL_ACCESS)?;
        }
        Ok(())
    }
}

/// 开当前进程令牌，取 logon SID（父进程侧用）。
unsafe fn current_logon_sid() -> Result<Vec<u8>> {
    const TOKEN_QUERY: u32 = 0x0008;
    let mut token: HANDLE = std::ptr::null_mut();
    if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
        return Err(CortexError::Invalid(format!(
            "OpenProcessToken(父) 失败：{}",
            GetLastError()
        )));
    }
    let r = logon_sid_bytes(token);
    CloseHandle(token);
    r
}

/// helper 模式主体：建令牌 + 桌面，`CreateProcessAsUserW` 起真命令。**不返回。**
pub fn exec_restricted(plan_json: &str) -> ! {
    let code = match run(plan_json) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("受限令牌沙箱起不来：{e}");
            127
        }
    };
    std::process::exit(i32::try_from(code).unwrap_or(1));
}

fn run(plan_json: &str) -> Result<u32> {
    let plan: RestrictedPlan = serde_json::from_str(plan_json)
        .map_err(|e| CortexError::Invalid(format!("读不懂受限令牌计划：{e}")))?;
    let _ = plan.network; // 本版不做网络隔离，占位以免误以为已处理
    // SAFETY: 整段是原始 Win32，指针生命周期都在本函数栈上覆盖
    unsafe { spawn_restricted(&plan.argv, &plan.cwd) }
}

// ───────────────────────── 令牌与桌面 ─────────────────────────

/// `ConvertStringSidToSidW` 包一层。回的指针要 `LocalFree`。
unsafe fn sid_from_string(s: &str) -> Result<*mut c_void> {
    let w = wide(s);
    let mut sid: *mut c_void = std::ptr::null_mut();
    if ConvertStringSidToSidW(w.as_ptr(), &mut sid) == 0 {
        return Err(CortexError::Invalid(format!(
            "解析 SID 失败（{s}）：{}",
            GetLastError()
        )));
    }
    Ok(sid)
}

/// 从当前进程令牌里找 logon SID（带 `SE_GROUP_LOGON_ID` 属性的组）。
/// 复刻 codex 的扫描逻辑。回的 SID 是**拷贝**，随 `Vec` 生命周期。
unsafe fn logon_sid_bytes(token: HANDLE) -> Result<Vec<u8>> {
    let mut needed: u32 = 0;
    GetTokenInformation(token, TokenGroups, std::ptr::null_mut(), 0, &mut needed);
    if needed == 0 {
        return Err(CortexError::Invalid("拿不到 TokenGroups 大小".into()));
    }
    let mut buf = vec![0u8; needed as usize];
    if GetTokenInformation(
        token,
        TokenGroups,
        buf.as_mut_ptr().cast(),
        needed,
        &mut needed,
    ) == 0
    {
        return Err(CortexError::Invalid(format!(
            "GetTokenInformation(TokenGroups) 失败：{}",
            GetLastError()
        )));
    }
    let group_count = std::ptr::read_unaligned(buf.as_ptr().cast::<u32>()) as usize;
    let after_count = buf.as_ptr().add(std::mem::size_of::<u32>()) as usize;
    let align = std::mem::align_of::<SID_AND_ATTRIBUTES>();
    let aligned = (after_count + (align - 1)) & !(align - 1);
    let groups = aligned as *const SID_AND_ATTRIBUTES;
    for i in 0..group_count {
        let entry = std::ptr::read_unaligned(groups.add(i));
        if entry.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID {
            let len = GetLengthSid(entry.Sid);
            if len == 0 {
                continue;
            }
            let mut out = vec![0u8; len as usize];
            if CopySid(len, out.as_mut_ptr().cast(), entry.Sid) == 0 {
                continue;
            }
            return Ok(out);
        }
    }
    Err(CortexError::Invalid("令牌上没有 logon SID".into()))
}

/// 造受限令牌。回的 HANDLE 用完要 `CloseHandle`。
unsafe fn make_restricted_token() -> Result<(HANDLE, Vec<u8>)> {
    // 拿当前进程令牌。要够权：复制 / 查询 / 调整
    const TOKEN_DUPLICATE: u32 = 0x0002;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ASSIGN_PRIMARY: u32 = 0x0001;
    const TOKEN_ADJUST_DEFAULT: u32 = 0x0080;
    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    let access: TOKEN_ACCESS_MASK = TOKEN_DUPLICATE
        | TOKEN_QUERY
        | TOKEN_ASSIGN_PRIMARY
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_PRIVILEGES;
    let mut base: HANDLE = std::ptr::null_mut();
    if OpenProcessToken(GetCurrentProcess(), access, &mut base) == 0 {
        return Err(CortexError::Invalid(format!(
            "OpenProcessToken 失败：{}",
            GetLastError()
        )));
    }

    let mut logon = logon_sid_bytes(base)?;
    let psid_logon = logon.as_mut_ptr().cast::<c_void>();
    let world = sid_from_string(WORLD_SID)?;

    // restricting SIDs：**[logon, Everyone]**。
    //
    // logon 是写作用域的锚：工作区只授了它，所以「有意的写」只落工作区。
    // Everyone 是**不得不放**的一环：`link.exe`（带 `/DEBUG`）要在会话的
    // `BaseNamedObjects` 里建命名对象跑 mspdbsrv IPC，而建对象要往那个
    // 目录对象写，那里授的是 Everyone。去掉 Everyone，cargo build 的链接步
    // 当场挂（实测）。代价：**Everyone 可写的目录也能写**（临时目录一类）——
    // 这是受限令牌换「能跑工具链」付的账，写进模块文档，别当没有。
    let mut restricting = [
        SID_AND_ATTRIBUTES {
            Sid: psid_logon,
            Attributes: 0,
        },
        SID_AND_ATTRIBUTES {
            Sid: world,
            Attributes: 0,
        },
    ];

    let mut token: HANDLE = std::ptr::null_mut();
    let flags = DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED;
    let ok = CreateRestrictedToken(
        base,
        flags,
        0,
        std::ptr::null(),
        0,
        std::ptr::null(),
        restricting.len() as u32,
        restricting.as_mut_ptr(),
        &mut token,
    );
    CloseHandle(base);
    if ok == 0 {
        LocalFree(world.cast());
        return Err(CortexError::Invalid(format!(
            "CreateRestrictedToken 失败：{}",
            GetLastError()
        )));
    }

    // 默认 DACL = [logon, Everyone] 全权，让进程建的管道/section 对象
    // 自己够得着（PowerShell/CLR 建 IPC 要）
    set_default_dacl(token, &[psid_logon, world])?;
    enable_privilege(token, "SeChangeNotifyPrivilege")?;

    LocalFree(world.cast());
    Ok((token, logon))
}

/// 把令牌的默认 DACL 设成给这些 SID 全权。
unsafe fn set_default_dacl(token: HANDLE, sids: &[*mut c_void]) -> Result<()> {
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
        TRUSTEE_W,
    };
    const GENERIC_ALL: u32 = 0x1000_0000;
    let entries: Vec<EXPLICIT_ACCESS_W> = sids
        .iter()
        .map(|&sid| EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: 0,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.cast(),
            },
        })
        .collect();
    let mut acl = std::ptr::null_mut();
    let rc = SetEntriesInAclW(
        entries.len() as u32,
        entries.as_ptr(),
        std::ptr::null_mut(),
        &mut acl,
    );
    if rc != 0 {
        return Err(CortexError::Invalid(format!("SetEntriesInAclW 失败：{rc}")));
    }

    #[repr(C)]
    struct TokenDefaultDaclInfo {
        default_dacl: *mut c_void,
    }
    let mut info = TokenDefaultDaclInfo {
        default_dacl: acl.cast(),
    };
    let ok = SetTokenInformation(
        token,
        TokenDefaultDacl,
        std::ptr::from_mut(&mut info).cast(),
        std::mem::size_of::<TokenDefaultDaclInfo>() as u32,
    );
    if !acl.is_null() {
        LocalFree(acl.cast());
    }
    if ok == 0 {
        return Err(CortexError::Invalid(format!(
            "SetTokenInformation(TokenDefaultDacl) 失败：{}",
            GetLastError()
        )));
    }
    Ok(())
}

/// 重开一个特权。
unsafe fn enable_privilege(token: HANDLE, name: &str) -> Result<()> {
    let w = wide(name);
    let mut luid = std::mem::zeroed();
    if LookupPrivilegeValueW(std::ptr::null(), w.as_ptr(), &mut luid) == 0 {
        return Err(CortexError::Invalid(format!(
            "LookupPrivilegeValue({name}) 失败：{}",
            GetLastError()
        )));
    }
    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    if AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
        return Err(CortexError::Invalid(format!(
            "AdjustTokenPrivileges({name}) 失败：{}",
            GetLastError()
        )));
    }
    Ok(())
}

/// 建私有桌面并把 logon SID 授上全权。回 `(桌面 handle, 全名 "WinSta0\\<name>")`。
unsafe fn make_private_desktop(logon: &mut [u8]) -> Result<(HDESK, String)> {
    // DESKTOP_ALL_ACCESS 的组合位
    const DESKTOP_ALL: u32 = 0x0001 // READOBJECTS
        | 0x0002 // CREATEWINDOW
        | 0x0004 // CREATEMENU
        | 0x0008 // HOOKCONTROL
        | 0x0010 // JOURNALRECORD
        | 0x0020 // JOURNALPLAYBACK
        | 0x0040 // ENUMERATE
        | 0x0080 // WRITEOBJECTS
        | 0x0100 // SWITCHDESKTOP
        | 0x0001_0000 // DELETE
        | 0x0002_0000 // READ_CONTROL
        | 0x0004_0000 // WRITE_DAC
        | 0x0008_0000; // WRITE_OWNER
    // 名字：进程 id + 计数（不用随机数，那类 API 在本环境被禁）
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        "{DESKTOP_PREFIX}{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    let name_w = wide(&name);
    let handle = CreateDesktopW(
        name_w.as_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        0,
        DESKTOP_ALL,
        std::ptr::null_mut(),
    );
    if handle.is_null() {
        return Err(CortexError::Invalid(format!(
            "CreateDesktopW 失败：{}",
            GetLastError()
        )));
    }

    // 把 logon SID 授上桌面全权
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
        TRUSTEE_W,
    };
    let ea = [EXPLICIT_ACCESS_W {
        grfAccessPermissions: DESKTOP_ALL,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 0,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: logon.as_mut_ptr().cast(),
        },
    }];
    let mut acl = std::ptr::null_mut();
    let rc = SetEntriesInAclW(1, ea.as_ptr(), std::ptr::null_mut(), &mut acl);
    if rc != 0 {
        CloseDesktop(handle);
        return Err(CortexError::Invalid(format!("桌面 ACL 合并失败：{rc}")));
    }
    let rc = SetSecurityInfo(
        handle,
        SE_WINDOW_OBJECT,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        acl,
        std::ptr::null_mut(),
    );
    if !acl.is_null() {
        LocalFree(acl.cast());
    }
    if rc != 0 {
        CloseDesktop(handle);
        return Err(CortexError::Invalid(format!("桌面授权失败：{rc}")));
    }
    Ok((handle, format!("WinSta0\\{name}")))
}

/// 建令牌 + 桌面，`CreateProcessAsUserW` 起真命令，等它退出。
unsafe fn spawn_restricted(argv: &[String], cwd: &str) -> Result<u32> {
    let (token, mut logon) = make_restricted_token()?;
    let (desktop, desktop_name) = match make_private_desktop(&mut logon) {
        Ok(v) => v,
        Err(e) => {
            CloseHandle(token);
            return Err(e);
        }
    };

    // ── git 的 HTTPS：换掉 schannel ─────────────────────────────
    //
    // 受限令牌下 schannel 建不出凭据（`SEC_E_NO_CREDENTIALS`），根因实测是
    // **证书系统存储只能只读打开**：`CertOpenSystemStore`（读写）回
    // `ERROR_ACCESS_DENIED`，而同一个存储用 `CERT_STORE_READONLY_FLAG` 开
    // 得开。那不是我们漏授 —— 证书存储的注册表键上明写着
    // `NT AUTHORITY\RESTRICTED: ReadKey`，是 Windows 对受限令牌的既定语义。
    //
    // 所以不去要写权限，而是让 git 不走 schannel：`http.sslBackend=openssl`
    // 用 git 自带的 ca-bundle 验证，不碰证书存储。实测在沙箱里 `git ls-remote`
    // 从 `fatal` 变成拿到真实 HEAD。
    //
    // 用 `GIT_CONFIG_COUNT/KEY/VALUE` 注入而不是改用户的 `~/.gitconfig` ——
    // 那是用户的文件，沙箱不该往里写东西；而且这条只该在沙箱内生效。
    //
    // ⚠️ **只解决走 git CLI 的那条路**。凡是进程自己链了 Schannel 的都换不掉：
    // `curl`（`curl --version` 里写着 Schannel）、以及 cargo 下载 `.crate` 时
    // 用的内建 libcurl。给它们 CA 文件（`--cacert` / `CARGO_HTTP_CAINFO`）也没用 ——
    // 那只换验证用的根，不换 TLS 后端。工具描述里如实写着哪些能、哪些不能。
    {
        use windows_sys::Win32::System::Environment::SetEnvironmentVariableW;
        for (k, v) in [
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "http.sslBackend"),
            ("GIT_CONFIG_VALUE_0", "openssl"),
            // cargo 的索引也改走 git CLI —— 那条路现在是通的。
            // ⚠️ **只解决索引，解决不了 `.crate` 文件的下载**：那一步走 cargo
            // 自己链进去的 libcurl（同样只编了 Schannel），cargo 没有把下载
            // 也交给 git 的开关。实测：加了这两个之后 `Updating crates.io index`
            // 过得去，随后卡在 `failed to download from https://static.crates.io/…`。
            // 留着它们是因为**索引更新本身也常是失败点**，能过一步是一步。
            ("CARGO_NET_GIT_FETCH_WITH_CLI", "true"),
            ("CARGO_REGISTRIES_CRATES_IO_PROTOCOL", "git"),
        ] {
            let kw = wide(k);
            let vw = wide(v);
            SetEnvironmentVariableW(kw.as_ptr(), vw.as_ptr());
        }
    }

    // %TEMP% 指进工作区。受限令牌下真 %TEMP%（授的是用户、非 logon/Everyone）
    // 写不进，而 rustc/link 到处用它。子进程继承 helper 的环境，所以在这里
    // SetEnvironmentVariable 就够。目录在工作区内 → 已授 logon → 写得进。
    {
        use windows_sys::Win32::System::Environment::SetEnvironmentVariableW;
        let tmp = format!(r"{}\.cortex-tmp", strip_verbatim(cwd));
        let _ = std::fs::create_dir_all(&tmp);
        let tmp_w = wide(&tmp);
        let tmp_name = wide("TMP");
        let temp_name = wide("TEMP");
        SetEnvironmentVariableW(tmp_name.as_ptr(), tmp_w.as_ptr());
        SetEnvironmentVariableW(temp_name.as_ptr(), tmp_w.as_ptr());
    }
    let mut cl = wide(&super::windows::command_line(argv));
    let cwd_w = wide(&strip_verbatim(cwd));
    let mut desk_w = wide(&desktop_name);

    let mut si: STARTUPINFOW = std::mem::zeroed();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    si.lpDesktop = desk_w.as_mut_ptr();
    si.dwFlags = STARTF_USESHOWWINDOW;
    si.wShowWindow = 0; // SW_HIDE
    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();

    let ok = CreateProcessAsUserW(
        token,
        std::ptr::null(),
        cl.as_mut_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        1, // bInheritHandles：继承父给的 stdio
        0,
        std::ptr::null(),
        cwd_w.as_ptr(),
        &si,
        &mut pi,
    );
    if ok == 0 {
        let e = GetLastError();
        CloseDesktop(desktop);
        CloseHandle(token);
        return Err(CortexError::Invalid(format!(
            "CreateProcessAsUserW 失败：{e}"
        )));
    }

    WaitForSingleObject(pi.hProcess, INFINITE);
    let mut code: u32 = 0;
    GetExitCodeProcess(pi.hProcess, &mut code);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseDesktop(desktop);
    CloseHandle(token);
    Ok(code)
}
