//! 让 AppContainer 档跑得动 `cargo build` —— 三件套。
//!
//! # 「结构性跑不了」被实验推翻的经过
//!
//! 此前的结论是「链接步在 AppContainer 里结构性地挂」（`link.exe` 报
//! `0xC0000142`，`rust-lld` 报 permission denied，FULL 授权也没救）。
//! 2026-08-29 重审时发现老实验有个洞：给 `.rustup` 的那次 FULL 走的是
//! 不传播的授权路，**文件本体从没拿到 ACE**。补齐三件事之后，
//! `cargo build`（含依赖、含 build script）在 AppContainer 里出了能跑的
//! .exe，全部实测：
//!
//! 1. **`.cargo` / `.rustup` 授容器 RX（可继承）** —— 没有它连 `cargo`
//!    这个命令都找不到（PATH 上的 `~/.cargo/bin` 读不了）。一次性代价
//!    实测 `.cargo` 60 秒、`.rustup` 182 秒，之后 `already_granted` 短路。
//! 2. **`rust-lld` 拷进工作区内**。位置论实测坐实，且真相是**祖先链穿透**
//!    而不是文件自身的 ACE：AppContainer 加载映像要求从卷根到文件每一级
//!    容器都能穿过，而 `%LOCALAPPDATA%\Cortex\...` 那条链里有
//!    `C:\Users\<你>` 这种改不动 DACL 的层（同 8.3 短名那节的墙），
//!    lld 放那儿即使授到 FULL 也 permission denied。工作区的祖先链
//!    `grant_ancestors` 已经授穿，所以拷到 `<工作区>\target\.cortex\`
//!    里就正常执行（`target` 几乎总在 gitignore，拷进去的文件继承工作区
//!    的容器 FULL）。`link.exe` 的 `0xC0000142` 是另一族（DLL 群在 VS
//!    目录里），不修 —— 换 lld 绕开。
//! 3. **注入 `LIB`**。换掉链接器之后 rustc 不再替你定位 MSVC / Windows SDK
//!    的导入库，lld 挂在 `could not open 'kernel32.lib'` 上。宿主侧发现
//!    两处 lib 目录注入进去；SDK 目录自带 `ALL APPLICATION PACKAGES` 的
//!    继承 RX，容器读得到，不用我们授。
//!
//! # 边界如实
//!
//! - **含 C/C++ 构建步骤的依赖仍然跑不了**：`cc` crate 调的 `cl.exe` 与
//!   `link.exe` 同族（`0xC0000142`），本模块不碰。要编 ring / openssl-sys
//!   这类，换受限令牌档。
//! - **拉新依赖照旧受网络策略管**：这一档默认断网，带 `Cargo.lock` 且
//!   本地缓存齐的项目才编得动。这不是本模块的欠账，是这一档的设计。
//! - 注入 `RUSTFLAGS` 会**追加**在用户已有值之后 —— 用户显式选过 linker
//!   的话会被我们盖掉。取舍：不盖的话在这一档里必然 `0xC0000142`，
//!   一个「尊重了用户设置」的必挂不如一个「盖掉后能跑」的明说。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cortex_core::Result;

/// 读 + 执行 + 列目录（`FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`）。
/// 不给写：工具链是用户的，沙箱往里写就是往「沙箱外下次也会执行的东西」
/// 里写 —— 与受限令牌档不授 `~/.cargo` 写是同一条理由。
const FILE_READ_EXECUTE: u32 = 0x0012_00A9;

/// 给沙箱命令注入「cargo build 能跑」所需的环境。**尽力而为**：
/// 本机没有 Rust 工具链就什么都不做，任何一步失败也只是回到原样报错。
///
/// # Safety
/// `sid` 必须是有效的容器 SID（调用方持有生命周期）。
pub(super) unsafe fn setup(
    cmd: &mut std::process::Command,
    sid: *mut std::ffi::c_void,
    cwd: &Path,
) {
    // lld 的落点必须在工作区内（target\.cortex\）：AppContainer 加载映像
    // 要求祖先链每一级都能穿过，而 Cortex 在 %LOCALAPPDATA% 下的目录穿不过
    // C:\Users\<你> 那一层（见模块文档 #2）。工作区随命令变，所以这一步
    // 每条命令都确认一次（按已存在短路），不像工具链授权那样进程级只做一次。
    let lld = match unsafe { ensure_lld_in_workspace(sid, cwd) } {
        Some(p) => p,
        None => return, // 没 rustc / 拷不动，回到原样报错
    };

    // 工具链两个根的只读授权是进程级一次性的（磁盘 ACL 跨进程幂等）。
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: 调用方保证 sid 有效；闭包同步执行完毕，不逃逸
        if let Err(e) = unsafe { grant_toolchain_roots(sid) } {
            tracing::warn!(error = %e, "Rust 工具链只读授权失败，沙箱里 cargo build 会照旧报错");
        }
    });

    let lib = discover_lib_dirs();
    // 追加而不是覆盖 —— 但同名冲突时后出现的 -Clinker 生效，即我们的。
    // 理由见模块文档「边界如实」最后一条。
    let flags = match std::env::var("RUSTFLAGS") {
        Ok(prev) if !prev.trim().is_empty() => format!("{prev} -Clinker={}", lld.display()),
        _ => format!("-Clinker={}", lld.display()),
    };
    cmd.env("RUSTFLAGS", flags);
    if let Some(lib) = &lib {
        let joined = match std::env::var("LIB") {
            Ok(prev) if !prev.trim().is_empty() => format!("{prev};{lib}"),
            _ => lib.clone(),
        };
        cmd.env("LIB", joined);
    }
}

/// 把 rust-lld 备进**工作区内** `target\\.cortex\\`，回它的路径。
///
/// 为什么在工作区内而不是 Cortex 自己的目录：见模块文档 #2（映像加载的
/// 祖先链穿透）。放 target 下是因为它几乎总在 gitignore，不污染用户的
/// `git status`；而工作区根 `grant_workspace` 已授容器 FULL 且可继承，
/// 拷进 target 的子目录自动继承。
///
/// None = 没 rustc 或拷不动。已存在就直接返回（幂等，省掉重复拷贝）。
///
/// # Safety
/// `sid` 必须是有效的容器 SID。
unsafe fn ensure_lld_in_workspace(sid: *mut std::ffi::c_void, cwd: &Path) -> Option<PathBuf> {
    let target = cwd.join("target").join(".cortex");
    let dst = target.join("rust-lld.exe");
    if dst.is_file() {
        return Some(dst);
    }
    let sysroot = rustc_sysroot()?;
    let src = sysroot.join(r"lib\\rustlib\\x86_64-pc-windows-msvc\\bin\\rust-lld.exe");
    if !src.is_file() {
        return None;
    }
    if let Err(e) = std::fs::create_dir_all(&target) {
        tracing::warn!(error = %e, "建不出 target\\\\.cortex 目录，cargo build 会照旧挂在链接步");
        return None;
    }
    // target 若是首次由本命令创建，它继承工作区根的容器 FULL；但若 target
    // 早已存在（上一次非沙箱构建建的），它可能没有容器 ACE —— 补授一次，
    // 幂等短路。授的是 .cortex 子目录即可，不动 cargo 自己的 target 内容。
    // SAFETY: 调用方保证 sid 有效
    if let Err(e) = unsafe {
        super::windows::grant_to_container(
            &target,
            sid,
            super::windows::INHERIT_ALL,
            FILE_READ_EXECUTE,
        )
    } {
        tracing::warn!(error = %e, "给 target\\\\.cortex 授容器权限失败");
        return None;
    }
    if let Err(e) = std::fs::copy(&src, &dst) {
        tracing::warn!(error = %e, "拷不动 rust-lld 到工作区");
        return None;
    }
    tracing::info!(lld = %dst.display(), "已为沙箱备好 rust-lld（AppContainer 里 MSVC link.exe 起不来，用它替代）");
    Some(dst)
}

/// 授 `.cargo` / `.rustup` 两个根容器只读（可继承）。进程级一次性。
///
/// 没有它连 `cargo` 都找不到（`~/.cargo/bin` 读不了）。一次性代价实测
/// `.cargo` 60 秒、`.rustup` 182 秒，之后 `already_granted` 短路。
///
/// # Safety
/// `sid` 必须是有效的容器 SID。
unsafe fn grant_toolchain_roots(sid: *mut std::ffi::c_void) -> Result<()> {
    let Some(home) = std::env::var_os("USERPROFILE") else {
        return Ok(());
    };
    let home = PathBuf::from(home);
    for d in [home.join(".cargo"), home.join(".rustup")] {
        if !d.is_dir() {
            continue;
        }
        let t = std::time::Instant::now();
        // SAFETY: 调用方保证 sid 有效
        if unsafe {
            super::windows::grant_to_container(
                &d,
                sid,
                super::windows::INHERIT_ALL,
                FILE_READ_EXECUTE,
            )?
        } {
            tracing::info!(
                dir = %d.display(),
                elapsed_ms = t.elapsed().as_millis(),
                "首次为 Rust 工具链授只读权限（跨重启只做一次；.rustup 实测约三分钟）"
            );
        }
    }
    Ok(())
}

/// 宿主的 rustc sysroot，每进程问一次。
fn rustc_sysroot() -> Option<PathBuf> {
    static S: OnceLock<Option<PathBuf>> = OnceLock::new();
    S.get_or_init(|| {
        let out = std::process::Command::new("rustc")
            .args(["--print", "sysroot"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!p.is_empty()).then(|| PathBuf::from(p))
    })
    .clone()
}

/// 找 MSVC 与 Windows SDK 的导入库目录，各取版本号最大的一个。
///
/// 不用 vswhere（那是 VS 安装器带的，路径本身要先找它）：两处布局是
/// 微软文档化的固定形状，按目录名排序取最新与 rustc 自己的策略一致。
fn discover_lib_dirs() -> Option<String> {
    fn newest(dir: &Path) -> Option<PathBuf> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .collect();
        names.sort();
        names.pop()
    }

    let mut parts = Vec::new();
    // MSVC：C:\Program Files\Microsoft Visual Studio\<年>\<版>\VC\Tools\MSVC\<ver>\lib\x64
    'msvc: for pf in ["C:\\Program Files", "C:\\Program Files (x86)"] {
        let vs = Path::new(pf).join("Microsoft Visual Studio");
        let Ok(years) = std::fs::read_dir(&vs) else {
            continue;
        };
        let mut years: Vec<_> = years.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        years.sort();
        for year in years.iter().rev() {
            let Ok(editions) = std::fs::read_dir(year) else {
                continue;
            };
            for ed in editions.filter_map(|e| e.ok()) {
                let msvc = ed.path().join(r"VC\Tools\MSVC");
                if let Some(v) = newest(&msvc) {
                    let lib = v.join(r"lib\x64");
                    if lib.is_dir() {
                        parts.push(lib);
                        break 'msvc;
                    }
                }
            }
        }
    }
    // SDK：C:\Program Files (x86)\Windows Kits\10\Lib\<ver>\{um,ucrt}\x64
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\Lib");
    if let Some(v) = newest(kits) {
        for sub in ["um", "ucrt"] {
            let lib = v.join(sub).join("x64");
            if lib.is_dir() {
                parts.push(lib);
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(
        parts
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(";"),
    )
}
