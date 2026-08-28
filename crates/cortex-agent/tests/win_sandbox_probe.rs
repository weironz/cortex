//! **Windows 沙箱选型的实测证据。**
//!
//! 这个文件里的两条不是产品测试 —— 它们是**把一个选型问题变成一个测量
//! 结果**的那次实验，留在仓库里是为了下一个人不必重做一遍，也为了将来
//! 有人怀疑这个选型时能当场复跑。
//!
//! ```text
//! cargo test -p cortex-agent --test win_sandbox_probe -- --ignored --nocapture
//! ```
//!
//! 默认 `#[ignore]`：它们会改文件系统 ACL、建 AppContainer profile，
//! 不该在每次 `just ci` 里跑。
//!
//! # 要回答的问题
//!
//! Windows 上没有 landlock / Seatbelt 的对等物，而 `shell` 工具跑的是
//! **PowerShell**。三条候选路，各自的死穴不同：
//!
//! | 机制 | 要管理员吗 | .NET / PowerShell | 结论 |
//! |---|---|---|---|
//! | 受限令牌（`CreateRestrictedToken`） | **不要** | **起不来** | ✗ |
//! | 独立本地用户（codex 的做法） | **要**（一次） | 正常 | 可行但贵 |
//! | **AppContainer** | **不要** | **正常** | ✓ **选它** |
//!
//! # 实测数字（2026-08-28，Windows 11 Pro 26200）
//!
//! **受限令牌**（restricting SID = Everyone + BUILTIN\Users + RESTRICTED）：
//!
//! ```text
//! cmd 写文件            0        用户目录里的文件   1（挡住了）
//! git --version         0        授权后的工作区     0（读得到）
//! powershell 5.1        0xFFFF0000   ← CLR 起不来
//! pwsh 7                0xE0434352   ← 同上
//! ```
//!
//! 也就是说：**边界成立，但 shell 工具用不了**。这条路只能配 `cmd.exe`，
//! 而那会改掉用户与模型都已经习惯的 shell 语义。
//!
//! 顺带解释了 codex 为什么宁可要那一次管理员配置去建独立用户 ——
//! 独立用户拿的是正常令牌，.NET 没问题。
//!
//! **AppContainer**（`CreateAppContainerProfile` + `SECURITY_CAPABILITIES`）：
//!
//! ```text
//! cmd / powershell 5.1 / pwsh 7 / git   全部 0
//! 用户目录里的文件                       1（挡住了）
//! 工作区：授权前 1 / 授权后 0
//! ```
//!
//! **三件都成立，且不需要管理员。** 所以阶段 1 走这条。
//!
//! # 这个边界买到什么、买不到什么
//!
//! 买到的：子进程**读不到用户目录里的东西** —— `~/.ssh`、`~/.aws`、
//! 浏览器 profile、文档。那正是模块头里「`bash -c 'cat ~/.ssh/id_rsa'`」
//! 那句话说的威胁。
//!
//! **买不到的，一条都不许含糊：**
//!
//! 1. **不挡网络。** AppContainer 默认没有网络能力，但这一条要靠不给
//!    `internetClient` 之类的 capability 来保证，而那会连带挡掉 agent
//!    自己要用的东西（`git clone`、`npm i`）—— 怎么取舍是阶段 2 的事
//! 2. **不挡 `ALL APPLICATION PACKAGES` 可读的东西**：`C:\Windows`、
//!    `Program Files` 的大部分。那里没有用户的秘密，但也不是「什么都碰不到」
//! 3. **不挡进程与注册表的全部**，只挡按 ACL 判定的那些
//! 4. **不挡把工作区里的东西发出去** —— 那是网络那一层的事

#![cfg(windows)]
// Rust 2024 要求 unsafe fn 内部也显式 unsafe 块。这里整片都是原始 Win32
// 调用，逐个包一层只是噪音 —— codex 那个 crate 顶上也是同样的处理。
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW,
    TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{ACL, DACL_SECURITY_INFORMATION, SECURITY_CAPABILITIES};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 建（或复用）一个 AppContainer profile，返回它的 SID。
unsafe fn app_container_sid(name: &str) -> *mut c_void {
    let n = wide(name);
    let disp = wide("cortex sandbox probe");
    let mut sid: *mut c_void = std::ptr::null_mut();
    // 已经存在时回 HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)，那时改用 Derive
    let hr = CreateAppContainerProfile(
        n.as_ptr(),
        disp.as_ptr(),
        disp.as_ptr(),
        std::ptr::null(),
        0,
        &mut sid,
    );
    if hr != 0 {
        let hr2 = DeriveAppContainerSidFromAppContainerName(n.as_ptr(), &mut sid);
        assert!(
            hr2 == 0,
            "拿不到 AppContainer SID：create={hr:#x} derive={hr2:#x}"
        );
    }
    sid
}

/// 在 AppContainer 里跑一条命令，回退出码。
unsafe fn run_in_container(sid: *mut c_void, cmdline: &str) -> u32 {
    let mut caps = SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: std::ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };

    let mut size: usize = 0;
    InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
    let mut buf = vec![0u8; size];
    let attrs: LPPROC_THREAD_ATTRIBUTE_LIST = buf.as_mut_ptr().cast();
    let ok = InitializeProcThreadAttributeList(attrs, 1, 0, &mut size);
    assert!(
        ok != 0,
        "InitializeProcThreadAttributeList 失败：{}",
        GetLastError()
    );

    // PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES
    const ATTR: usize = 0x0002_0009;
    let ok = UpdateProcThreadAttribute(
        attrs,
        0,
        ATTR,
        std::ptr::from_mut(&mut caps).cast::<c_void>(),
        std::mem::size_of::<SECURITY_CAPABILITIES>(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    assert!(
        ok != 0,
        "UpdateProcThreadAttribute 失败：{}",
        GetLastError()
    );

    let mut si: STARTUPINFOEXW = std::mem::zeroed();
    si.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).unwrap();
    si.lpAttributeList = attrs;
    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
    let mut cl = wide(cmdline);

    let ok = CreateProcessW(
        std::ptr::null(),
        cl.as_mut_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        0,
        EXTENDED_STARTUPINFO_PRESENT,
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::from_ref(&si).cast(),
        &mut pi,
    );
    if ok == 0 {
        let e = GetLastError();
        DeleteProcThreadAttributeList(attrs);
        println!("  CreateProcessW 失败：{e}（命令：{cmdline}）");
        return u32::MAX;
    }
    WaitForSingleObject(pi.hProcess, INFINITE);
    let mut code: u32 = 0;
    GetExitCodeProcess(pi.hProcess, &mut code);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    DeleteProcThreadAttributeList(attrs);
    code
}

/// 把 `dir` 授给某个 SID（完全控制 + 继承）。
unsafe fn grant_sid(dir: &Path, sid: *mut c_void) {
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: 0x001F_01FF,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 3,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: sid.cast::<u16>(),
        },
    };
    let mut new_acl: *mut ACL = std::ptr::null_mut();
    let rc = SetEntriesInAclW(1, &ea, std::ptr::null_mut(), &mut new_acl);
    assert!(rc == 0, "SetEntriesInAclW 失败：{rc}");
    let mut path = wide(&dir.display().to_string());
    let rc = SetNamedSecurityInfoW(
        path.as_mut_ptr(),
        SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        new_acl,
        std::ptr::null_mut(),
    );
    assert!(rc == 0, "SetNamedSecurityInfoW 失败：{rc}");
}

#[test]
#[ignore = "探针"]
fn appcontainer_里_powershell_起不起得来() {
    unsafe {
        let name = "cortex.sandbox.probe";
        let sid = app_container_sid(name);
        println!("拿到 AppContainer SID");

        for (what, cmd) in [
            ("cmd", "cmd.exe /c exit 0"),
            (
                "powershell 5.1",
                "powershell.exe -NoProfile -Command \"exit 0\"",
            ),
            ("pwsh 7", "pwsh.exe -NoProfile -Command \"exit 0\""),
            ("git", "git.exe --version"),
        ] {
            let rc = run_in_container(sid, cmd);
            println!("{what:<16} 退出码 {rc}");
        }

        let n = wide(name);
        let _ = DeleteAppContainerProfile(n.as_ptr());
    }
}

#[test]
#[ignore = "探针"]
fn appcontainer_挡不挡得住用户目录() {
    unsafe {
        let name = "cortex.sandbox.probe2";
        let sid = app_container_sid(name);

        let work = std::env::temp_dir().join("cortex-appc-work");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("inside.txt"), "INSIDE").unwrap();
        let home = std::env::var("USERPROFILE").unwrap();
        let secret = Path::new(&home).join("cortex-appc-secret.txt");
        std::fs::write(&secret, "SECRET").unwrap();

        let type_it = |p: &Path| format!("cmd.exe /c type \"{}\"", p.display());
        let inside = work.join("inside.txt");

        let rc_secret = run_in_container(sid, &type_it(&secret));
        let rc_before = run_in_container(sid, &type_it(&inside));
        grant_sid(&work, sid);
        let rc_after = run_in_container(sid, &type_it(&inside));

        println!("① 用户目录里的文件   退出码 {rc_secret}（非 0 = 挡住了）");
        println!("② 授权前的工作区     退出码 {rc_before}");
        println!("③ 授权后的工作区     退出码 {rc_after}（0 = 读得到）");

        let _ = std::fs::remove_file(&secret);
        let _ = std::fs::remove_dir_all(&work);
        let n = wide(name);
        let _ = DeleteAppContainerProfile(n.as_ptr());

        assert_ne!(
            rc_secret, 0,
            "AppContainer 读到了用户目录里的文件 —— 边界不成立"
        );
        assert_eq!(rc_after, 0, "授权之后仍读不到工作区 —— agent 没法干活");
    }
}
