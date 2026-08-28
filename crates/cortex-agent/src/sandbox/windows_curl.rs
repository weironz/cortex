//! 给受限令牌沙箱备一份**非 Schannel** 的 curl。
//!
//! # 为什么
//!
//! 受限令牌下 schannel 建不出 TLS 客户端凭据（根因见
//! [`super::windows_cargo_mirror`] 的模块文档），而系统自带的 curl 和
//! Git 自带的 curl **都只编了 Schannel**（实测版本串一模一样），没有
//! 运行时开关可换。所以 `curl https://` 在沙箱里必挂。
//!
//! 解法与 git 那条同形状 —— 换 TLS 后端：curl 官方的 Windows 构建
//! （curl.se/windows，curl-for-win 项目）用的是 **LibreSSL**，不碰
//! 证书存储。实测它在受限令牌沙箱里 HTTPS 全通，条件是把
//! `CURL_CA_BUNDLE` 指到随包的 `curl-ca-bundle.crt`：这份构建带
//! NativeCA 特性，**默认**去读 Windows 证书存储做验证，而那条路在
//! 沙箱里照样走不通（握手能过，验证挂在 unable to get local issuer）。
//!
//! # 供应链
//!
//! 下载地址与 **SHA-256 都钉死在源码里**，不信 DNS 也不信 TLS ——
//! 哈希对不上就删掉重来，绝不解压。校验值抄自 curl.se/windows 页面
//! 公布的值，且本机独立算过一遍相符。
//!
//! ⚠️ **curl.se 只保留最近几个版本的下载目录**，上游发新版后这条 URL
//! 会 404。那时的行为是：下载失败 → 打 WARN → 不注入 PATH → `curl`
//! 退回 Schannel 的老报错（诚实回落，不会更糟）。逃逸测试里那条
//! `curl 走 https 能通` 会先红，提醒来这里同时换 URL 和哈希。
//!
//! # 下载与解压全用系统自带工具，按绝对路径调
//!
//! 宿主侧的 `System32\curl.exe`（Schannel 在宿主上是好的）下载、
//! `System32\certutil.exe` 算哈希、`System32\tar.exe`（bsdtar）解 zip。
//! 三个都是 Win10+ 必有的。**必须绝对路径**：裸敲 `tar` 在 Git Bash
//! 环境里解析到 GNU tar，它不认 zip（实测踩过）；裸敲任何名字也都
//! 可能被 PATH 上的同名程序顶掉。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cortex_core::{CortexError, Result};

/// 钉死的版本。换版本要同时换三样：目录名、URL、哈希。
const VERSION: &str = "8.21.0_7";
const URL: &str = "https://curl.se/windows/dl-8.21.0_7/curl-8.21.0_7-win64-mingw.zip";
/// curl.se/windows 公布的 SHA-256（本机独立算过一遍相符）。
const SHA256: &str = "e469dcdb219d0eca9236b01c7e4bb34fe04af3d4036d350829178dc60f241ae4";

/// 保证本机有这份 curl，回它的 `bin` 目录。**幂等，每进程只真做一次。**
///
/// 任何一步失败都回 `None`（打过 WARN）：调用方据此不注入 PATH，
/// `curl` 退回原样报错 —— 绝不因为「装备品装不上」拦住别的命令。
pub(super) fn ensure_curl() -> Option<&'static Path> {
    static ONCE: OnceLock<Option<PathBuf>> = OnceLock::new();
    ONCE.get_or_init(|| match setup() {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(error = %e, "备不出 LibreSSL 版 curl，沙箱里 curl 的 HTTPS 会继续报错");
            None
        }
    })
    .as_deref()
}

fn setup() -> Result<PathBuf> {
    let local = std::env::var("LOCALAPPDATA")
        .map_err(|_| CortexError::Invalid("没有 LOCALAPPDATA".into()))?;
    let tools = Path::new(&local)
        .join("Cortex")
        .join("win-sandbox")
        .join("tools");
    let bin = tools
        .join(format!("curl-{VERSION}-win64-mingw"))
        .join("bin");
    if bin.join("curl.exe").exists() && bin.join("curl-ca-bundle.crt").exists() {
        return Ok(bin);
    }

    std::fs::create_dir_all(&tools)
        .map_err(|e| CortexError::Invalid(format!("建不出工具目录：{e}")))?;
    let sysdir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let sys32 = Path::new(&sysdir).join("System32");

    // 下载到带 pid 的临时名：两个进程同时首启时各下各的，互不覆盖；
    // 最后的解压落点靠「目录已在就算别人赢了」收敛。
    let zip = tools.join(format!("curl-{VERSION}.zip.{}", std::process::id()));
    tracing::info!(url = URL, "首次准备沙箱用的 curl（约 9 MB，一次性）");
    let dl = std::process::Command::new(sys32.join("curl.exe"))
        .args(["-sSL", "--max-time", "300", "-o"])
        .arg(&zip)
        .arg(URL)
        .output()
        .map_err(|e| CortexError::Invalid(format!("起不了下载进程：{e}")))?;
    if !dl.status.success() {
        let _ = std::fs::remove_file(&zip);
        return Err(CortexError::Invalid(format!(
            "下载 {URL} 失败：{}",
            String::from_utf8_lossy(&dl.stderr).trim()
        )));
    }

    // 校验。certutil 的输出格式跨语言稳定：哈希单独占一行、64 个十六进制字符
    //（老版本带空格分组，这里剥掉再比）。
    let hash_out = std::process::Command::new(sys32.join("certutil.exe"))
        .arg("-hashfile")
        .arg(&zip)
        .arg("SHA256")
        .output()
        .map_err(|e| CortexError::Invalid(format!("起不了 certutil：{e}")))?;
    let got = String::from_utf8_lossy(&hash_out.stdout)
        .lines()
        .map(|l| l.replace(' ', "").to_ascii_lowercase())
        .find(|l| l.len() == 64 && l.bytes().all(|b| b.is_ascii_hexdigit()));
    if got.as_deref() != Some(SHA256) {
        let _ = std::fs::remove_file(&zip);
        return Err(CortexError::Invalid(format!(
            "下载的 curl 哈希不符（拿到 {got:?}，要 {SHA256}）—— 不解压，已删除"
        )));
    }

    // 解压到带 pid 的暂存目录，成了再一次性挪进最终名 ——
    // 别的进程只会看到「没有」或「完整」，不会看到解了一半的。
    let staging = tools.join(format!("staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| CortexError::Invalid(format!("建不出暂存目录：{e}")))?;
    let tar = std::process::Command::new(sys32.join("tar.exe"))
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&staging)
        .output()
        .map_err(|e| CortexError::Invalid(format!("起不了 tar：{e}")))?;
    let _ = std::fs::remove_file(&zip);
    if !tar.status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(CortexError::Invalid(format!(
            "解压失败：{}",
            String::from_utf8_lossy(&tar.stderr).trim()
        )));
    }

    let unpacked = staging.join(format!("curl-{VERSION}-win64-mingw"));
    let target = tools.join(format!("curl-{VERSION}-win64-mingw"));
    match std::fs::rename(&unpacked, &target) {
        Ok(()) => {}
        // 挪不动多半是并发的另一个进程已经放好了 —— 下面按最终态复核
        Err(e) => tracing::debug!(error = %e, "挪进最终目录失败，按已存在处理"),
    }
    let _ = std::fs::remove_dir_all(&staging);

    if bin.join("curl.exe").exists() && bin.join("curl-ca-bundle.crt").exists() {
        Ok(bin)
    } else {
        Err(CortexError::Invalid(
            "解压完成但没有 curl.exe / curl-ca-bundle.crt —— 包的目录结构变了？".into(),
        ))
    }
}
