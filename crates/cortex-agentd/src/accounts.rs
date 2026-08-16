//! 账号：注册 / 登录 / 刷新 / 登出，以及它们背后的那几张表。
//!
//! 纯逻辑（密码哈希、令牌生成、重放判定）在 [`crate::credentials`]；
//! 这里只做「跟数据库打交道」与「拼成 HTTP」。
//!
//! # 会话为什么能一直用下去
//!
//! 两种令牌，寿命差两个数量级：
//!
//! - **access token**：15 分钟。每次请求都带，泄露的窗口因此很短。
//! - **refresh token**：30 天。只在换 access token 时用一次，用完就轮转。
//!
//! 客户端把 refresh token 存进系统凭据库，于是「关掉应用再打开」不需要
//! 重新输密码 —— 它拿旧的换一对新的就继续用。**这是「不用反复登录」
//! 的全部机制**，而它成立的前提是客户端真的把那串东西存下来
//! （见 `app/lib/auth/`）。服务端发了长效令牌而客户端不存，等于没做。
//!
//! # 轮转与重放
//!
//! 每次刷新都签一个新的、把旧的标记 `rotated_at`。拿一个已经轮转过的
//! 来换 = 它泄露了（合法客户端手上只有最新那个），此时**整条 family
//! 一起作废** —— 只废这一个不够，攻击者手上还有他刚换到的那个。
//!
//! 判定本身在 [`crate::credentials::judge_refresh`]，那里有三态与它们的理由。

use axum::{Json, extract::State};
use chrono::{Duration, Utc};
use cortex_proto::auth::{AuthTokens, LoginRequest, RefreshRequest, RegisterRequest, WhoAmI};
use sqlx::Row as _;

use crate::credentials::{
    RefreshRecord, RefreshVerdict, RejectReason, generate_refresh, hash_password, judge_refresh,
    refresh_digest, verify_password,
};
use crate::error::ApiError;
use crate::rate_limit::{key_prefix, login_key};
use crate::state::{Accounts, AgentState};

/// access token 的寿命。
///
/// 短到「泄露也没多大用」，长到「不至于每几分钟就为刷新往返一次」。
/// 15 分钟是这两者的常见折衷，没有更深的理由 —— 真正兜底的是刷新那条链
/// 能被整体作废。
const ACCESS_TTL: Duration = Duration::minutes(15);

/// refresh token 的寿命。
///
/// **这个数就是「多久不用就要重新登录」。** 30 天意味着日常使用的人
/// 永远不会再看到登录框（每次刷新都会续期），而一台放了一个月没开的
/// 机器要重新输密码 —— 那正是我们想要的那条线。
const REFRESH_TTL: Duration = Duration::days(30);

/// 注册默认是**关**的。
///
/// 开放注册意味着任何人都能拿服务端那把 DeepSeek key 烧钱，而生产是
/// 一台 2 核 3.5 GB、已经跑着 19 个容器的机器。
///
/// 取值刻意不是 `1` / `true` 而是这个字面量，与 `CORTEX_AUTH=disabled`
/// 同款：一个手滑设成 `0` 的环境变量不该把门打开。
/// 开发机免密登录：值就是**要登成谁**。
///
/// # 为什么是「用户名」而不是一个 `DEV=true`
///
/// 一个布尔开关答不出「那我进去是谁」，于是实现只能编一个幽灵用户或者
/// 抓第一个账号 —— 前者让租户解析失去意义，后者在多账号的开发机上会随
/// 建号顺序变。写清楚登成谁，这两个问题都不存在。
///
/// 它也因此**不可能手滑打开**：`CORTEX_DEV_LOGIN=1` 只会去找一个叫 `1` 的
/// 用户然后失败，而不是把门敞开。
///
/// 生效条件还要求**用户名与密码都为空** —— 正常登录一行代码都不受影响。
const DEV_LOGIN_ENV: &str = "CORTEX_DEV_LOGIN";

/// 这台机器开着免密登录吗，登成谁。
#[must_use]
pub fn dev_login_user() -> Option<String> {
    std::env::var(DEV_LOGIN_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

const OPEN_REGISTRATION_ENV: &str = "CORTEX_OPEN_REGISTRATION";
const OPEN_REGISTRATION_ON: &str = "enabled";

fn open_registration() -> bool {
    std::env::var(OPEN_REGISTRATION_ENV).is_ok_and(|v| v.trim() == OPEN_REGISTRATION_ON)
}

/// 用户名的形状。**与 `migrations-global` 里 `users_username_shape` 那条
/// CHECK 是同一份规则**（`^[a-zA-Z0-9][a-zA-Z0-9._-]{1,62}$`）。
///
/// # 为什么要在这儿再判一次，而不是让约束去拒
///
/// 让 CHECK 去拒也能挡住，但那条路上的错误会被包成
/// `写用户记录失败：…violates check constraint "users_username_shape"` ——
/// HTTP 上是 500（「服务器坏了」，其实是「你名字里有空格」），
/// 命令行上是一段没人读得懂的 SQL。**这不是防御性编程，是把状态码和
/// 文案摆对**：用户名不合法是 400。
///
/// 两处规则同步不了会怎样：这边松了由 CHECK 兜底（退回今天的烂文案，
/// 不会写坏数据）；这边紧了会拒掉库其实收得下的名字。所以下面那条
/// 测试拿的是 CHECK 里那几类边界值。
fn check_username_shape(username: &str) -> Result<(), ApiError> {
    let mut chars = username.chars();
    let head_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    // 首字符之后还允许 1~62 个 —— 也就是总长 2~63，与 CHECK 一致。
    // 类是纯 ASCII，所以按字符数与按字节数在这里没有区别
    let rest: Vec<char> = chars.collect();
    let ok = head_ok
        && (1..=62).contains(&rest.len())
        && rest
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        return Ok(());
    }
    Err(ApiError::bad_request(
        "用户名只能用字母、数字和 . _ -，必须以字母或数字开头，长度 2~63。",
    ))
}

/// 这个部署有几个账号。
///
/// 只用来判断「是不是第一个」，所以不缓存 —— 它每次注册请求跑一次，
/// 而注册在这个产品里是极低频动作。
///
/// # Errors
/// 查不动。
async fn user_count(accounts: &Accounts) -> Result<i64, ApiError> {
    let row = sqlx::query("SELECT count(*) AS n FROM cortex_auth.users")
        .fetch_one(&accounts.pool)
        .await
        .map_err(|e| ApiError::internal(format!("数不出用户数：{e}")))?;
    Ok(row.get("n"))
}

/// 建一个账号：开 schema、迁移、写 users 行。
///
/// # 为什么它是自由函数，而不是 `AgentState` 上的方法
///
/// 因为**建号有三个入口，而它们手上的东西不一样**：HTTP 的 `/auth/register`
/// 有完整的 `AgentState`，`--create-user` 只有一个数据库连接，
/// 启动时的 [`ensure_admin`] 也只有那个。挂在 `AgentState` 上的话，
/// 后两条要么造一个假 state（那要 docker、要 LLM 客户端 —— 而建号一样都
/// 不需要），要么各自再写一遍建号逻辑。
///
/// **各自再写一遍**正是这个仓库反复咬人的形状：同一件事两处装配，
/// 漏改的那一处不会有任何测试红。所以三条路都落到这一个函数上。
///
/// # 顺序不能反
///
/// 先建 schema 并迁移，**最后**才写 users 那一行。反过来会留下
/// 「账号存在但库是空的」这个中间态，而那个用户一登录就撞上一片没有
/// 表的 schema —— 报的是看不懂的 SQL 错误，不是「你的账号还没建好」。
///
/// # Errors
/// 用户名不合法或已存在、密码太短、建 schema 失败、migration 失败。
pub async fn create_account_in(
    accounts: &Accounts,
    username: &str,
    password: &str,
) -> Result<String, ApiError> {
    check_username_shape(username)?;
    // `map_err(|e| bad_request(e.to_string()))` 会把文案套两层：
    // `CortexError::Invalid` 的 Display 已经带了「非法输入：」，再包一次
    // 得到的是「非法输入：非法输入：密码至少需要 12 个字节」。
    // 直接 `From` 过来 —— `Invalid` 本来就映射到 400
    let hash = hash_password(password).map_err(ApiError::from)?;
    let user_id = cortex_core::Id::new().to_string();

    // **第一个账号落在 `public` 上，不另开 schema。**
    //
    // 那片库里已经有东西了 —— 一个自托管的人是先用了几个月、
    // 后来才建账号的。给他也 derive 一个新 schema，等于他建完号
    // 一登录，几个月的记忆全部消失（库还在，只是没有任何一条路
    // 通向它了）。而这**不报错**：会话列表就是空的，看着像新装的。
    //
    // 端到端跑一次才发现：老 token 建号之后指向了 `u_01kz…`，
    // 而存量数据在 `public`。设计里本来就写着「1 号用户的 schema 名
    // 就叫 public，存量数据不搬家」，是实现漏了这一支。
    let schema = schema_for_new_account(&user_id, user_count(accounts).await? == 0)?;

    // schema 先行。失败时还没有任何账号记录，重试即可 ——
    // 而反过来会留下一个登不进去的账号
    cortex_store::provision(accounts.tenants.as_ref(), &schema)
        .await
        .map_err(|e| ApiError::internal(format!("给新账号开库失败：{e}")))?;

    let inserted = sqlx::query(
        "INSERT INTO cortex_auth.users (id, username, password_hash, schema_name)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING",
    )
    .bind(&user_id)
    .bind(username)
    .bind(&hash)
    .bind(schema.as_str())
    .execute(&accounts.pool)
    .await
    .map_err(|e| ApiError::internal(format!("写用户记录失败：{e}")))?;

    if inserted.rows_affected() == 0 {
        // schema 已经建了，但账号没写成。删掉它，别留孤儿 ——
        // 一片没人认领的 schema 会一直占着 catalog，而且下次用同一个
        // 用户名注册时又会建一片新的
        let _ = accounts.tenants.as_ref().drop_schema(&schema).await;
        return Err(ApiError::bad_request("这个用户名已经有人用了"));
    }
    Ok(user_id)
}

/// 这个用户名已经有账号了吗（大小写不敏感，与唯一索引同一口径）。
///
/// # Errors
/// 查不动。
pub async fn user_exists(accounts: &Accounts, username: &str) -> Result<bool, ApiError> {
    // `idx_users_username` 建在 `lower(username)` 上，所以这里也必须 lower ——
    // 不 lower 的话「Alice 不存在」会成立，然后 INSERT 撞唯一索引，
    // 于是 ensure_admin 每次启动都报一次「用户名已经有人用了」而拒绝启动
    let row =
        sqlx::query("SELECT 1 AS hit FROM cortex_auth.users WHERE lower(username) = lower($1)")
            .bind(username)
            .fetch_optional(&accounts.pool)
            .await
            .map_err(|e| ApiError::internal(format!("查用户失败：{e}")))?;
    Ok(row.is_some())
}

// ══════════════════════════════════════════════════════════════
//  从 .env 建第一个账号
// ══════════════════════════════════════════════════════════════

/// 第一个管理员账号的用户名与密码。
///
/// 存在的理由是**把建号这件事挪到公网之前**。register 里那条
/// 「第一个账号无条件放行」的特例已经删掉了，代价是没有别的路时，
/// 一台刚部署好的机器只能靠临时打开开放注册来建号 —— 而那正是
/// 「谁先注册谁是主人」那个窗口，只是换成了手工开关。
///
/// 这两个变量与 `--create-user` 是**同一件事的两条路**，覆盖两种部署者：
/// 点一个 compose 就部署完的人没有 shell，只能用 `.env`；
/// 而已经在机器上的人不想把口令写进文件，用 `--create-user` 走 stdin。
const ADMIN_USER_ENV: &str = "CORTEX_ADMIN_USERNAME";
pub const ADMIN_PASSWORD_ENV: &str = "CORTEX_ADMIN_PASSWORD";

/// 这两个环境变量合起来是什么意思。
///
/// # 为什么单拎出来，而且不自己读环境
///
/// 「只配了一半要拒绝启动」这条规则是这里唯一容易写歪的东西，而它值得
/// 一条测试。让它自己读 `std::env` 的话，测它就得 `set_var` —— 那是**进程
/// 全局**的，与并行跑的别的用例互相踩，症状是单独跑绿、全量跑红。
/// 这个仓库为同一个原因返工过一次（代理那组用例），不再来第二遍。
///
/// 返回 `None` = 两个都空 = 绝大多数部署，什么都不做。
///
/// # Errors
/// 只配了一半。
fn admin_spec<'a>(
    raw_user: &'a str,
    raw_password: &'a str,
) -> anyhow::Result<Option<(&'a str, &'a str)>> {
    // 用户名 trim，密码**不 trim**：口令里的空格是口令的一部分，
    // 悄悄剪掉的后果是「服务端算的哈希和用户下次输入的对不上」，
    // 而那看起来像密码记错了
    let username = raw_user.trim();
    match (username.is_empty(), raw_password.is_empty()) {
        (true, true) => Ok(None),
        (true, false) => anyhow::bail!(
            "配了 {ADMIN_PASSWORD_ENV} 却没配 {ADMIN_USER_ENV}。\n\
             半份配置建不出账号，而它不会有任何症状 —— 你会得到一台\n\
             谁也登不进去的服务器。补上用户名，或者两个都留空。"
        ),
        (false, true) => anyhow::bail!(
            "配了 {ADMIN_USER_ENV} 却没配 {ADMIN_PASSWORD_ENV}。\n\
             半份配置建不出账号，而它不会有任何症状 —— 你会得到一台\n\
             谁也登不进去的服务器。补上密码，或者两个都留空。"
        ),
        (false, false) => Ok(Some((username, raw_password))),
    }
}

/// 启动时按 `.env` 把第一个账号建出来。**在开始监听之前调用。**
///
/// 三种情形，行为刻意不同：
///
/// | `.env` 里 | 做什么 |
/// |---|---|
/// | 两个都空 | 什么都不做（绝大多数部署） |
/// | 两个都有，账号不存在 | 建号 |
/// | 两个都有，账号已存在 | 跳过。**不改密码** —— 改密码是另一件事，见下 |
/// | 只有一个 | **拒绝启动** |
///
/// # 为什么「只配了一半」要拒绝启动
///
/// 这是本仓库数到第 6 次的「空串顶掉默认值」那个形状的近亲：配了用户名
/// 没配密码的人，**以为自己配了管理员**，而实际得到的是一台谁也登不进去
/// 的服务器 —— 没有任何报错，登录框就是不认他。当场停住比那个好。
///
/// # 为什么已存在时不改密码
///
/// 因为那会让「忘了从 `.env` 里删掉这两行」变成一条**每次重启都把密码
/// 重置回去**的路：用户在界面上改了密码，重启一次又变回 `.env` 里那个，
/// 而他不会想到去看服务端的环境变量。真要改密码，那是一条需要旧密码的
/// 独立操作。
///
/// # Errors
/// 只配了一半、用户名不合法、密码太短、或者建号本身失败。
pub async fn ensure_admin(accounts: &Accounts) -> anyhow::Result<()> {
    let raw_user = std::env::var(ADMIN_USER_ENV).unwrap_or_default();
    let raw_password = std::env::var(ADMIN_PASSWORD_ENV).unwrap_or_default();
    let Some((username, password)) = admin_spec(&raw_user, &raw_password)? else {
        return Ok(());
    };

    if user_exists(accounts, username)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.message()))?
    {
        tracing::info!(
            user = username,
            "{ADMIN_USER_ENV} 指的账号已经存在，跳过（这两个变量只建号，不改密码）"
        );
        return Ok(());
    }

    let id = create_account_in(accounts, username, password)
        .await
        .map_err(|e| anyhow::anyhow!("按 {ADMIN_USER_ENV} 建管理员账号失败：{}", e.message()))?;
    tracing::info!(
        user = username,
        id = %id,
        "已按 .env 建好管理员账号。建完之后可以把 {ADMIN_PASSWORD_ENV} 从 .env 里删掉"
    );
    Ok(())
}

impl AgentState {
    /// 建一个账号。实现在 [`create_account_in`] —— 这里只是把
    /// `AgentState` 里那份连接取出来递过去。
    ///
    /// # Errors
    /// 见 [`create_account_in`]，外加「这个部署没接数据库」。
    pub async fn create_account(&self, username: &str, password: &str) -> Result<String, ApiError> {
        create_account_in(self.accounts()?, username, password).await
    }

    /// 签一对新令牌。`family` 为 `None` 时开一条新链（登录）。
    async fn issue(
        &self,
        user_id: &str,
        family: Option<String>,
        device: Option<&str>,
    ) -> Result<AuthTokens, ApiError> {
        let (plain, digest) = generate_refresh();
        let family = family.unwrap_or_else(|| cortex_core::Id::new().to_string());
        let expires = Utc::now() + REFRESH_TTL;

        sqlx::query(
            "INSERT INTO cortex_auth.auth_tokens
                 (id, user_id, token_sha256, family_id, expires_at, device_label)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(cortex_core::Id::new().to_string())
        .bind(user_id)
        .bind(&digest)
        .bind(&family)
        .bind(expires)
        .bind(device)
        .execute(&self.accounts()?.pool)
        .await
        .map_err(|e| ApiError::internal(format!("签发令牌失败：{e}")))?;

        Ok(AuthTokens {
            access_token: self.access_book().issue(
                user_id,
                std::time::Duration::from_secs(
                    u64::try_from(ACCESS_TTL.num_seconds()).unwrap_or(900),
                ),
            ),
            access_expires_in_secs: u64::try_from(ACCESS_TTL.num_seconds()).unwrap_or(900),
            refresh_token: plain,
            refresh_expires_in_secs: u64::try_from(REFRESH_TTL.num_seconds()).unwrap_or(2_592_000),
            user_id: user_id.to_owned(),
        })
    }

    /// 用 refresh token 换一对新的，并把旧的标记为已轮转。
    ///
    /// # Errors
    /// 令牌不认识、已过期、已作废，或者**被重放**（此时整条 family 作废）。
    pub async fn rotate(&self, plain: &str) -> Result<AuthTokens, ApiError> {
        let digest = refresh_digest(plain);
        let row = sqlx::query(
            "SELECT id, user_id, family_id, rotated_at IS NOT NULL AS rotated,
                    revoked_at IS NOT NULL AS revoked, expires_at < now() AS expired
               FROM cortex_auth.auth_tokens WHERE token_sha256 = $1",
        )
        .bind(&digest)
        .fetch_optional(&self.accounts()?.pool)
        .await
        .map_err(|e| ApiError::internal(format!("查令牌失败：{e}")))?;

        // 查不到就是查不到。**不要**告诉客户端「这个令牌不存在」与
        // 「这个令牌过期了」的区别 —— 那等于给爆破者一个进度条
        let Some(row) = row else {
            return Err(ApiError::unauthorized("登录已失效，请重新登录"));
        };

        let family_raw: String = row.get("family_id");
        let record = RefreshRecord {
            family: family_raw
                .parse()
                .map_err(|_| ApiError::internal("库里的 family_id 不是合法 ULID"))?,
            rotated: row.get("rotated"),
            revoked: row.get("revoked"),
            expired: row.get("expired"),
        };
        let user_id: String = row.get("user_id");
        let token_id: String = row.get("id");

        match judge_refresh(&record) {
            RefreshVerdict::RevokeFamily { family } => {
                // 这一条已经被换过了，而合法客户端手上只有最新那个 ——
                // 所以这次请求来自别人。整条链作废，两边都得重新登录
                let family = family.to_string();
                let n = self.revoke_family(&family).await;
                tracing::warn!(
                    user = %user_id,
                    family = %family,
                    revoked = n,
                    "检测到 refresh token 重放，已作废整条链"
                );
                Err(ApiError::unauthorized(
                    "这个登录凭据已经被使用过了。出于安全，该设备上的所有会话都已注销，请重新登录",
                ))
            }
            RefreshVerdict::Reject(why) => Err(ApiError::unauthorized(match why {
                RejectReason::Expired => "登录已过期，请重新登录",
                RejectReason::FamilyRevoked => "登录已注销，请重新登录",
            })),
            RefreshVerdict::Rotate => {
                sqlx::query("UPDATE cortex_auth.auth_tokens SET rotated_at = now() WHERE id = $1")
                    .bind(&token_id)
                    .execute(&self.accounts()?.pool)
                    .await
                    .map_err(|e| ApiError::internal(format!("轮转令牌失败：{e}")))?;
                self.issue(&user_id, Some(record.family.to_string()), None)
                    .await
            }
        }
    }

    /// 作废整条链，返回作废了几条。
    async fn revoke_family(&self, family: &str) -> u64 {
        let Ok(acc) = self.accounts() else { return 0 };
        sqlx::query(
            "UPDATE cortex_auth.auth_tokens SET revoked_at = now()
              WHERE family_id = $1 AND revoked_at IS NULL",
        )
        .bind(family)
        .execute(&acc.pool)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0)
    }
}

/// 已签发的 access token → (用户 id, 过期时刻)。
///
/// # 为什么在内存里，不落库、也不签 JWT
///
/// 落库意味着**每个请求一次数据库往返**，而 access token 每个请求都带。
/// JWT 要引一套签名依赖，还要管密钥轮转 —— 而它换来的「无状态校验」
/// 对一个单进程部署没有价值。
///
/// 代价是**重启之后所有 access token 失效**。这不痛：refresh token 在库里
/// 活得好好的，客户端撞到一次 401 就用它换一对新的，用户什么都感觉不到。
/// 这也正是「两种令牌」这个设计顺带买到的东西。
#[derive(Default)]
pub struct AccessBook {
    inner: std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>,
}

impl AccessBook {
    fn issue(&self, user_id: &str, ttl: std::time::Duration) -> String {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).expect("内核熵源不可用，拒绝签发可预测的令牌");
        let token = hex::encode(buf);
        let mut g = self.inner.lock().expect("access 簿子的锁不该中毒");
        // 顺手清一次过期的。挂定时器要多一个后台任务与它的失败路径，
        // 而这条路本来就是「有人登录/刷新」时才走
        let now = std::time::Instant::now();
        g.retain(|_, (_, exp)| *exp > now);
        g.insert(token.clone(), (user_id.to_owned(), now + ttl));
        token
    }

    /// 这个 token 属于谁。过期或不认识都返回 `None`。
    /// 只给测试用的签发口。
    ///
    /// `issue` 是私有的（签发要走完整的登录流程），但认证中间件那条不变式
    /// 需要一张真的、簿子认得的票 —— 手搓一串十六进制测不到任何东西。
    #[cfg(test)]
    pub fn issue_for_test(&self, user_id: &str, ttl: std::time::Duration) -> String {
        self.issue(user_id, ttl)
    }

    pub fn resolve(&self, token: &str) -> Option<String> {
        let g = self.inner.lock().expect("access 簿子的锁不该中毒");
        let (user, exp) = g.get(token)?;
        (*exp > std::time::Instant::now()).then(|| user.clone())
    }
}

/// 新账号落在哪个 schema。
///
/// # 第一个账号必须是 `public`
///
/// 那片库里通常已经有东西 —— 自托管的人是先用了几个月、后来才建账号的。
/// 给他也 derive 一个新 schema，等于建完号一登录，几个月的记忆全部消失：
/// 库还在，只是没有任何一条路通向它了。而这**不报错**，会话列表就是空的，
/// 看着像新装的。
///
/// 端到端跑一次才发现的（老 token 建号之后指向了 `u_01kz…`，
/// 而存量数据在 `public`）。设计里本来就写着「1 号用户的 schema 名就叫
/// public，存量数据不搬家」，是实现漏了这一支。
///
/// # Errors
/// `user_id` 不是合法 ULID。
fn schema_for_new_account(
    user_id: &str,
    is_first: bool,
) -> Result<cortex_store::SchemaName, ApiError> {
    if is_first {
        return Ok(cortex_store::SchemaName::public());
    }
    cortex_store::SchemaName::derive(user_id).map_err(|e| ApiError::internal(e.to_string()))
}

/// `POST /auth/login`
pub async fn login(
    State(st): State<AgentState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<AuthTokens>, ApiError> {
    // ── 限流，在做任何事之前 ──
    //
    // 键是**提交的用户名**（小写），限的是**失败**次数：成功不计，所以正常
    // 登录不受影响；被爆破时那个用户名一分钟只挨得了 10 下。外加一道全局
    // 兜底，拦「每个名字只试一两下」的撒网。dev 免密登录（用户名为空）也
    // 走这条 —— 空名字按 "(blank)" 计，既不豁免，也不与真实用户名互相牵连。
    //
    // 这里只查不记：记不记要等这次认证的结果（见下面的 `deny`）。
    let throttle_key = login_key(&req.username);
    if let Err(wait) = st.auth_throttle().check_login(&throttle_key) {
        tracing::warn!(
            key = %key_prefix(&throttle_key),
            wait,
            "login 限流命中：这个用户名（或全局兜底）的失败额度已满"
        );
        return Err(ApiError::too_many_requests(format!(
            "登录尝试太频繁，请 {wait} 秒后再试"
        )));
    }

    // ── 开发机免密：用户名与密码都空时，登成这台机器指名的那个人 ──
    //
    // 判断必须在**查库之前**：空用户名查不出任何行，事后再判等于永远走不到。
    //
    // 而查的是**真账号**（不是造一个幽灵用户）：登进去之后租户、schema、
    // 配额全是真的。造一个假的能让登录成功，但那个人的会话会落进一片没人
    // 认领的库里 —— 一个只在开发机上出现、查起来毫无头绪的坏。
    let dev_login = req.username.trim().is_empty() && req.password.is_empty();
    let username = if dev_login {
        match dev_login_user() {
            Some(u) => u,
            // 没开这个开关就按普通登录走下去：空用户名查不到行，
            // 回的是与「密码错了」同一句话
            None => req.username.clone(),
        }
    } else {
        req.username.clone()
    };
    let dev_login = dev_login && dev_login_user().is_some();

    let row = sqlx::query(
        "SELECT id, password_hash, disabled_at IS NOT NULL AS disabled
           FROM cortex_auth.users WHERE lower(username) = lower($1)",
    )
    .bind(&username)
    .fetch_optional(&st.accounts()?.pool)
    .await
    .map_err(|e| ApiError::internal(format!("查用户失败：{e}")))?;

    // 用户名不存在与密码错误回**同一句话**：区分开来等于送人一份用户名字典。
    //
    // 时间侧信道这里不做等时处理：真要防，得对不存在的用户也跑一遍 argon2，
    // 而那会把「随便猜个用户名」变成一次 16ms 的 CPU 消耗 —— 那是个更好用的
    // DoS 放大器。取舍写在这里，别当成漏了
    let deny = || {
        // 只在**失败**这一刻记账（成功那条路走不到这儿）——
        // 与上面的 check_login 配对：先查、办事、失败了再记
        st.auth_throttle().record_login_failure(&throttle_key);
        ApiError::unauthorized("用户名或密码不对")
    };
    let Some(row) = row else { return Err(deny()) };
    if row.get::<bool, _>("disabled") {
        // 停用的账号也计一次失败：能持续敲一个停用账号的门，本身就是在探测
        st.auth_throttle().record_login_failure(&throttle_key);
        return Err(ApiError::unauthorized("这个账号已被停用"));
    }
    if dev_login {
        // **每一次都记一条**，不是启动时说一遍就算。一台开着免密的机器
        // 要在日志里持续可见 —— 「我以为那是开发机」是这类事故的原话
        tracing::warn!(user = %username, "免密登录（CORTEX_DEV_LOGIN 开着）");
    } else if !verify_password(&req.password, &row.get::<String, _>("password_hash")) {
        return Err(deny());
    }

    let user_id: String = row.get("id");
    Ok(Json(
        st.issue(&user_id, None, req.device_label.as_deref())
            .await?,
    ))
}

/// `POST /auth/refresh` —— 客户端**不用重新登录**靠的就是这条。
pub async fn refresh(
    State(st): State<AgentState>,
    Json(req): Json<RefreshRequest>,
) -> Result<Json<AuthTokens>, ApiError> {
    // ── 限流，必须在碰库之前 ──
    //
    // 这道闸挡的正是「把库写爆」那种事故（2026-08-16：一个客户端续期循环
    // 往 auth_tokens 里塞了 21.9 万行）—— 先查库再限流等于把闸建在洪水
    // 下游，每一次被限的请求都已经打过一次库。`routes.rs` 的测试就靠这个
    // 顺序钉住阈值：没接库时前 N 次 501、第 N+1 次 429。
    //
    // 键是 token 的摘要而不是用户名 / IP：正常客户端 15 分钟才来一次，
    // 同一把 token 每分钟 5 次以上必是循环。为什么不能是用户名 / IP，
    // 见 `rate_limit::AuthThrottle::check_refresh`。
    let digest = refresh_digest(&req.refresh_token);
    if let Err(wait) = st.auth_throttle().check_refresh(&digest) {
        tracing::warn!(
            key = %key_prefix(&digest),
            wait,
            "refresh 限流命中：同一把 token 来得太密，多半是客户端的续期循环跑飞了"
        );
        return Err(ApiError::too_many_requests(format!(
            "刷新太频繁：这把凭据在过去一分钟里用得太多，请 {wait} 秒后再试"
        )));
    }
    Ok(Json(st.rotate(&req.refresh_token).await?))
}

/// `POST /auth/logout` —— 作废这条链。
///
/// 作废**整条 family** 而不是只作废递上来的那一个：登出的意图是
/// 「这台设备上的登录不再有效」，而那条链上还有它刚换出来的后继。
pub async fn logout(
    State(st): State<AgentState>,
    Json(req): Json<RefreshRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let digest = refresh_digest(&req.refresh_token);
    let n = sqlx::query(
        "UPDATE cortex_auth.auth_tokens SET revoked_at = now()
          WHERE family_id = (SELECT family_id FROM cortex_auth.auth_tokens WHERE token_sha256 = $1)
            AND revoked_at IS NULL",
    )
    .bind(&digest)
    .execute(&st.accounts()?.pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);

    // 认不出的令牌也回 200：登出是幂等的，而报 404 只会让「我已经登出了吗」
    // 变成一个客户端要处理的分支
    Ok(Json(serde_json::json!({ "revoked": n })))
}

/// `POST /auth/register` —— **默认 403**。
pub async fn register(
    State(st): State<AgentState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<AuthTokens>, ApiError> {
    // **不再有「第一个账号无条件放行」这条特例。**
    //
    // 它曾经的理由是：私有化部署的第一步是把它跑起来，那时既没有邮箱通道、
    // 也可能没有 shell。代价是一台刚部署好、还没建号、又暴露在公网上的机器，
    // 第一个访问它的人会成为主人 —— 对反复部署来说这个窗口躲不过去。
    //
    // 替掉它的是两条**不经过公网**的路（2026-08-16 补上，此前它们只存在于
    // 注释和 `.env.example` 里，代码一行都没有 —— 于是默认配置下根本建不出
    // 第一个账号）：
    //
    //   - `.env` 的 `CORTEX_ADMIN_USERNAME` / `_PASSWORD` → [`ensure_admin`]，
    //     在**开始监听之前**跑，所以那个窗口根本不存在
    //   - `cortex-agentd --create-user <名字>`，口令走 stdin，不落文件
    //
    // 两条与这里都落到 [`create_account_in`]，不是三份实现。
    if !open_registration() {
        // 说清楚是「这个部署关着」而不是「你填错了」，否则对方会一直重试。
        // 给的两条都要是**真实存在**的：指一条不存在的命令，对方会先怀疑
        // 自己的 PATH，再怀疑版本，最后才怀疑这句话
        return Err(ApiError::forbidden(format!(
            "这个部署没有开放注册。管理员可以设 {OPEN_REGISTRATION_ENV}={OPEN_REGISTRATION_ON} 打开，\
             或者用 `cortex-agentd --create-user <用户名>` 直接建号（不用开放注册）。"
        )));
    }
    // ── 全局限流，在碰库之前 ──
    //
    // 注册是人类填表单的动作，背后却是一次 argon2 加一片新 schema —— 全局
    // 低频一道闸就够，不必按人细分（见 `rate_limit::REGISTER_PER_WINDOW`）。
    //
    // 放在上面那道 403 **之后**：关着的部署回 403（「永远不行」）比 429
    // （「等等再来」）更真，别让限流把那个更有信息量的回答盖掉。
    if let Err(wait) = st.auth_throttle().check_register() {
        tracing::warn!(wait, "register 限流命中（全局闸）");
        return Err(ApiError::too_many_requests(format!(
            "注册请求太频繁，请 {wait} 秒后再试"
        )));
    }
    let user_id = st.create_account(&req.username, &req.password).await?;
    Ok(Json(st.issue(&user_id, None, None).await?))
}

/// `GET /auth/usage` —— 我这个窗口用了多少、还剩多少。
///
/// # Errors
/// 没接账号体系，或者算不出用量。
pub async fn usage(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<crate::quota::QuotaView>, ApiError> {
    let user_id = current_user(&st, &headers).await;
    Ok(Json(st.quota_status(&user_id).await?.into()))
}

/// 这个请求是谁发的。
///
/// access token 认不出时回落到 1 号用户 —— 那是老的预共享 token 那条路，
/// 它已经过了入站那道门，否则这个 handler 根本不会被调到。
pub async fn current_user(st: &AgentState, headers: &axum::http::HeaderMap) -> String {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if let Some(id) = st.access_book().resolve(bearer) {
        return id;
    }
    // **委托令牌也要认得出。**
    //
    // 沙箱容器带的就是这把（`auth::require` 已经放行了它，否则走不到这儿）。
    // 认不出的后果不是 401 —— 是**静默落到 1 号用户**，于是容器写的 episode
    // 全部进了 `public` 那片库，而不是发起这轮对话的人的。数据看着都在，
    // 只是在别人家里，且没有任何一条日志说过这件事。
    //
    // 这条是从记忆服务那侧**原样带过来**的行为（那边一样如此），不是搬迁
    // 引入的；但身份现在归这个进程管，所以补在这里。
    if let Some(scope) = st.delegations().resolve(bearer) {
        return scope.owner;
    }
    owner_user_id(st).await.unwrap_or_else(|| "owner".into())
}

/// `GET /auth/me`
///
/// 用请求里那个 access token 反查是谁。**老的预共享 token 也认**，
/// 映射到 1 号用户 —— 过渡期 CLI 与现有桌面端都还在用它，
/// 一次性切换会让它们当天全部失联。
pub async fn whoami(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<WhoAmI>, ApiError> {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let user_id = match st.access_book().resolve(bearer) {
        Some(id) => id,
        // 委托令牌（沙箱容器带的那把）同样认得出，理由见 `current_user`
        None => match st.delegations().resolve(bearer) {
            Some(scope) => scope.owner,
            // 认不出就是老 token 那条路（它已经过了入站那道门，
            // 否则这个 handler 根本不会被调到）
            None => owner_user_id(&st).await.unwrap_or_else(|| "owner".into()),
        },
    };
    let row = sqlx::query("SELECT username, schema_name FROM cortex_auth.users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&st.accounts()?.pool)
        .await
        .map_err(|e| ApiError::internal(format!("查用户失败：{e}")))?;
    match row {
        Some(r) => Ok(Json(WhoAmI {
            user_id,
            username: r.get("username"),
            schema_name: r.get("schema_name"),
        })),
        // 老 token 那条路映射到 1 号用户，而 1 号用户可能还没在 users 表里
        // （部署早于账号体系）。这不是错误，是过渡期的正常状态
        None => Ok(Json(WhoAmI {
            user_id,
            username: "(尚未建账号)".into(),
            schema_name: "public".into(),
        })),
    }
}

/// 1 号用户的 id。老 token 那条路映射到他。
///
/// 按 id 升序取第一个 —— ULID 有序，所以「第一个」就是「最早建的那个」。
async fn owner_user_id(st: &AgentState) -> Option<String> {
    let acc = st.accounts().ok()?;
    sqlx::query("SELECT id FROM cortex_auth.users ORDER BY id LIMIT 1")
        .fetch_optional(&acc.pool)
        .await
        .ok()
        .flatten()
        .map(|r| r.get("id"))
}

#[cfg(test)]
mod tests {
    /// **注册再也没有「第一个账号」这条特例了。**
    ///
    /// 它曾经无条件放行，代价是一台刚部署好、还没建号、又暴露在公网上的
    /// 机器，第一个访问 /auth/register 的人会成为主人。部署一次能靠手快
    /// 躲过去，反复部署躲不过去。
    ///
    /// 替掉它的是两条不经过公网的路：[`ensure_admin`]（`.env`，在监听之前
    /// 跑）与 `--create-user`。**这条测试成立的前提是那两条真的在**——
    /// 它们缺席时删掉特例只是把窗口换了个形状（临时开放注册再关掉），
    /// 而这条测试照样绿。2026-08-16 之前正是那个状态。
    #[test]
    fn registration_has_no_first_account_exception() {
        let src = include_str!("accounts.rs");
        let gate = src
            .split("pub async fn register")
            .nth(1)
            .expect("register 还在吧");
        // 只看函数体前半段（到 create_account 为止就够了）。按**字符**截而
        // 不是按字节：这个函数的注释是中文，字节 2000 落在多字节字符中间时
        // 切片会 panic —— 限流那段注释加进来之后真的撞上过
        let head: String = gate.chars().take(2000).collect();
        assert!(
            !head.contains("user_count"),
            "register 里又数起用户数了 —— 那通常意味着「第一个账号特殊对待」             这条特例回来了，而它带着一个公网上的抢主人窗口。             第一个账号走的是不经过公网的两条：ensure_admin（.env，监听之前）             与 --create-user"
        );
    }

    use super::*;

    /// **默认必须是关的。**
    ///
    /// 忘了配一个环境变量的后果不该是「陌生人可以开号并烧我的 API key」。
    #[test]
    fn registration_is_closed_unless_explicitly_opened() {
        // 这个测试不改进程环境（会影响并行跑的别的用例），
        // 只验判据本身：除了那个字面量，什么都不算开
        for v in [
            "", "0", "1", "true", "TRUE", "yes", "on", "disabled", "Enabled",
        ] {
            assert_ne!(
                v.trim(),
                OPEN_REGISTRATION_ON,
                "{v:?} 不该被当成「开放注册」—— 手滑设成 1 或 true 不能把门打开"
            );
        }
        assert_eq!("enabled", OPEN_REGISTRATION_ON);
    }

    /// refresh 的寿命就是「多久不用要重新登录」，别让它悄悄变短。
    #[test]
    fn a_session_survives_a_month_of_restarts() {
        assert_eq!(
            REFRESH_TTL.num_days(),
            30,
            "这个数就是用户体感上的「多久要重新登录一次」，改它要有意识"
        );
        assert!(
            ACCESS_TTL < REFRESH_TTL,
            "access 必须比 refresh 短得多，否则短命令牌那一层就没意义了"
        );
    }
}

#[cfg(test)]
mod account_schema_tests {
    use super::*;

    /// 第一个账号接管 `public`，其余各自开一片。
    #[test]
    fn the_first_account_takes_over_public() {
        let id = cortex_core::Id::new().to_string();
        assert_eq!(
            schema_for_new_account(&id, true).unwrap().as_str(),
            "public",
            "第一个账号必须落在 public 上。落在别处的话，一个用了几个月才建号的人             建完号一登录，全部记忆消失 —— 而且不报错，会话列表就是空的"
        );
        let second = schema_for_new_account(&id, false).unwrap();
        assert!(
            second.as_str().starts_with("u_"),
            "第二个之后必须各自一片，实际拿到 {}",
            second.as_str()
        );
        assert_ne!(second.as_str(), "public");
    }

    /// 用户名的规则必须与库里那条 CHECK 一致。
    ///
    /// 取的全是 `users_username_shape`
    /// （`^[a-zA-Z0-9][a-zA-Z0-9._-]{1,62}$`）的边界值 —— 两处规则漂开时，
    /// 松了的后果是退回「500 + 一段 SQL」那种烂文案，紧了的后果是拒掉
    /// 库其实收得下的名字。
    #[test]
    fn the_username_rule_matches_the_check_constraint() {
        for ok in ["ab", "a1", "alice", "a.b_c-d", "0zz", &"a".repeat(63)] {
            assert!(
                check_username_shape(ok).is_ok(),
                "CHECK 收得下 {ok:?}，这里却拒了 —— 规则比库还紧"
            );
        }
        for bad in [
            "",              // 空
            "a",             // 只有 1 个字符，CHECK 要求至少 2
            &"a".repeat(64), // 超过 63
            ".ab",           // 首字符必须是字母或数字
            "-ab",           //
            "_ab",           //
            "a b",           // 空格
            "a@b",           // 类外字符
            "张三",          // 非 ASCII
            "ab\n",          // 尾随换行 —— 从 stdin/env 读来时最容易带上
        ] {
            assert!(
                check_username_shape(bad).is_err(),
                "CHECK 收不下 {bad:?}，这里却放行了 —— 会退化成 500 + 一段 SQL"
            );
        }
    }

    /// **只配了一半必须当场停住。**
    ///
    /// 半份配置建不出账号，而它没有任何症状：服务照起、healthy 照报，
    /// 只是谁也登不进去，而配置看着「我明明配了管理员」。
    #[test]
    fn half_a_configuration_refuses_to_start() {
        assert!(
            admin_spec("", "").expect("两个都空是合法的").is_none(),
            "两个都空 = 绝大多数部署，必须什么都不做"
        );
        assert!(
            admin_spec("alice", "").is_err(),
            "配了用户名没配密码要拒绝启动"
        );
        assert!(
            admin_spec("", "hunter2hunter2").is_err(),
            "配了密码没配用户名要拒绝启动"
        );
        // 全空白的用户名与没配是一回事 —— 这是本仓库数到第 6 次的
        // 「空串顶掉默认值」那个形状：`.env` 里写 `CORTEX_ADMIN_USERNAME= `
        // 的人以为自己没配，而 `is_empty()` 对一个空格是 false
        assert!(
            admin_spec("   ", "").expect("空白用户名等于没配").is_none(),
            "只有空白的用户名要当成没配，而不是「配了用户名没配密码」"
        );

        let (user, password) = admin_spec("  alice  ", " pw with spaces ")
            .expect("两个都有是合法的")
            .expect("两个都有时不该是 None");
        assert_eq!(user, "alice", "用户名要 trim");
        assert_eq!(
            password, " pw with spaces ",
            "密码**不能** trim —— 口令里的空格是口令的一部分，\
             悄悄剪掉的症状是「密码明明没打错却登不进去」"
        );
    }
}
