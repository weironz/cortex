//! 连接池与写事务入口。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::error::Result;
use crate::model::{EpisodeAttachment, SessionEvent, SessionState};
use crate::txn::WriteTxn;

/// 编译期嵌入的 migration 集合。
///
/// 用 `migrate!` 而非运行时读目录：二进制自带 schema，部署时不必带上 `migrations/`。
/// 注意它只是**读**文件，不需要编译期数据库连接（`query!` 宏才需要，本 crate
/// 刻意不用 —— 见 crate 级文档）。
static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// **整个部署只跑一遍**的那一套：账号、令牌、邀请、用量。
///
/// 与 [`MIGRATOR`] 分开是多租户逼出来的：那一套每个租户各跑一遍，
/// 而这一套是跨租户的。混在一起时，新租户建号会跑到 `cortex_auth.users`
/// 那条上撞 `relation already exists` —— 而失败点在建号中途。
static GLOBAL_MIGRATOR: Migrator = sqlx::migrate!("../../migrations-global");

/// 默认连接池上限。写事务被 advisory lock 串行化，池子不需要很大；
/// 富余的连接留给读路径（四路召回会并发发多条查询）。
const DEFAULT_MAX_CONNECTIONS: u32 = 16;

/// 存储层句柄。内部是 `Arc` 语义的连接池，克隆代价极低。
#[derive(Clone, Debug)]
pub struct Store {
    pool: PgPool,
    /// 这个池连到哪个租户的 schema。
    ///
    /// 带着它是为了两样**原来是全局单例**的东西：写事务的 advisory lock 键，
    /// 与 sync 的 NOTIFY 通道名。放在 `Store` 上而不是每次传参 ——
    /// 传参意味着每个调用点都可能传错，而传错的后果是「两个租户共用一把锁」
    /// 或者「订阅了别人的通道」。
    schema: crate::tenant::SchemaName,
}

impl Store {
    /// 按默认参数连接。
    pub async fn connect(database_url: &str) -> Result<Self> {
        Self::connect_with(
            PgPoolOptions::new().max_connections(DEFAULT_MAX_CONNECTIONS),
            database_url,
        )
        .await
    }

    /// 按自定义池参数连接。
    pub async fn connect_with(options: PgPoolOptions, database_url: &str) -> Result<Self> {
        Ok(Self::from_pool(options.connect(database_url).await?))
    }

    /// 接管一个已建好的连接池。测试用独立 schema 时走这条路。
    ///
    /// schema 按 `public` 算 —— 单用户部署与测试 harness 都落在那儿。
    /// 多租户走 [`Self::from_pool_in`]。
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            schema: crate::tenant::SchemaName::public(),
        }
    }

    /// 接管一个**已经把 `search_path` 焊好**的池，并记住它是谁的。
    #[must_use]
    pub fn from_pool_in(pool: PgPool, schema: crate::tenant::SchemaName) -> Self {
        Self { pool, schema }
    }

    /// 这个池服务的租户。
    #[must_use]
    pub fn schema(&self) -> &crate::tenant::SchemaName {
        &self.schema
    }

    /// 把这个租户的 schema 迁到最新。
    ///
    /// 只跑 [`MIGRATOR`]（`migrations/`，每租户一份的记忆表）。
    /// 全局那套见 [`Self::migrate_global`]。
    ///
    /// # Errors
    /// 哪条 migration 跑不过。
    pub async fn migrate(&self) -> Result<()> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// 跑全局那一套（`cortex_auth`）。整个部署一次，不是每租户一次。
    ///
    /// # 为什么要单独连一次而不是用现成的池
    ///
    /// 它的 `_sqlx_migrations` 必须落在 `cortex_auth` 里，否则会与租户那套
    /// 在 `public` 里的版本表撞在一起 —— 两套各有各的版本号，混在一张表里
    /// 的表现是「某一套被认为已经应用过」，也就是**表没建但以为建了**。
    ///
    /// 而 `search_path` 是连接级属性，改不了现成的池。
    ///
    /// # Errors
    /// 连不上、建不出 `cortex_auth`，或者哪条 migration 跑不过。
    pub async fn migrate_global(database_url: &str) -> Result<()> {
        use std::str::FromStr as _;

        // 先把 schema 建出来：`_sqlx_migrations` 要落在里面，而那发生在
        // 第一条 migration 之前 —— 让 migration 自己建就晚了一步
        let boot = PgPoolOptions::new()
            .max_connections(1)
            .connect(database_url)
            .await?;
        sqlx::query("CREATE SCHEMA IF NOT EXISTS cortex_auth")
            .execute(&boot)
            .await?;
        boot.close().await;

        let options = sqlx::postgres::PgConnectOptions::from_str(database_url)
            .map_err(|e| crate::error::StoreError::Invalid(format!("DATABASE_URL 不合法：{e}")))?
            // public 留在尾部：`ulid` / `sha256` 两个 DOMAIN 装在那儿
            .options([("search_path", "cortex_auth,public")]);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let result = GLOBAL_MIGRATOR.run(&pool).await;
        pool.close().await;
        result?;
        Ok(())
    }

    /// 探活。
    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// 关闭连接池，等待在途查询结束。
    pub async fn close(&self) {
        self.pool.close().await;
    }

    #[doc(hidden)]
    pub fn debug_pool(&self) -> &PgPool {
        &self.pool
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 在一个**受纪律约束**的写事务里执行 `f`。
    ///
    /// 事务开头即取 `pg_advisory_xact_lock(4272)`，把并发写事务串行化 ——
    /// 这保证 `sync_log.seq` 的顺序等于对读端的可见顺序，游标推进永不漏行。
    /// 详见 [`crate::txn`] 的模块文档。
    ///
    /// `f` 返回 `Ok` 则提交，返回 `Err` 则回滚。`f` 拿到的
    /// [`WriteTxn`] **只有写方法**：需要先查后写的流程（矛盾消解、实体消解、
    /// 合并环检测）应在进事务前读完算完 —— 取号事务必须短小、纯写。
    ///
    /// ```no_run
    /// # use cortex_store::{Store, NewEpisode};
    /// # async fn demo(store: &Store, ep: NewEpisode) -> cortex_store::Result<()> {
    /// let seq = store
    ///     .write_txn(async |tx| tx.insert_episode(&ep).await)
    ///     .await?;
    /// # let _ = seq;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn write_txn<T, F>(&self, f: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut WriteTxn) -> Result<T>,
    {
        let mut txn = WriteTxn::begin(&self.pool, self.schema.lock_key()).await?;

        match f(&mut txn).await {
            Ok(value) => {
                txn.commit().await?;
                Ok(value)
            }
            Err(err) => {
                if let Err(rollback_err) = txn.rollback().await {
                    tracing::warn!(error = %rollback_err, "写事务回滚失败");
                }
                Err(err)
            }
        }
    }
}

/// 一个会话的概览。
///
/// **两个来源拼起来**：能从消息本身算出来的（起止时间、条数、首条用户消息）
/// 从 `episodes` 聚合；需要独立存一份的（用户自定义标题、归档、工作区）
/// 来自 `session_events` 的末态视图 `session_state`。
///
/// 后半部分曾经不存在 —— 那时会话不是一等公民，改名与删除都做不了。
/// 补上它的**不是**往 `episodes` 里塞控制记录（那等于在 L0 原始层开一条
/// 控制通道，代价是抽取、召回、同步、三端 UI 每一处都要学会跳过它），
/// 而是一张独立的事件表。见 `migrations/20260807000002_session_events.sql`。
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct SessionDigest {
    pub session_id: String,
    pub message_count: i64,
    /// 会话第一条消息的发生时间
    pub started_at: DateTime<Utc>,
    /// 最后一条消息的发生时间 —— 列表排序用它
    pub updated_at: DateTime<Utc>,
    /// 首条**用户**消息的正文。派生标题由它来，截断长度是展示决策，留给上层。
    pub first_user_text: Option<String>,
    /// 最后一条有正文的消息，供列表做预览。
    pub last_text: Option<String>,
    /// 用户设置的标题。`None` = 从未改名，上层回落到从 `first_user_text` 派生。
    pub title: Option<String>,
    /// 已归档 —— 从默认列表隐藏，但消息与派生记忆一概没动。
    pub archived: bool,
    /// 绑定的本机目录。`None` = 纯聊天会话，文件工具不进模型的工具目录。
    pub workspace: Option<String>,
}

impl Store {
    /// 会话列表：按最后活动时间倒序。
    ///
    /// # 为什么是一句 SQL 聚合，而不是拉最近 N 条 episode 在内存里归纳
    ///
    /// 后者（曾经的实现）有个不会报错的缺陷：窗口之外的会话**直接消失**。
    /// 拉 200 条最近消息，一个话痨会话就能把它占满，于是昨天的会话在列表里
    /// 不见了 —— 用户看到的是「我的历史丢了」，而日志里一切正常。
    /// 归纳交给数据库，`limit` 才真正作用在**会话**上而不是消息上。
    ///
    /// # 为什么归档过滤在 SQL 里而不是取回来再 filter
    ///
    /// 和上面是同一个坑的另一面：先取 `limit` 条再在内存里丢掉归档的，
    /// 归档会话就会**占用配额**——归档二十个会话，列表里能看见的就少二十个。
    /// 过滤必须发生在 `LIMIT` 之前。
    ///
    /// # 只列出有消息的会话
    ///
    /// 聚合的起点是 `episodes`，所以一个「只写过 session_events、还没发过
    /// 消息」的会话不会出现在这里。这是有意的：它没有时间、没有条数、没有预览，
    /// 列出来只是一行空白。用户新建会话后先选工作区再发第一句，这段时间里
    /// 那个会话由客户端本地持有 —— 第一条消息一落库它就自动出现，
    /// 而此前写下的改名 / 绑定事件会原样生效（末态与写入顺序无关）。
    pub async fn session_digests(
        &self,
        limit: i64,
        include_archived: bool,
    ) -> Result<Vec<SessionDigest>> {
        let rows = sqlx::query_as::<_, SessionDigest>(
            // 先聚合再 JOIN 视图：反过来的话 session_state 会被每一条 episode
            // 拖着算一遍，而它自己内部已经有三次 DISTINCT ON 了
            "SELECT d.session_id, d.message_count, d.started_at, d.updated_at,
                    d.first_user_text, d.last_text,
                    s.title,
                    coalesce(s.archived, false) AS archived,
                    s.workspace
               FROM (
                    SELECT e.session_id,
                           count(*)            AS message_count,
                           min(e.occurred_at)  AS started_at,
                           max(e.occurred_at)  AS updated_at,
                           (SELECT u.text FROM episodes u
                             WHERE u.session_id = e.session_id
                               AND u.role = 'user' AND u.text IS NOT NULL
                             ORDER BY u.occurred_at ASC, u.id ASC LIMIT 1)  AS first_user_text,
                           (SELECT l.text FROM episodes l
                             WHERE l.session_id = e.session_id AND l.text IS NOT NULL
                             ORDER BY l.occurred_at DESC, l.id DESC LIMIT 1) AS last_text
                      FROM episodes e
                     GROUP BY e.session_id
               ) d
               LEFT JOIN session_state s ON s.session_id = d.session_id
              WHERE $2 OR NOT coalesce(s.archived, false)
              ORDER BY d.updated_at DESC, d.session_id DESC
              LIMIT $1",
        )
        .bind(limit)
        .bind(include_archived)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 单个会话的概览。**不过滤归档** —— 调用方是按 id 明确要这一个，
    /// 归档与否是它要看的信息之一，而不是该被藏起来的理由。
    ///
    /// 返回 `None` 表示这个会话一条消息都没有（可能只写过生命周期事件）。
    pub async fn session_digest(&self, session_id: &str) -> Result<Option<SessionDigest>> {
        let row = sqlx::query_as::<_, SessionDigest>(
            "SELECT d.session_id, d.message_count, d.started_at, d.updated_at,
                    d.first_user_text, d.last_text,
                    s.title,
                    coalesce(s.archived, false) AS archived,
                    s.workspace
               FROM (
                    SELECT e.session_id,
                           count(*)            AS message_count,
                           min(e.occurred_at)  AS started_at,
                           max(e.occurred_at)  AS updated_at,
                           (SELECT u.text FROM episodes u
                             WHERE u.session_id = e.session_id
                               AND u.role = 'user' AND u.text IS NOT NULL
                             ORDER BY u.occurred_at ASC, u.id ASC LIMIT 1)  AS first_user_text,
                           (SELECT l.text FROM episodes l
                             WHERE l.session_id = e.session_id AND l.text IS NOT NULL
                             ORDER BY l.occurred_at DESC, l.id DESC LIMIT 1) AS last_text
                      FROM episodes e
                     WHERE e.session_id = $1
                     GROUP BY e.session_id
               ) d
               LEFT JOIN session_state s ON s.session_id = d.session_id",
        )
        .bind(session_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    /// 一个会话三个维度各自的末态。
    ///
    /// 返回 `None` 表示这个会话从未有过任何生命周期事件 —— 没改过名、
    /// 没归档、没绑工作区。调用方按「全默认」处理即可，不是错误。
    pub async fn session_state(&self, session_id: &str) -> Result<Option<SessionState>> {
        let row = sqlx::query_as::<_, SessionState>(
            "SELECT session_id, title, archived, workspace, decided_at
               FROM session_state WHERE session_id = $1",
        )
        .bind(session_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    /// 一个会话的全部生命周期事件，升序。界面的「它是怎么变的」。
    pub async fn session_events(&self, session_id: &str) -> Result<Vec<SessionEvent>> {
        let rows = sqlx::query_as::<_, SessionEvent>(
            "SELECT id, session_id, op, title, workspace, actor, device_id, created_at
               FROM session_events WHERE session_id = $1
              ORDER BY created_at ASC, id ASC",
        )
        .bind(session_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 批量取一批 episode 的附件，**连同 mime 与大小**。
    ///
    /// 会话详情要给每条消息挂上附件，逐条查就是 N+1 —— 一个两百条的会话
    /// 变成两百次往返，而这条路正是「打开一个带图的会话」的热路径。
    ///
    /// 元信息随行返回而不是让客户端拿着 hash 再去 `GET /blobs/{hash}` 问一遍：
    /// 那是每个附件一次往返，且在此之前客户端**没有任何东西可以显示** ——
    /// 一个 PDF 只能画成「文档 · a1b2c3d4」。
    pub async fn episode_attachments_bulk(
        &self,
        episode_ids: &[String],
    ) -> Result<Vec<EpisodeAttachment>> {
        if episode_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, EpisodeAttachment>(
            "SELECT eb.episode_id, eb.blob_hash, eb.kind, eb.filename, b.mime, b.size_bytes
               FROM episode_blobs eb
               JOIN blobs b ON b.hash = eb.blob_hash
              WHERE eb.episode_id = ANY($1)
              ORDER BY eb.episode_id ASC, eb.blob_hash ASC",
        )
        .bind(episode_ids)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }
}
