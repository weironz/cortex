//! `cortex login` 存下来的东西，以及它存在哪。
//!
//! # 为什么 CLI 非要有登录不可
//!
//! 在这之前 CLI 只能用 `cortex-agentd --generate-token` 那把**预共享
//! token**，而那把 token 映射的永远是**第一个账号**。后果分两种：
//!
//! - 单人部署：桌面端登的就是 1 号，CLI 也是 1 号 —— 同一份数据，正常
//! - 多用户：桌面端登的若是 2 号，CLI 拿预共享 token 进去的是**1 号的数据**
//!   —— 不报错，就是另一个人的会话与记忆
//!
//! 第二种是这个模块存在的全部理由。它不报错，所以只能靠「让 CLI 也能
//! 表明自己是谁」来解决，而不能靠提醒。
//!
//! # 为什么不是「多加一个 --access-token 参数」
//!
//! access token 只活 900 秒。让人手工粘贴一个 15 分钟后就失效的东西，
//! 等于每刻钟登一次录 —— 所以必须是一条真正的登录命令：拿 refresh token
//! 存本机，每次用它换一把新的 access。
//!
//! # 存哪儿：本机文件，0600
//!
//! 桌面端把 refresh token 交给系统凭据库（Windows Credential Manager /
//! macOS Keychain）。CLI 这边**不引入那个依赖**：
//!
//! - 那要多一个跨三平台的原生依赖，而 CLI 要能在一台只有 ssh 的服务器上跑，
//!   那里根本没有会话钥匙串（headless 上 Secret Service 常常连不上，
//!   而失败方式是「登录成功但下次读不回来」）
//! - `gh` / `aws` / `docker` 都是文件 + 严格权限，运维对这个形态有预期
//!
//! 代价写在明处：**同机的其他用户如果能读你的家目录，就能拿到这个文件**。
//! 0600 挡的是「同机别的账号」，挡不了 root，也挡不了你自己把它 cat 出来
//! 贴进聊天窗口。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 落盘的那份。**只存 refresh token，不存 access。**
///
/// access 活 900 秒，存下来的那一刻就在过期路上 —— 存它只会让「文件里有
/// 一个不能用的凭据」变成一种需要处理的状态。每次跑起来现换一把即可。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredLogin {
    /// 这份凭据是对**哪台服务端**的。
    ///
    /// 必须存：一个人会在公司那台和自己那台之间来回切，而两边的 refresh
    /// token 互不认识。不存的话，切一次地址就会拿着 A 的凭据去 B 那儿，
    /// 得到的是 401 —— 而错误信息会让人以为凭据过期了。
    pub server: String,
    /// 登的是谁。**只为了能打印出来**（`cortex login` 之后那句确认，
    /// 以及将来 `cortex whoami`）。服务端认的是 token 不是它。
    pub username: String,
    pub refresh_token: String,
}

/// 凭据文件的位置。
///
/// `CORTEX_CLI_HOME` 存在的理由是测试：它要能在一个临时目录里跑完整条
/// 存取路径，而不是去碰真实用户的家目录 —— 后者会让「跑一次测试」变成
/// 「把我自己登出了」。
///
/// # Errors
/// 找不到家目录（罕见，但在某些 CI 容器里真的会）。
pub fn credentials_path() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("CORTEX_CLI_HOME")
        && !dir.trim().is_empty()
    {
        return Ok(PathBuf::from(dir).join("credentials.json"));
    }
    let home = home_dir().ok_or_else(|| {
        anyhow::anyhow!("找不到家目录，无法定位凭据文件；可以用 CORTEX_CLI_HOME 指定一个目录")
    })?;
    Ok(home.join(".cortex").join("credentials.json"))
}

/// 与 `cortex_agent::sandbox::policy` 里那份同一个写法，**刻意不引 `dirs`**：
/// 为了一个两行的查询多一条依赖不划算，而这两个环境变量在三个平台上都稳定。
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// 读回这台机器上存着的登录。**服务端地址对不上就当没有。**
///
/// 返回 `None` 而不是报错：没登录过是最常见的情况，而它与「文件坏了」
/// 对用户是同一件事 —— 都得去登录一次。坏文件额外打一行 debug。
#[must_use]
pub fn load(server: &str) -> Option<StoredLogin> {
    load_at(&credentials_path().ok()?, server)
}

/// [`load`] 的**不读全局状态**的那一半。
///
/// 拆出来是为了测试：让它自己去读 `CORTEX_CLI_HOME` 的话，测试就得
/// `set_var` —— 而那是**进程全局**的，两条并行的用例会互相踩。
/// 这个仓库为同一个原因返工过两次（代理那组、admin_spec 那条），
/// 而我写这个模块时又踩了第三次：两条用例各自 set/remove 同一个变量，
/// 单独跑绿、一起跑红。
#[must_use]
pub fn load_at(path: &std::path::Path, server: &str) -> Option<StoredLogin> {
    let raw = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<StoredLogin>(&raw) {
        Ok(s) if s.server == normalize_server(server) => Some(s),
        Ok(s) => {
            tracing_hint(&format!(
                "凭据文件里存的是 {} 的登录，而这次连的是 {server} —— 当作未登录",
                s.server
            ));
            None
        }
        Err(e) => {
            tracing_hint(&format!(
                "凭据文件读不动（{e}），当作未登录：{}",
                path.display()
            ));
            None
        }
    }
}

/// 存下来。**目录 0700、文件 0600**（Unix）。
///
/// # Errors
/// 建不了目录、写不了文件。
pub fn save(login: &StoredLogin) -> anyhow::Result<PathBuf> {
    save_at(&credentials_path()?, login)
}

/// [`save`] 的不读全局状态的那一半。见 [`load_at`]。
///
/// # Errors
/// 建不了目录、写不了文件。
pub fn save_at(path: &std::path::Path, login: &StoredLogin) -> anyhow::Result<PathBuf> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        set_private(dir, 0o700)?;
    }
    let body = serde_json::to_string_pretty(login)?;
    std::fs::write(path, body)?;
    set_private(path, 0o600)?;
    Ok(path.to_path_buf())
}

/// 删掉。**不存在也算成功** —— `logout` 跑两次不该第二次报错。
///
/// # Errors
/// 文件在但删不掉。
pub fn clear() -> anyhow::Result<()> {
    clear_at(&credentials_path()?)
}

/// [`clear`] 的不读全局状态的那一半。见 [`load_at`]。
///
/// # Errors
/// 文件在但删不掉。
pub fn clear_at(path: &std::path::Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// 地址归一化：末尾斜杠不该让同一台服务端变成两份凭据。
#[must_use]
pub fn normalize_server(s: &str) -> String {
    s.trim().trim_end_matches('/').to_string()
}

/// Unix 上收紧权限；Windows 上是 no-op 并**说明为什么**。
///
/// Windows 的等价物是 ACL，而 `%USERPROFILE%` 默认就只有本人与管理员可读 ——
/// 再去动 ACL 需要一个 winapi 依赖，换来的边际安全性接近零。
/// 这里不假装做了：注释写清楚，比一个空函数好。
#[cfg(unix)]
fn set_private(path: &std::path::Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private(_path: &std::path::Path, _mode: u32) -> anyhow::Result<()> {
    Ok(())
}

/// 这个 crate 没有接 tracing，而这几条是给人看的提示，不是日志。
fn tracing_hint(msg: &str) {
    eprintln!("提示：{msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **换了服务端就不该复用凭据。**
    ///
    /// 不判的话，拿着公司那台的 refresh token 去自己那台换 access，
    /// 得到 401 —— 而那条错误读起来像「凭据过期了」，于是人会去重新登录
    /// 公司那台，然后困惑为什么还是不行。
    ///
    /// 用 `*_at` 而不是设 `CORTEX_CLI_HOME`：环境变量是进程全局的，
    /// 两条并行用例会互相踩。见 [`load_at`] 的文档 —— 第一版就是这么写的，
    /// 单独跑绿、`cargo test` 一起跑红。
    #[test]
    fn a_login_for_another_server_is_not_reused() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("credentials.json");

        save_at(
            &path,
            &StoredLogin {
                server: normalize_server("https://a.example.com"),
                username: "alice".into(),
                refresh_token: "r1".into(),
            },
        )
        .expect("存");

        assert!(
            load_at(&path, "https://a.example.com").is_some(),
            "同一台应当读得回来"
        );
        assert!(
            load_at(&path, "https://b.example.com").is_none(),
            "另一台服务端的凭据不该被拿来用"
        );
        assert!(
            load_at(&path, "https://a.example.com/").is_some(),
            "末尾斜杠属于同一台，不该变成两份凭据"
        );
    }

    /// `logout` 跑两次不该第二次报错。
    #[test]
    fn clearing_twice_is_fine() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("credentials.json");
        save_at(
            &path,
            &StoredLogin {
                server: normalize_server("http://x"),
                username: "u".into(),
                refresh_token: "r".into(),
            },
        )
        .expect("存");
        clear_at(&path).expect("第一次");
        clear_at(&path).expect("第二次也该成功");
        assert!(load_at(&path, "http://x").is_none());
    }

    /// 坏掉的凭据文件当作「没登录」，而不是让整条命令炸掉。
    ///
    /// 一个手工编辑过、或者写到一半断电的文件，不该让 `cortex chat` 完全
    /// 不能用 —— 那时正确的下一步是「重新登录一次」，而不是「先去修 JSON」。
    #[test]
    fn a_corrupt_file_reads_as_not_logged_in() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, "{ 这不是 json").expect("写");
        assert!(load_at(&path, "http://x").is_none());
    }
}
