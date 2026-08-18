//! 委托凭据 —— 签给**不受信调用方**的、带语义作用域的短期钥匙。
//!
//! # 它为什么住在这个进程里
//!
//! 这份文件从记忆服务搬过来。搬之前那侧的论证是「这是**我**的入站授权策略，
//! 与谁在外面起容器无关」——那句话在「会话行住在记忆服务的库里」的时候是
//! 对的：签一把钥匙要先读会话行（它绑的是哪个会话、哪个项目、跑在云上还是
//! 本机），而只有那边读得到。
//!
//! 2026-08-16 会话搬了过来（判据见 `cortex-store` 的 crate 文档），
//! 那个前提不再成立，反过来成立了：**签发方必须是持有会话行的那一方**。
//! 于是这本簿子跟着会话走。
//!
//! 沙箱容器眼下是唯一的持有者，但下面每一条论证对任何低信任调用方都成立
//! （第三方 agent、一个 CI 任务、一段用完就扔的脚本）—— 拿沙箱举例只是因为
//! 那是真的验证过的场景。
//!
//! # 为什么不能把用户的登录 token 塞进容器
//!
//! 容器里跑的是**不可信代码**：模型的输出、用户装的依赖、被 prompt 注入的
//! 工具调用，全在里面。拿到用户的 bearer token 等于拿到那个人的全部会话、
//! 全部附件、以及经由记忆服务的全部记忆。
//!
//! OpenHands 在这一点上走的是另一条路（把 LLM api key 直接发进容器，容器
//! 自己连供应商）。我们不 —— key 留在这个进程，容器经 `/llm/stream` 代理，
//! 顺带天然有了配额与计费的位置。
//!
//! # 这个令牌的作用域**不只是一张路由白名单**
//!
//! 这是本设计里最不显然的一处。四家云 agent（Codex / Claude web / Devin /
//! Manus）都可以只做路由级隔离，因为**它们的沙箱没有可写的长期记忆**：
//! 沙箱能碰的只有一个 clone 出来的 git 工作副本，坏了丢掉重来。
//!
//! 我们不同。沙箱要调 `POST /episodes` —— 那是一条**穿透容器边界的持久
//! 通道**：写进去的东西会被转给记忆服务做抽取，而抽出来的 fact 会在**未来
//! 所有会话、所有设备**上被召回并注入 system prompt。容器早就销毁了，
//! 记忆还在。`role` 若由请求体自由决定，一个被注入的 agent 就能伪装成
//! 「用户亲口说的」写进那条管线。
//!
//! 所以「爆炸半径已被容器限住」这句话对那四家成立，对我们不成立。作用域
//! 必须下沉到**语义**：见 [`DelegatedScope`]。
//!
//! # 作用域的粒度 = **容器**的粒度，不是会话
//!
//! 一个用户可能有多个会话，而同一个用户的两段工作之间确实需要边界（一段在
//! 处理公司合同、另一段在跑一个从网上抄来的脚本）。那条边界由**项目**划：
//! 按项目分容器、分卷（见 [`DelegatedScope::key`]）。
//!
//! 令牌的作用域必须与容器**一样粗**。曾经它只认签发时那一个会话 id，
//! 比容器细一格 —— 而细出来的那一格不买任何隔离（同容器的会话本来就共用
//! 一个卷、彼此文件全可见），只逼出一个 `rebind` 补丁，而那个补丁会把上一个
//! 还在跑的那一轮打死。2026-08-18 生产实测，见
//! [`DelegatedScope::may_write_session`]。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 委托凭据的有效期。
///
/// 比票据（60 秒）长得多：这把是长连接用的，容器一活可能几小时。
/// 但仍然有限 —— 容器被回收后令牌应当自然失效，而不是永远留在表里。
/// 每次成功的 [`DelegatedTokens::resolve`] 与 [`DelegatedTokens::find_by_key`]
/// 都会盖一个新时间戳，所以活跃的沙箱不会被误杀。
const TTL: Duration = Duration::from_secs(6 * 3600);

/// 沙箱能做什么。**按语义授权，不是按路由**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegatedScope {
    /// 令牌属于谁。租户隔离仍然由它决定 —— 这一层是既有的。
    pub owner: String,
    /// 签发这把令牌时的那个会话。
    ///
    /// # ⚠️ 它**不再**是「只能写这一个会话」的判据
    ///
    /// 一个容器服务的是**同一个 key 下的全部会话**（见 [`Self::key`]）——
    /// 同项目的、以及全部未分组的。而这里从前只记一个会话 id，写 episode 时
    /// 逐字比对，于是同一个容器换一个会话来用就必须 `rebind`。
    ///
    /// 那条 rebind 会把**上一个会话还在跑的那一轮**打死：它写 assistant
    /// episode 时拿到 400「这把凭据只能写它绑定的那个会话」。生产日志里那两行
    /// 配对就是它（`bound=live-sbx-… attempted=live-chat-…`）。触发条件很日常：
    /// 在 Web 上同时开着两个会话。
    ///
    /// 现在判据是 [`Self::may_write_session`] —— 「这个会话属不属于这个作用域」。
    /// 这个字段留着只为**排查**（这把钥匙当初是为谁签的），不参与授权。
    pub session_id: String,
    /// 这个会话属于哪个项目。`None` = 未分组。
    ///
    /// **它决定容器与卷**（见 [`Self::key`]），不参与 [`Self::allows`] ——
    /// 令牌能干什么仍然只看会话。
    pub project: Option<String>,
}

impl DelegatedScope {
    /// 容器与卷按这个键分。
    ///
    /// # 为什么按项目而不是按用户
    ///
    /// 上一版一个用户只有一个 `/workspace`。于是「客户合同」与「从网上抄来的
    /// 脚本」两段工作的文件混在同一个目录里 —— 装依赖互相踩、`ls` 一屏全是
    /// 别的项目的东西，而这两件事用户自己是分开的（他建了两个项目）。
    ///
    /// 按项目分之后，切项目会重建容器（冷启动 913 ms，用户感知不到），
    /// 同一个项目的多个会话共用一个容器。
    ///
    /// # 为什么未分组仍然用裸 owner
    ///
    /// 恒等映射 = **生产上不用做数据迁移**。现有那些卷叫 `cortex-ws-<owner>`，
    /// 它们的会话都还没有项目，所以照旧命中；一旦被移进项目才会拿到新卷，
    /// 而那时用户是明确在「新建一个项目」，空工作区是他预期的。
    ///
    /// # 项目那一段为什么是哈希而不是 id 本身
    ///
    /// **容器名会当 DNS 名用**（agentd 与沙箱同网段时直连 `http://<容器名>`），
    /// 而 DNS 标签的硬上限是 **63 字节**。两个 ULID 直接拼起来：
    ///
    /// ```text
    /// cortex-sbx- + 26 + --p- + 26 = 67  ✗
    /// ```
    ///
    /// 真机上撞到过，症状很误导：容器 `Up (healthy)`、里面的 agent 日志写着
    /// 「已就绪」，而编排侧报「30 秒没应答」—— 因为它连名字都解析不出来。
    ///
    /// 12 位十六进制（48 bit）之后是 11+26+4+12 = **53**，留足余量。
    /// 碰撞概率在「一个用户的项目数」这个量级上可以忽略。
    ///
    /// 代价是容器名读不出项目 id。反查的路子是每轮那条日志
    /// （`本轮走云沙箱 owner=… scope=… sandbox=…`）—— 它把三者印在一行。
    /// **不给 runner 加一个 project 入参**：那条 trait 只收作用域与 spec hash
    /// 是刻意的（有测试守着「调用方塞不进第二个挂载」），为了一行日志
    /// 松掉它不划算。
    ///
    /// # 分隔符为什么不用管注入
    ///
    /// owner 由服务端从凭据解析（`current_user`），永远在最前；
    /// project 由客户端给，但它只能改变**自己名下**的后半段 ——
    /// 拼出的键无论如何都以自己的 owner 开头，够不到别人的键。
    /// 哈希顺带把「客户端能塞多长的 id 进来」这件事也钉死了。
    #[must_use]
    pub fn key(&self) -> String {
        match &self.project {
            None => self.owner.clone(),
            Some(p) => {
                use sha2::{Digest as _, Sha256};
                let digest = Sha256::digest(p.as_bytes());
                format!("{}--p-{}", self.owner, hex::encode(&digest[..6]))
            }
        }
    }

    /// 这把令牌能不能写那个会话的对话记录。
    ///
    /// # 判据是「同一个容器」，不是「同一个会话」
    ///
    /// 授权的粒度必须与**容器**的粒度一致：一个容器服务同一个 key 下的全部
    /// 会话，读写的是同一个卷。让令牌只认一个会话，换来的不是更强的隔离
    /// （那些会话的文件本来就在同一个卷里、彼此完全可见），而是一个必须靠
    /// `rebind` 打补丁的错配 —— 而 rebind 会打死上一个还在跑的那一轮。
    ///
    /// 放宽到「同 key」之后，一把被偷走的沙箱令牌能多做的事是：写同项目下
    /// 别的会话的对话记录。而它**本来就能**读写那些会话的全部文件。
    /// 也就是说这不是放宽了一格权限，是把权限调回与它实际能碰到的东西一致。
    ///
    /// `other` 是那个会话所属的项目（`None` = 未分组）。调用方从库里查，
    /// 查不到就按未分组 —— 与 `delegation_scope` 的降级方向一致。
    #[must_use]
    pub fn may_write_session(&self, other_project: Option<&str>) -> bool {
        self.project.as_deref() == other_project
    }

    /// 这条路径 + 方法允许吗。
    ///
    /// # 为什么是白名单而不是黑名单
    ///
    /// 与 `WORKSPACE_FREE_TOOLS` 同一个论证：黑名单漏掉一条新路由 = 沙箱
    /// 悄悄多了一样能力（静默、危险）；白名单漏掉一条 = 沙箱少一样能力
    /// （响亮、当场可见）。**失败方向不同，选会往安全那边倒的那个。**
    ///
    /// 清单是代码审计出来的**实际调用**（`cortex-local` 的 `remote.rs` 逐条
    /// 核过），不是猜的。少一条的后果已经量化过：漏掉 `GET /sessions/{id}`
    /// 的话，`session_history` 会静默降级为空历史，云端会话**逐轮失忆且没有
    /// 任何报错**。
    ///
    /// # 这张单子只管**这个进程**服务的路由
    ///
    /// 它是 agentd 的入站授权，不是「沙箱在世界上能干什么」的总清单。
    /// 一条这个进程根本不提供的路由写在这里，读的人会以为它由我们把关，
    /// 而实际上把关的是别人 —— 见下面 `/memory/search` 那条。
    #[must_use]
    pub fn allows(&self, method: &axum::http::Method, path: &str) -> bool {
        use axum::http::Method;
        match (method, path) {
            // 每轮一次到多次。LLM key 不进容器的代价与收益都在这条上
            (&Method::POST, "/llm/stream") => true,
            // 写 user / assistant episode，以及 outbox 重放
            (&Method::POST, "/episodes") => true,
            // 每轮拉会话历史。**漏掉这条 = 逐轮失忆且静默**
            (&Method::GET, p) if is_session_path(p) => true,
            // 启动时问「这把凭据属于谁」。漏掉的话状态目录永远落在 _pending
            (&Method::GET, "/auth/me") => true,
            // 本轮附件的字节。沙箱够不到对象存储（egress 对私有段一律拒，
            // 有意的设计），字节由 agentd 从对象存储中转 —— 漏掉这条的话
            // 云端会话的附件会**静默全部不可用**（cortex-local 把取不到
            // 落成「附件不可用」说明，不报错）。只放单段路径：`/blobs`
            // 本身是 POST 上传，`/blobs/{hash}/url` 的预签名 URL 在沙箱里
            // 也用不了（对象存储不可达），都不放。
            (&Method::GET, p) if is_blob_path(p) => true,
            _ => false,
        }
    }
}

/// `/blobs/{hash}` 形状（不含 `/blobs` 本身与 `/blobs/{hash}/url`）。
fn is_blob_path(path: &str) -> bool {
    match path.strip_prefix("/blobs/") {
        // 只认一层，与 is_session_path 同一条纪律：多一层是另一个端点
        Some(rest) => !rest.is_empty() && !rest.contains('/'),
        None => false,
    }
}

/// `/sessions/{id}` 形状（不含 `/sessions` 本身 —— 那是列全部会话）。
fn is_session_path(path: &str) -> bool {
    match path.strip_prefix("/sessions/") {
        // 只认一层：`/sessions/abc` 是，`/sessions/abc/xxx` 不是。
        // 多一层就是另一个端点，而这里的清单是逐条审计出来的
        Some(rest) => !rest.is_empty() && !rest.contains('/'),
        None => false,
    }
}

/// 已签发的委托令牌。
///
/// 与 `TicketBook` 同形（内存、随进程走）。agentd 重启后所有令牌失效，
/// 而那是**对的**：那些容器也需要被重新接管，让它们带着一把还能用的凭据
/// 继续跑才是问题。`runner::ensure` 对这件事有配套处理 —— 容器在跑但令牌
/// 对不上时它会重建，而不是让接下来每一条请求都 401。
#[derive(Default)]
pub struct DelegatedTokens {
    inner: Mutex<HashMap<String, (DelegatedScope, Instant)>>,
}

impl DelegatedTokens {
    /// 签一把。调用方必须**已经**确认了 owner 的身份。
    pub fn issue(&self, scope: DelegatedScope) -> String {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).expect("内核熵源不可用，拒绝签发可预测的令牌");
        let token = hex::encode(buf);
        let mut guard = self.inner.lock().expect("令牌表的锁不该中毒");
        // 顺手清过期的。同 TicketBook：签发是唯一让这个表长大的动作，
        // 在这里清就不可能「只涨不清」
        let now = Instant::now();
        guard.retain(|_, (_, at)| now.duration_since(*at) < TTL);
        guard.insert(token.clone(), (scope, now));
        token
    }

    /// 认这把令牌吗，认的话它的作用域是什么。顺带续期。
    #[must_use]
    pub fn resolve(&self, token: &str) -> Option<DelegatedScope> {
        let mut guard = self.inner.lock().ok()?;
        let now = Instant::now();
        let (scope, at) = guard.get_mut(token)?;
        if now.duration_since(*at) >= TTL {
            guard.remove(token);
            return None;
        }
        *at = now;
        Some(scope.clone())
    }

    /// 这个作用域（用户 + 项目）手上那把还有效的令牌（如果有）。
    ///
    /// # 为什么需要「复用」而不是每轮签一把新的
    ///
    /// 容器的**入站**认证用的是它启动时 env 里那把（`cortex-local` 的入站与
    /// 出站共用一个 token，刻意不引入第二个秘密）。每轮换新的话，第二轮反代
    /// 过去就是 401 —— 而那条错误读起来像「沙箱坏了」。
    ///
    /// 真机第一轮通、第二轮 401，就是这个。**令牌的生命周期跟着容器走**，
    /// 不跟着轮次走。
    ///
    /// 按 [`DelegatedScope::key`] 而不是按 owner 找：一个用户可以同时有几个
    /// 项目的沙箱活着，拿错那把的后果是**把 A 项目的令牌塞给 B 项目的容器**
    /// —— 而容器起来之后照常应答，只是它认的是另一把，第二轮才 401。
    /// # 命中就顺手续期
    ///
    /// 那次续期从前由 `rebind` 顺带做（每轮都会调它）。`rebind` 删掉之后
    /// 必须挪到这里 —— 不挪的话，一个连着聊了 6 小时的容器会在 TTL 上突然
    /// 失效，而它明明一直在被用。
    #[must_use]
    pub fn find_by_key(&self, key: &str) -> Option<String> {
        let mut guard = self.inner.lock().ok()?;
        let now = Instant::now();
        let hit = guard.iter().find_map(|(tok, (scope, at))| {
            (scope.key() == key && now.duration_since(*at) < TTL).then(|| tok.clone())
        })?;
        if let Some((_, at)) = guard.get_mut(&hit) {
            *at = now;
        }
        Some(hit)
    }

    /// 作废。调用方是 `DELETE /delegated-tokens` —— 一个自己编排沙箱的
    /// 第三方把容器停掉之后收回那把钥匙。
    ///
    /// **本进程的空闲回收器不调它**，理由见 `reaper::sweep_once` 里那段：
    /// 停掉容器之后作废，只会让下一次重试签一把新钥匙，于是
    /// `runner::ensure` 的 `token_matches` 判否、把一个可能好好的容器
    /// 强制重建掉。
    pub fn revoke(&self, token: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.remove(token);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    fn scope() -> DelegatedScope {
        DelegatedScope {
            owner: "u1".into(),
            session_id: "s1".into(),
            project: None,
        }
    }

    fn in_project(owner: &str, project: &str) -> DelegatedScope {
        DelegatedScope {
            owner: owner.into(),
            session_id: "s1".into(),
            project: Some(project.into()),
        }
    }

    /// 白名单**正好**是审计出来的那四条，一条不多。
    ///
    /// 每一条都有代价：多放一条就是沙箱多一样穿透容器边界的能力。
    ///
    /// 两条否定用例值得单独读一遍：
    ///
    /// - **`/memory/search`** 在记忆服务那侧的原版里是允许的，搬过来时**去掉
    ///   了**。这个进程根本不提供那条路由（`only_docker_shaped_paths_are_
    ///   served_here` 正钉着它 404），转给 Cormex 的是边缘，而那一侧另有一本
    ///   自己的令牌簿。留着它等于在这里替别人家的路由作保 —— 下一个读这张
    ///   单子的人会以为沙箱的记忆检索由我们把关。哪天真要在这个进程上代理
    ///   那条路，它会以 403 当场变红，那正是白名单该有的失败方向。
    /// - **`/confirmations`** 从来没进过这张单子，而它 2026-08-16 起在这个
    ///   进程上**真的存在了**（反代进容器的那条）。于是放行它的后果比以前更
    ///   直接：容器问 agentd，agentd 原样转回同一个容器 —— 一个无限乒乓，
    ///   正是 `sandbox_proxy` 模块头第三条纪律警告的那种。
    #[test]
    fn the_allowlist_is_exactly_what_the_agent_actually_calls() {
        let s = scope();
        for (m, p) in [
            (Method::POST, "/llm/stream"),
            (Method::POST, "/episodes"),
            (Method::GET, "/sessions/01ABC"),
            (Method::GET, "/auth/me"),
        ] {
            assert!(
                s.allows(&m, p),
                "{m} {p} 是每轮都要打的，漏了整个沙箱不可用"
            );
        }

        for (m, p) in [
            // 记忆检索：这个进程不提供它，见上面那段
            (Method::GET, "/memory/search"),
            // 确认回路：加进来会让 agentd 与容器之间的转发变成一个无限回环
            (Method::POST, "/confirmations"),
            (Method::GET, "/confirmations"),
            // 签钥匙那条本身。放行它 = 一把委托凭据能给自己续签、
            // 甚至给别的会话签一把，作用域绑定当场作废
            (Method::POST, "/delegated-tokens"),
            // 列全部会话 —— 沙箱只该看得见自己那一个
            (Method::GET, "/sessions"),
            // 改会话（含换工作区）不该由沙箱自己决定
            (Method::PATCH, "/sessions/01ABC"),
            // 项目、附件、同步：都不是 agent 循环需要的
            (Method::GET, "/projects"),
            (Method::POST, "/blobs"),
            (Method::GET, "/sync"),
            // 在线名册（roadmap E 的阶段 3）。**沙箱不该出现在里面**：
            // 那份名册回答的是「我那个绑在别处的会话该去哪台机器上打开」，
            // 而容器不是一台用户能过去的机器 —— 报上去只会多一行没人能用的。
            //
            // 靠白名单默认拒绝而不是靠容器里那个 agent 自觉：它那侧确实也
            // 判了 exec_env，但那只是为了少一条每 30 秒的失败日志。
            (Method::POST, "/agents/heartbeat"),
            (Method::GET, "/agents"),
            (Method::POST, "/auth/ticket"),
            // 写 episode 是 POST；GET 那条是回放别人的对话
            (Method::GET, "/episodes/01ABC"),
            // 多一层路径就是另一个端点
            (Method::GET, "/sessions/01ABC/messages"),
        ] {
            assert!(
                !s.allows(&m, p),
                "{m} {p} 不在审计出来的调用清单里，放行等于给沙箱一样它不需要的能力"
            );
        }
    }

    /// **被攻陷的容器碰不到自己的备份。**
    ///
    /// 这条单独立出来，是因为它守的不是「少给一点权限」这种程度问题，
    /// 而是数据兜底第一层**整层的前提**：快照由宿主驱动、写进容器够不着的
    /// 地方，所以一个能在容器里执行任意命令的攻击者删不掉它。
    ///
    /// 那条性质完全来自这里的**拓扑**（令牌白名单里没有这几条路由），
    /// 不来自任何运行期判断。谁哪天顺手把 `/sandbox/snapshots` 加进
    /// `allows`，整层兜底就一起没了 —— 而那**不会有任何症状**，
    /// 直到有人真的需要恢复的那一天。
    ///
    /// 搬进这个进程之后这条更要紧了：快照的那几条路由现在**就在同一个
    /// 二进制里**，`allows` 与它们之间只隔着这一个 match。
    #[test]
    fn the_sandbox_cannot_reach_its_own_backups() {
        let s = scope();
        for (m, p) in [
            (Method::GET, "/sandbox/snapshots"),
            (Method::POST, "/sandbox/snapshots"),
            (Method::POST, "/sandbox/snapshots/01ABC/restore"),
            // 账本那一半（碰库的那条），与动作那一半是两条路由
            (Method::GET, "/sandbox-snapshots"),
            (Method::POST, "/sandbox-snapshots"),
        ] {
            assert!(
                !s.allows(&m, p),
                "{m} {p} 被放行了 —— 数据兜底的第一层就此失效：\
                 一个被攻陷的容器可以列出、覆盖、并（将来 purge 落地后）\
                 删掉自己的全部备份，而这件事没有任何症状"
            );
        }
    }

    #[test]
    fn a_token_resolves_to_its_scope_and_an_unknown_one_does_not() {
        let book = DelegatedTokens::default();
        let t = book.issue(scope());
        assert_eq!(
            book.resolve(&t).as_ref(),
            Some(&scope()),
            "刚签发的令牌必须解析回它自己的作用域，否则 auth::require 里那一支永远走不通"
        );
        assert!(
            book.resolve("deadbeef").is_none(),
            "没签发过的令牌必须认不出来"
        );
    }

    #[test]
    fn revoking_takes_effect_immediately() {
        let book = DelegatedTokens::default();
        let t = book.issue(scope());
        book.revoke(&t);
        assert!(
            book.resolve(&t).is_none(),
            "作废之后必须当场失效 —— 容器可能还活着（stop 之前的最后一瞬），\
             而它此刻已经不该再写 episode 了"
        );
    }

    /// 未分组的键**恒等于 owner** —— 生产上那些卷靠这条不用迁移。
    #[test]
    fn an_ungrouped_scope_key_is_just_the_bare_owner() {
        assert_eq!(
            scope().key(),
            "u1",
            "改掉这条等于让生产上每个用户的 cortex-ws-<owner> 卷当场失联：\
             容器会挂一个空卷起来，而用户看到的是「我的文件全没了」"
        );
        assert_ne!(
            in_project("u1", "p1").key(),
            scope().key(),
            "项目里的沙箱必须是另一个卷，否则按项目分工作区这件事等于没做"
        );
        assert_ne!(
            in_project("u1", "p1").key(),
            in_project("u1", "p2").key(),
            "两个项目之间也要分开 —— 这才是用户真正抱怨的那件事"
        );
    }

    /// 容器名会当 **DNS 名**用，所以它必须 ≤ 63 字节。
    ///
    /// 这条是补写的：第一版把两个 ULID 直接拼起来，得到 67 字节的容器名。
    /// 真机上的症状极具误导性 —— 容器 `Up (healthy)`、里面的 agent 日志写着
    /// 「本地 agent 已就绪」，而编排侧报「30 秒没应答，可能是崩了」。
    /// 实际是 `getent hosts <容器名>` 直接解析失败。
    ///
    /// 前缀取的是 [`crate::runner::NAME_PREFIX`] 本身，不是手抄一份字面量：
    /// 两处都在这个 crate 里，抄一份的话「有人把前缀加长了」这条就测不到，
    /// 而那正是这条测试唯一要抓的东西。
    ///
    /// 上限写死 63 而不是引用某个常量：这是 DNS 标签的**协议**上限
    /// （RFC 1035），不是我们选的。
    #[test]
    fn a_container_name_built_from_a_scope_key_stays_within_the_dns_label_limit() {
        // 最长的现实输入：两个 26 字符的 ULID
        let worst = DelegatedScope {
            owner: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            session_id: "s".into(),
            project: Some("01BX5ZZKBKACTAV9WEVGEMMVRZ".into()),
        };
        let name_len = crate::runner::NAME_PREFIX.len() + worst.key().len();
        assert!(
            name_len <= 63,
            "容器名 {name_len} 字节，超过 DNS 标签上限 63。\
             症状不是「建容器失败」，而是容器起来了、健康检查也过了，\
             agentd 却连它的名字都解析不出来 —— 报出来的是「30 秒没应答」"
        );
    }

    /// 同一个项目 id 每次都要算出**同一个**键。
    #[test]
    fn the_project_hash_is_deterministic() {
        assert_eq!(
            in_project("u1", "p1").key(),
            in_project("u1", "p1").key(),
            "键不稳定的话，每一轮对话都会去找一个不存在的容器 —— \
             于是每轮建一个新的，旧的那些带着文件留在原地"
        );
    }

    /// 项目 id 由客户端给，但它**改不动别人的键**。
    #[test]
    fn a_client_supplied_project_id_cannot_reach_another_users_sandbox() {
        // 一个想撞到 u2 名下去的项目 id
        let evil = in_project("u1", "--p-x/../u2");
        assert!(
            evil.key().starts_with("u1"),
            "键必然以服务端解析出来的 owner 开头（current_user，不来自请求体），\
             所以无论项目 id 长成什么样都只能改动自己名下那一段"
        );
    }

    /// 附件字节那条：放行 `GET /blobs/{hash}`，且只放这一个形状。
    ///
    /// 红了的症状分两个方向：单段放行没了 → 云端会话的附件**静默全部
    /// 不可用**（cortex-local 把取不到落成说明，不报错）；多段/别的
    /// 方法被放进来 → 沙箱多了一条没审计过的路。
    #[test]
    fn blob_bytes_are_reachable_but_nothing_else_under_blobs() {
        use axum::http::Method;
        let s = scope();
        assert!(
            s.allows(&Method::GET, "/blobs/abc123"),
            "沙箱取不到附件字节 —— 云端会话的附件会静默全部不可用"
        );
        for (m, p) in [
            (Method::POST, "/blobs"),
            (Method::POST, "/blobs/presign"),
            (Method::POST, "/blobs/commit"),
            (Method::GET, "/blobs/abc/url"),
            (Method::GET, "/blobs/"),
            (Method::DELETE, "/blobs/abc"),
        ] {
            assert!(
                !s.allows(&m, p),
                "{m} {p} 不在审计清单上，不该被 blob 那条捎带放行"
            );
        }
    }

    #[test]
    fn expired_tokens_are_swept_when_a_new_one_is_issued() {
        let book = DelegatedTokens::default();
        {
            let mut g = book.inner.lock().unwrap();
            g.insert(
                "old".into(),
                (scope(), Instant::now() - TTL - Duration::from_secs(1)),
            );
        }
        assert_eq!(book.len(), 1);
        let fresh = book.issue(scope());
        assert_eq!(
            book.len(),
            1,
            "过期令牌应当在签新令牌时被清掉 —— 这是这张表唯一的清理时机，\
             漏了它就只涨不清"
        );
        assert!(
            book.resolve(&fresh).is_some(),
            "清理不能把刚签的这把一起扫掉"
        );
    }

    /// 同一个作用域**重复要钥匙拿到的是同一把**，且会跟着换会话。
    ///
    /// **同一个容器下的两个会话不许互相打死。**
    ///
    /// 生产实测（2026-08-18）：一个容器服务同 owner/项目下的全部会话，而令牌
    /// 从前只认签发时那一个会话 id，于是每来一个新会话就 `rebind`。上一个
    /// 会话**还在跑的那一轮**随后写 assistant episode 时拿到 400 ——
    /// 文件已经改完了，回答却进不了历史。日志里那两行配对是它的指纹：
    /// `bound=live-sbx-… attempted=live-chat-…`。
    ///
    /// 触发条件很日常：在 Web 上同时开着两个会话。
    #[test]
    fn two_sessions_in_one_container_do_not_evict_each_other() {
        let book = DelegatedTokens::default();
        // 未分组作用域 —— 一个用户所有「没归到项目里」的会话共用这一个容器
        let token = book.issue(DelegatedScope {
            owner: "u1".into(),
            session_id: "第一个会话".into(),
            project: None,
        });

        // 第二个会话来了。同一个 key，于是复用同一把令牌 —— 而**不改绑**
        let same = book
            .find_by_key(
                &DelegatedScope {
                    owner: "u1".into(),
                    session_id: "第二个会话".into(),
                    project: None,
                }
                .key(),
            )
            .expect("同一个 key 该找回同一把");
        assert_eq!(same, token);

        let scope = book.resolve(&token).expect("令牌还有效");
        assert!(
            scope.may_write_session(None),
            "第一个会话还在跑的那一轮必须写得进去 —— 这正是从前被 400 打死的那一步"
        );
    }

    /// 这条钉的是 `routes::delegate_token` 依赖的那个性质。它红了，症状是
    /// 真机上「第一轮好使、第二轮 401」：容器的入站认证认的是启动时 env 里
    /// 那把，而我们每轮换了新的。
    #[test]
    fn the_same_scope_gets_the_same_token_back() {
        let book = DelegatedTokens::default();
        let first = book.issue(scope());
        let found = book
            .find_by_key(&scope().key())
            .expect("同一个作用域必须找得回它那把令牌");
        assert_eq!(
            found, first,
            "找回来的不是同一把 —— 下一轮反代进容器就是 401，而那条错误读起来像「沙箱坏了」"
        );

        // 同一个容器服务下一个会话时**什么都不用改**。
        //
        // 这里从前是 `rebind(&found, "s2")` —— 把令牌改绑到新会话。那一步会把
        // **上一个还在跑的那一轮**打死：它写 assistant episode 时拿到 400。
        // 现在授权看作用域、不看这个 id，所以改绑连同它的坑一起删了。
        let after = book.resolve(&found).expect("找回来的令牌该是有效的");
        assert_eq!(
            after.owner, "u1",
            "作用域里的 owner 与项目才是判据，它们不该被任何一轮动过"
        );
        assert!(
            after.may_write_session(None),
            "未分组作用域的令牌，写未分组会话的记录 —— 同一个容器、同一个卷"
        );
        assert!(
            !after.may_write_session(Some("别人的项目")),
            "跨项目**必须**拒：那是另一个容器、另一个卷"
        );

        // 别人的作用域找不到它
        assert!(
            book.find_by_key("u2").is_none(),
            "按 key 找必须是精确匹配。撞上别人那把的后果是把 A 的令牌塞进 B 的容器"
        );
    }
}
