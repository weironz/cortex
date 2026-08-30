//! 打到远端 cortexd 的 HTTP 客户端。
//!
//! 只覆盖本地 agent 自己要用的三条：写 episode、检索、LLM 代理。
//! 客户端要的其余端点（会话列表、回放、blob、/sync、/ws）走
//! [`crate::proxy`] 原样转发 —— 本地 agent 不该为它们各写一个方法，
//! 那等于把整套契约在这里再实现一遍。

use std::sync::{Arc, RwLock};
use std::time::Duration;

use cortex_core::{CortexError, Result};
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};
use cortex_proto::llm::{GeneratedImages, LlmStreamRequest};

/// 写入与检索的超时。
///
/// 比 `/llm/stream` 短得多：这两条是「查库 + 算一次向量」，秒级就该回。
/// 拖长了不会更容易成功，只会让一轮对话卡在那儿 —— 而离线队列本来就是
/// 为「连不上」准备的，早点失败早点排队。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// [`Remote::blob_response`] 等**响应头**的上限（只包 send，不包身子）。
/// 「连不上 / 服务端不吭声」要尽快失败 —— 附件取不到只是降级，
/// 不该让用户对着转圈等两分钟。
const BLOB_HEADER_TIMEOUT: Duration = Duration::from_secs(15);

/// 启动路径上那两次探测的超时。
///
/// `protocol_check` 与 `whoami` 都**只在启动时各调一次**，而且失败路径本来
/// 就是优雅降级（跳过协议检查照常启动 / 用上一次那个账号的目录）。
/// 让它们跟着 `REQUEST_TIMEOUT` 走的实测后果：远端是个**黑洞地址**
/// （VPN 断了、防火墙 DROP 而不是 REJECT）时，两次各跑满 20 秒 ——
/// **启动要 40 秒**，而这个 agent 其实第一秒就能干活。
///
/// 症状很难归因：桌面端与 CLI 都会以为「agent 起不来」然后回落，
/// 而回落意味着工具跑到别人的机器上去。
///
/// 2 秒的取舍：一个 2 秒内握不上手的远端，对「启动时要不要检查协议」
/// 这个问题而言就是不可达。真正的写入与检索另有 `REQUEST_TIMEOUT`，
/// 不受这里影响；而协议不兼容那种情况远端是活着的，2 秒绰绰有余。
const STARTUP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// 远端 cortexd 的句柄。克隆代价低（内部 `Arc`）。
#[derive(Clone)]
pub struct Remote {
    http: reqwest::Client,
    base: String,
    /// 出站凭据，**可热替换**。见 [`Remote::set_token`]。
    ///
    /// 共享一格而不是各克隆一份：`Remote` 被克隆进 `LocalState`、进每一个
    /// 轮次的 host —— 各存各的话，换了凭据只有换的那一份知道。
    token: Arc<RwLock<Option<String>>>,
    /// 凭据换过几次。**只用来叫醒等着的人**，值本身没有意义。
    ///
    /// 存在的理由见 [`Self::token_generation`]：有一条路在 401 之后要睡
    /// 十分钟，而新凭据往往几秒后就到了。
    token_generation: Arc<tokio::sync::watch::Sender<u64>>,
}

impl std::fmt::Debug for Remote {
    /// 手写而不是 derive：`token` 是长期凭据，derive 会把它打进任何一行
    /// `{:?}` 日志里，而那些日志经常是出问题时第一个被贴到聊天窗口的东西。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("base", &self.base)
            .field("token", &self.read_token().map(|_| "<已设置>"))
            .finish()
    }
}

/// `/sessions/{id}` 那一次请求带回来的、这一轮要用的东西。
///
/// 只有两样，但**都要**：历史给模型看，工作区名决定文件工具的根。
/// 分两次请求去拿是每轮多一次往返，而这两样本来就在同一个响应里。
#[derive(Debug, Default)]
pub struct RemoteSession {
    /// `(是不是用户说的, 正文)`，按时间正序。
    pub turns: Vec<(bool, String)>,
    /// 容器工作区卷里的子目录名。`None` = 卷根（默认）。
    pub container_workspace: Option<String>,
}

impl Remote {
    /// # Errors
    /// HTTP 客户端构造失败（TLS 后端不可用之类）。
    pub fn new(base: impl Into<String>, token: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            // 这里**不设**全局超时：/llm/stream 是长流，模型「想」几分钟是常态。
            // 逐个请求设各自的超时，见 REQUEST_TIMEOUT 与 llm_stream
            //
            // ⚠️ 但**连接**这一段要有界，而且它与上面那句不矛盾：
            // `connect_timeout` 只管 TCP（与 TLS）握手，握完之后一个字节
            // 都不碰 —— 模型想多久都不受影响。
            //
            // 没有它的时候：一个把 SYN 丢进黑洞的地址（写错的自建部署、
            // 掉线的 VPN、被防火墙静默丢弃）会让 `.send()` **无限期**挂着。
            // 而 `/llm/stream` 那条路刻意没有逐请求超时，于是那一轮就一直
            // 挂在那里，用户只能去按停止键。
            //
            // 30 秒：TCP 握手要 30 秒的网络已经没法用了，而它长到不会
            // 误伤慢速移动网络下的 TLS 协商。**这只是那件事的一半** ——
            // 「连上了但一个字节不回」还是没有界，见 roadmap 里那一节。
            //
            // ⚠️ **这一行没有测试守着，写清楚免得被当成有覆盖。**
            // 要测就得有一个真的把 SYN 丢进黑洞的地址，而「哪个地址会
            // 黑洞」取决于跑测试那台机器的网络：试过 `10.255.255.1`，
            // 在这台机器上 5 秒就返回错误 —— 于是删掉这一行测试照样绿，
            // 一条**为了错误的原因通过**的测试。宁可没有。
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| CortexError::Config(format!("HTTP 客户端构造失败：{e}")))?;
        Ok(Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            token: Arc::new(RwLock::new(token)),
            token_generation: Arc::new(tokio::sync::watch::channel(0).0),
        })
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// 当前出站凭据的副本。
    ///
    /// 返回 `String` 而不是 `&str`：它住在锁后面，借出去等于把守卫的生命周期
    /// 泄露给调用方，而这个进程里的调用方全都会跨 `.await`。
    #[must_use]
    pub fn token(&self) -> Option<String> {
        self.read_token()
    }

    /// 换一把出站凭据。**桌面端的 access token 每 15 分钟轮换一次。**
    ///
    /// # 为什么是热替换，而不是重启这个进程
    ///
    /// 凭据烧在进程启动参数里的时候，桌面端只能靠**重启整个 agent** 来换 ——
    /// 于是每 15 分钟：跑着的轮次被拦腰砍断、监听端口换一个、旧进程咽气前
    /// 用一把已经退位的凭据答 401，而客户端把那个 401 读成「你的登录失效了」
    /// 并把用户踢回登录页。一次凭据轮换本该是无事发生的。
    pub fn set_token(&self, token: Option<String>) {
        let mut g = self
            .token
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *g = token.filter(|t| !t.trim().is_empty());
        // 先放锁再叫人：唤醒的那一方第一件事就是读凭据，
        // 在锁里叫等于让它立刻撞上自己这把写锁
        drop(g);
        self.token_generation
            .send_modify(|n| *n = n.wrapping_add(1));
    }

    /// 订阅「凭据换了」。见 `crate::tunnel` 里那处 `select!`。
    ///
    /// # 为什么是 `watch` 而不是 `Notify`
    ///
    /// `Notify::notify_waiters` 只叫醒**此刻正在等**的人，而这里的竞态恰好
    /// 落在那个缝里：隧道拨号被 401 拒绝 → （桌面端推来新凭据）→ 隧道才
    /// 开始睡。用 `Notify` 的话这一下丢了，机器要多离线十分钟；`watch`
    /// 记的是代号，等的人一 `changed()` 就发现自己落后了，不管先后。
    #[must_use]
    pub fn token_generation(&self) -> tokio::sync::watch::Receiver<u64> {
        self.token_generation.subscribe()
    }

    fn read_token(&self) -> Option<String> {
        // 中毒了也要把值读出来：写路径只是替换一个 String，不存在
        // 「被看到的中间状态」；这里因为中毒而返回 None，等于让 agent
        // 在一次无关的 panic 之后**静默失去凭据**，那才是真的坏
        self.token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.read_token() {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// 把一轮对话写回记忆库。见 `cortex_proto::episodes`。
    pub async fn write_episode(&self, req: &NewEpisodeRequest) -> Result<EpisodeAck> {
        let resp = self
            .auth(self.http.post(self.url("/episodes")))
            .timeout(REQUEST_TIMEOUT)
            .json(req)
            .send()
            .await
            .map_err(map_transport)?;
        let resp = checked(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析 /episodes 响应失败：{e}")))
    }

    /// 生一张图。图由服务端抓下来入库，这里只拿回哈希。
    ///
    /// # 为什么超时给得比别的路长得多
    ///
    /// 生图是**同步等结果**的：旗舰型号几十秒，加上服务端还要把图从供应商
    /// 那儿下载下来再入库。用 `REQUEST_TIMEOUT`（给普通请求的那个）会在
    /// 图生成到一半时把连接掐了 —— 而钱已经花掉了。
    ///
    /// # Errors
    /// 服务端没有能生图的来源、供应商拒绝、或者网络断了。
    pub async fn generate_image(
        &self,
        prompt: &str,
        size: Option<&str>,
        n: u8,
        session_id: &str,
    ) -> Result<GeneratedImages> {
        let resp = self
            .auth(self.http.post(self.url("/llm/image")))
            // ⚠️ **超时要跟着张数走。** 服务端在不支持 n 的协议上是**连发**
            // n 次凑够的（见 `cortex_llm::image::generate`），四张就是四倍的
            // 时间。写死 240s 的话，画三张以上必定在最后一张上被掐断 ——
            // 而那时前面几张的钱已经花掉了
            .timeout(std::time::Duration::from_secs(
                180 + 120 * u64::from(n.max(1)),
            ))
            .json(&serde_json::json!({
                "prompt": prompt,
                "size": size,
                "n": n,
                // 只进画廊，不影响生成 —— 有了它，画廊里那张图才回答得了
                // 「这是我在哪条会话里画的」
                "session_id": session_id,
            }))
            .send()
            .await
            .map_err(map_transport)?;
        let resp = checked(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析 /llm/image 响应失败：{e}")))
    }

    /// 取一份技能的正文（`load_skill` 走的那条）。
    ///
    /// # 为什么按名字取一份，而不是把整张表拉回来自己找
    ///
    /// 拉整张表意味着一次工具调用把**所有**技能的正文搬过网络，只为了用其中
    /// 一份 —— 那正好把分层的好处退回去了。分层的意义是「贵的那一半只在真的
    /// 要用时才搬」，搬的定义包括这一段网络。
    ///
    /// # Errors
    /// 没有这个技能（或者它被关掉了）、服务端不认识这条路（老部署），
    /// 或者网络断了。
    pub async fn skill_body(&self, name: &str) -> Result<cortex_proto::skills::SkillBody> {
        // ⚠️ 名字走 query 而不是路径段：技能名是用户随手起的，里面完全
        // 可以有斜杠、`?`、空格。塞进路径段要自己转义（而 `%2F` 在不同的
        // 反代上被规范化的方式还不一样），走 query 则由 reqwest 转义
        let resp = self
            .auth(self.http.get(self.url("/skills/body")))
            .query(&[("name", name)])
            .send()
            .await
            .map_err(map_transport)?;
        let resp = checked(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析技能正文失败：{e}")))
    }

    /// `POST /search` —— 联网检索。key 在服务端，见 `cortex_agentd::search`。
    pub async fn web_search(
        &self,
        query: &str,
        limit: Option<i64>,
        topic: Option<&str>,
        time_range: Option<&str>,
    ) -> Result<serde_json::Value> {
        let resp = self
            .auth(self.http.post(self.url("/search")))
            // 两个可选位**原样透传**，不在这一层归一：判据在服务端
            // （`normalize_topic` / `normalize_time_range`）。两处各归一
            // 一次的话，哪天放开一个新值就会漏改一处，而症状是「模型传了
            // 但没生效」——最难查的那一类
            .json(&serde_json::json!({
                "query": query,
                "limit": limit,
                "topic": topic,
                "time_range": time_range,
            }))
            .send()
            .await
            .map_err(map_transport)?;
        let resp = checked(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析搜索结果失败：{e}")))
    }

    /// `POST /fetch` —— 把一个网页读回来。key 在服务端，见 `cortex_agentd::fetch`。
    ///
    /// `offset` 用来分段读长页面：上一次结果里的 `next_offset` 原样传回来。
    pub async fn web_fetch(&self, url: &str, offset: Option<i64>) -> Result<serde_json::Value> {
        let resp = self
            .auth(self.http.post(self.url("/fetch")))
            .json(&serde_json::json!({ "url": url, "offset": offset }))
            .send()
            .await
            .map_err(map_transport)?;
        let resp = checked(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析抓取结果失败：{e}")))
    }

    /// `POST /library/search` —— 资料库全文检索。
    ///
    /// 回的是 `[{item_id, item_name, ord, body, rank}]`。**原样透传**给
    /// 模型：这一层不挑不改 —— 排序与截断都在服务端做过了（那里才知道
    /// 一次给几段合适），客户端再挑一次只会让两处判据漂移。
    pub async fn library_search(
        &self,
        query: &str,
        limit: Option<i64>,
    ) -> Result<serde_json::Value> {
        let resp = self
            .auth(self.http.post(self.url("/library/search")))
            .json(&serde_json::json!({ "query": query, "limit": limit }))
            .send()
            .await
            .map_err(map_transport)?;
        let resp = checked(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析检索结果失败：{e}")))
    }

    /// `GET /library/{id}/text` —— 按段读资料库里某一份的正文。
    pub async fn library_read(
        &self,
        item_id: &str,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<serde_json::Value> {
        let mut q: Vec<(&str, String)> = Vec::new();
        if let Some(f) = from {
            q.push(("from", f.to_string()));
        }
        if let Some(t) = to {
            q.push(("to", t.to_string()));
        }
        let resp = self
            // id 直接插值：它是 ULID（域上钉死 26 个大写字母数字，
            // 见 init 迁移），没有需要转义的字符 —— 与 `/sessions/{id}`
            // 同样的判断。技能名走 query 是因为**那个**是用户随手起的
            .auth(self.http.get(self.url(&format!("/library/{item_id}/text"))))
            .query(&q)
            .send()
            .await
            .map_err(map_transport)?;
        let resp = checked(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析正文失败：{e}")))
    }

    // 这里**没有** `pending_confirmations`。cortexd 不跑 agent，也就不再有
    // 「远端那本确认簿」可问 —— 那个端点连同它服务的那个进程内 agent 一起
    // 删掉了。跨端批确认要回来的话，得先有一个「确认属于哪台机器」的答案，
    // 而不是再挂一次 HTTP。

    /// 报一次到 —— 「这台机器上的 agent 活着，它手上有这些会话的绑定」。
    ///
    /// 返回服务端认为的 TTL（秒），调用方据此定下一次的间隔。**不要自己写一个
    /// 常数**：两侧各一个的话，改一边就会出现「agent 以为自己还在线、名册里
    /// 已经没了」，而用户看到的是「机器离线」而机器明明开着。
    ///
    /// # Errors
    /// 连不上、或者服务端认不出这把凭据。调用方应当**只打一条 debug**：
    /// 名册是锦上添花，报不上去不影响任何一轮对话。
    pub async fn heartbeat(
        &self,
        hb: &cortex_proto::presence::AgentHeartbeat,
    ) -> Result<cortex_proto::presence::HeartbeatAck> {
        let resp = self
            .auth(self.http.post(self.url("/agents/heartbeat")))
            .timeout(REQUEST_TIMEOUT)
            .json(hb)
            .send()
            .await
            .map_err(map_transport)?;
        checked(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析心跳回执失败：{e}")))
    }

    /// 起一次 LLM 代理调用，返回**原始字节流**。SSE 的解析在
    /// [`crate::provider`] 里做 —— 那边才知道要还原成什么类型。
    pub async fn llm_stream(&self, req: &LlmStreamRequest) -> Result<reqwest::Response> {
        // 刻意不设超时：模型「想」几分钟是常态，而服务端每 15 秒发一次
        // keep-alive，真断了连接自己会报错。设一个「够长」的超时，
        // 等于给最慢的那次合法推理挖一个偶发失败
        let resp = self
            .auth(self.http.post(self.url("/llm/stream")))
            .json(req)
            .send()
            .await
            .map_err(map_transport)?;
        checked(resp).await
    }

    /// 远端讲的协议能不能对话。
    ///
    /// 三态，而**三态都不能合并**：
    ///
    /// - `Ok(Ok(()))` —— 兼容，正常启动。
    /// - `Ok(Err(msg))` —— 连上了，但协议不兼容。**必须拒绝启动**：
    ///   降级运行的表现是「某个功能悄悄不对」，比起不起来难查得多。
    /// - `Err(_)` —— **连不上**。这不是不兼容，是离线，而离线是本地 agent
    ///   明确支持的形态（对话照常、写入排进 outbox）。因为拿不到远端版本
    ///   就拒绝启动，等于把「网络不好」变成「装了个用不了的东西」。
    ///
    /// # Errors
    /// 远端不可达，或 `/health` 的响应不是能解析的 JSON。
    pub async fn protocol_check(&self) -> Result<std::result::Result<(), String>> {
        let resp = self
            .auth(self.http.get(self.url("/health")))
            .timeout(STARTUP_PROBE_TIMEOUT)
            .send()
            .await
            .map_err(map_transport)?;
        let peer: cortex_proto::dto::PeerProtocol = checked(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Unavailable(format!("解析远端 /health 失败：{e}")))?;
        Ok(cortex_proto::check_peer(
            peer.protocol,
            peer.min_peer_protocol,
            "远端 cortexd",
        ))
    }

    /// 我这把凭据属于谁。
    ///
    /// 本地状态要按账号分目录（outbox、工作区绑定），而**只有远端知道
    /// 这把 token 是谁的** —— 本地拿到的是一串会轮转的 access token，
    /// 从它派生目录名会在下一次刷新时换一片新目录，把队列孤儿掉。
    ///
    /// # Errors
    /// 远端不可达，或响应不是能解析的 JSON。离线是**正常情况**，
    /// 调用方据此回落到上一次记住的账号（见 `config::user_dir`）。
    pub async fn whoami(&self) -> Result<String> {
        let resp = self
            .auth(self.http.get(self.url("/auth/me")))
            .timeout(STARTUP_PROBE_TIMEOUT)
            .send()
            .await
            .map_err(map_transport)?;
        #[derive(serde::Deserialize)]
        struct Me {
            user_id: String,
        }
        let me: Me = checked(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Unavailable(format!("解析 /auth/me 失败：{e}")))?;
        Ok(me.user_id)
    }

    /// 给会话改名。导入用它给每段对话挂上「Claude · 原标题」。
    ///
    /// 只发 `title` 一个字段：这条路径**不碰 workspace** ——
    /// 那是设备本地状态，走 `PUT /local/workspaces/{id}`（见
    /// [`crate::local_workspace`]）。
    pub async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .patch(self.url(&format!("/sessions/{session_id}"))),
            )
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({ "title": title }))
            .send()
            .await
            .map_err(map_transport)?;
        checked(resp).await.map(|_| ())
    }

    /// 声明这个会话的**执行归属**。
    ///
    /// 与 [`Self::rename_session`] 同一条路径，但语义完全不同：改名是内容，
    /// 这个是「这段对话在哪儿跑」。本地 agent 在绑定 / 解绑本机目录之后调它
    /// —— 那是唯一同时知道「路径合格」与「它在这一台机器上」的地方。
    pub async fn set_session_runtime(&self, session_id: &str, local: bool) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .patch(self.url(&format!("/sessions/{session_id}"))),
            )
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({
                "runtime": if local { "local" } else { "cloud" },
            }))
            .send()
            .await
            .map_err(map_transport)?;
        checked(resp).await.map(|_| ())
    }

    /// 取一个已登记附件的字节流（`GET /blobs/{hash}`）。
    ///
    /// 返回原始响应而不是字节：调用方要先看头（MIME、Content-Length）
    /// 再决定读进内存、流式落盘、还是就地放弃 —— 一个 300 MB 的 zip
    /// 不该先进内存再发现走错了路。策略在 `attachments` 模块，这里只管
    /// 认证与错误归一。
    ///
    /// 容器里这条打的是回调面（agentd 从对象存储中转），委托令牌的
    /// 白名单里有对应的一条 —— 少了它云端会话的附件会静默全部不可用。
    pub async fn blob_response(&self, hash: &str) -> Result<reqwest::Response> {
        // **不能用 `RequestBuilder::timeout`**：reqwest 的 per-request 超时
        // 是**总死线** —— 计时器被塞进 Response，身子读到第 15 秒照样炸。
        // 上一版就是这么写的，后果是任何下载总时长 >15s 的附件必然
        // 「下载中断」，而 512MiB 的工作区上限在 15 秒预算下根本够不到。
        // 这里只包 send()（连上 + 等到响应头）；身子的逐 chunk 空转超时
        // 由调用方（`attachments::read_capped` / `write_stream`）自己包。
        let fut = self
            .auth(self.http.get(self.url(&format!("/blobs/{hash}"))))
            .send();
        let resp = tokio::time::timeout(BLOB_HEADER_TIMEOUT, fut)
            .await
            .map_err(|_| CortexError::Unavailable("等附件响应头超时".into()))?
            .map_err(map_transport)?;
        checked(resp).await
    }

    /// 取这个会话最近的若干轮，用来铺当前这一轮的上下文。
    ///
    /// # 为什么本地 agent 也要问远端
    ///
    /// 它没有数据库。会话的原文全在远端 —— 包括**别的设备上**发生的那些轮次，
    /// 而「任何设备连上即是完整的你」正要求这台机器也看得见它们。
    ///
    /// `/sessions/{id}?limit=N` 给的是**最新 N 条、正序**，正是上下文要的
    /// （见那个 DTO 的注释：分页天然从新往老取，反转在服务端做）。
    ///
    /// # 连不上就当没有历史
    ///
    /// 离线是明确支持的形态：这一轮照样能跑，只是模型看不到前几轮 ——
    /// 与「记忆未连接」是同一类降级，不该让整轮对话失败。
    /// 顺带带回**容器工作区名**。
    ///
    /// 单独再打一次 `/sessions/{id}` 只为了拿一个字符串，是每轮多一次往返 ——
    /// 而这条请求本来就在发。返回一个结构而不是元组：加第三样东西时不必改
    /// 所有调用点的解构。
    pub async fn session_history(&self, session_id: &str, limit: i64) -> Result<RemoteSession> {
        let resp = self
            .auth(self.http.get(self.url(&format!("/sessions/{session_id}"))))
            .timeout(REQUEST_TIMEOUT)
            .query(&[("limit", limit.to_string())])
            .send()
            .await
            .map_err(map_transport)?;
        let detail: cortex_proto::dto::SessionDetail = checked(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析会话详情失败：{e}")))?;
        let container_workspace = detail.session.container_workspace.clone();
        let turns = detail
            .episodes
            .into_iter()
            .filter_map(|e| {
                // 历史轮次里的附件不重新取字节（每轮重取重编码会打穿
                // 前缀缓存，也贵）—— 只加一行**确定性**注记，让模型知道
                // 「那一轮带过什么文件、工作区里那个文件是哪来的」
                let note = crate::attachments::history_note(&e.attachments);
                match e.role.as_str() {
                    // tool / system 是内部记录，塞进上下文既占预算，
                    // 又让模型以为那是用户说的话
                    "user" => Some((true, format!("{}{note}", e.text.unwrap_or_default()))),
                    "assistant" => Some((false, e.text.unwrap_or_default())),
                    _ => None,
                }
            })
            .collect();
        Ok(RemoteSession {
            turns,
            container_workspace,
        })
    }

    /// 远端活着吗。决定这一轮是走在线路径还是排进 outbox。
    /// 远端此刻通不通。**必须快**。
    ///
    /// # 为什么超时是 600 毫秒而不是 5 秒
    ///
    /// 这个函数唯一的调用者是本地 agent 的 `/health`，而 `/health` 是
    /// **探活端点** —— 桌面端在轮询它，CLI 靠它判断该不该拉起 agent，
    /// 将来的 Docker HEALTHCHECK 也会用它。
    ///
    /// 5 秒超时的实测后果：远端不可达时 `/health` **每次要 2.04 秒**
    /// （连接被拒也要等这么久）。于是 CLI 那侧 800 毫秒的探活永远超时，
    /// 表现是「本地 agent 起不来」，而它其实好端端地跑着。
    ///
    /// 而这个函数答的问题本来就不需要 5 秒：一个 600 毫秒内没握上手的
    /// 本机/局域网远端，对「现在能不能写记忆」这个问题而言就是不可用 ——
    /// 真正的写入请求另有它自己的超时，不受这里影响。**探活宁可误报离线，
    /// 也不能让探活本身变慢**：前者下一次轮询就自己纠正了，
    /// 后者会让所有依赖它的判断一起坏掉。
    pub async fn is_reachable(&self) -> bool {
        self.http
            .get(self.url("/health"))
            .timeout(Duration::from_millis(600))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

/// 传输层失败 → `Unavailable`，**不是** `Provider`。
///
/// 这个区分是离线队列的开关：调用方据此判断「远端连不上，排队」还是
/// 「远端说我错了，别重试」。混成一类的话，一个 400（比如附件没登记）
/// 会被永远重放下去，而队列头堵死意味着后面全部对话都灌不回去。
fn map_transport(e: reqwest::Error) -> CortexError {
    CortexError::Unavailable(format!("连不上 cortexd：{e}"))
}

/// 非 2xx 一律带上响应体 —— cortexd 的错误消息是写给人看的，
/// 只报一个状态码等于把它扔掉。
async fn checked(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let detail =
        serde_json::from_str::<cortex_proto::dto::ErrorBody>(&body).map_or(body, |e| e.error);
    Err(classify(status, detail))
}

/// HTTP 状态码 → 错误种类。**调用方按种类决定下一步，所以这个映射是契约。**
///
/// 单独拎出来是为了能测：`checked` 要一个真的 `reqwest::Response`，而这里
/// 有判断的只是这三档。
fn classify(status: reqwest::StatusCode, detail: String) -> CortexError {
    // 5xx 与 408/429 是「等会儿再来」—— 唯一会让调用方排队重试的一档
    if status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429 {
        return CortexError::Unavailable(format!("cortexd {status}：{detail}"));
    }
    // **404 从 Invalid 里单拎出来。**
    //
    // 它与 400/401/403 在调用方那儿是两回事：那几个是「你发的东西不对」，
    // 404 常常只是「这东西还不存在」—— 而**新会话的第一轮必然撞到它**
    // （会话行是随第一条 episode 建的，而拉历史发生在写 episode 之前）。
    //
    // 混在一起的后果不是崩，是**噪声**：每开一个新会话，日志里就有一条
    // 「读会话历史失败」。看多了没人当回事，而真正读丢历史的那次长得一模一样。
    // 2026-08-13 生产上第一次跑云沙箱就撞见了。
    //
    // 重试行为不受影响：`NotFound` 与 `Invalid` 在每一处判定里都不是
    // `Unavailable`，所以两者一样「不重试」。
    if status.as_u16() == 404 {
        return CortexError::NotFound {
            kind: "会话",
            id: detail,
        };
    }
    // ★ **401/403 单拎出来，不归 Invalid。**
    //
    // 出站凭据是桌面端推进来的（access token，15 分钟过期），本进程自己换
    // 不了。归进 `Invalid` 的话它以一段纯文本混在事件流里回到界面，而桌面端
    // 的过期探测只接它自己那条 HTTP 线 —— 看不见，于是不续期、不推新凭据，
    // 这个进程抱着一把死 token 一直用下去，**要重启应用才恢复**。实测过。
    if matches!(status.as_u16(), 401 | 403) {
        return CortexError::Unauthorized(format!("cortexd {status}：{detail}"));
    }
    CortexError::Invalid(format!("cortexd {status}：{detail}"))
}

#[cfg(test)]
mod tests {

    /// 换凭据必须让**所有克隆**都看见 —— `Remote` 被克隆进 LocalState、
    /// 进每一个轮次的 host，各存各的话换了等于没换（只有换的那份知道）。
    #[test]
    fn a_rotated_credential_is_visible_through_every_clone() {
        let a = Remote::new("http://127.0.0.1:1", Some("old".into())).unwrap();
        let b = a.clone();
        a.set_token(Some("new".into()));
        assert_eq!(
            b.token().as_deref(),
            Some("new"),
            "克隆出去的那份还拿着旧凭据 —— 轮换之后它发的每个请求都会 401"
        );
    }

    /// **换凭据要能把睡着的人叫醒，而且不许漏掉「拨号与开睡之间」那一下。**
    ///
    /// 这一位服务的是隧道那条路：agentd 的 access token 一重启全部失效
    /// （有意的取舍），于是每次发版 worker 握手都 401，然后睡十分钟。
    /// 桌面端几秒后就把新凭据推了进来 —— 收不到这一声的话，一台开着的
    /// 机器在一次日常发版之后离线十分钟。
    #[tokio::test]
    async fn a_credential_push_wakes_whoever_is_waiting_on_it() {
        let r = Remote::new("http://127.0.0.1:1", Some("old".into())).unwrap();
        let mut seen = r.token_generation();
        seen.mark_unchanged();

        // ── 竞态那一支：**先推，后等** ──
        //
        // 时序是「拨号被 401 拒 →（推）→ 才开始等」。用 `Notify` 写的话
        // 这一下会丢，而这条测试正是钉住它不许退化成 `Notify`
        r.set_token(Some("new".into()));
        tokio::time::timeout(std::time::Duration::from_secs(1), seen.changed())
            .await
            .expect("推在等之前，这一声不许丢 —— 丢了就白等十分钟")
            .expect("发送端还活着");

        // ── 正常那一支：先等，后推 ──
        seen.mark_unchanged();
        let waiter = tokio::spawn(async move { seen.changed().await });
        r.set_token(Some("newer".into()));
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("推来的凭据必须当场叫醒等着的人")
            .expect("任务没 panic")
            .expect("发送端还活着");
    }

    /// 空串必须落成「没有凭据」而不是「凭据是空串」。
    ///
    /// 后者的实测症状：agent 拿到 `Some("")`，以为自己有认证，
    /// 于是发一个 `Authorization: Bearer ` 出去，而远端回的 401
    /// 与「凭据过期」长得一模一样。
    #[test]
    fn a_blank_credential_means_none_not_empty() {
        let r = Remote::new("http://127.0.0.1:1", Some("x".into())).unwrap();
        for blank in ["", "   ", "	"] {
            r.set_token(Some(blank.into()));
            assert_eq!(r.token(), None, "空串 {blank:?} 被当成了一把凭据");
        }
    }

    use super::*;
    use reqwest::StatusCode;

    /// 四档的分界。**这是契约**：调用方按种类决定排队重试、当场放弃、
    /// 还是去换一把凭据。
    #[test]
    fn 状态码分成该重试_不存在_凭据被拒_与你发错了四档() {
        let retryable = [500, 502, 503, 408, 429];
        for code in retryable {
            let e = classify(StatusCode::from_u16(code).unwrap(), "x".into());
            assert!(
                matches!(e, CortexError::Unavailable(_)),
                "{code} 该是 Unavailable（唯一会被排进 outbox 重试的一档），实际 {e:?}"
            );
        }

        let e = classify(StatusCode::NOT_FOUND, "找不到 session：abc".into());
        assert!(
            matches!(e, CortexError::NotFound { .. }),
            "404 必须与 400/401/403 分开：新会话的第一轮必然撞到它，\
             而那时候空历史是正确答案、不是失败。实际 {e:?}"
        );

        for code in [400, 409, 422] {
            let e = classify(StatusCode::from_u16(code).unwrap(), "x".into());
            assert!(
                matches!(e, CortexError::Invalid(_)),
                "{code} 该是 Invalid（你发的东西不对，重试没用），实际 {e:?}"
            );
        }

        // ★ **401/403 必须与 400 那族分开。**
        //
        // 归进 Invalid 的话，出站凭据过期以后就只是「一句非法输入」混在
        // 事件流里 —— 桌面端的过期探测只接它自己那条 HTTP 线，看不见这条，
        // 于是不续期、不推新凭据，本机 agent 抱着死 token 用到应用重启。
        // 那正是 2026-08-30 用户报的「过一会儿就不能用了」。
        for code in [401, 403] {
            let e = classify(StatusCode::from_u16(code).unwrap(), "x".into());
            assert!(
                matches!(e, CortexError::Unauthorized(_)),
                "{code} 该是 Unauthorized（换一把凭据再来），实际 {e:?} ——                  归进 Invalid 的话客户端没有任何线索去续期",
            );
        }
    }

    /// 挪走 404 **不能**改变「要不要重试」。
    ///
    /// 每一处重试判定写的都是 `matches!(e, Unavailable(_))`，`NotFound` 与
    /// `Invalid` 在那儿都是 false。这条把它钉住 —— 谁哪天把 NotFound 也归进
    /// 可重试，outbox 会对着一个永远不存在的东西重试到天荒地老。
    #[test]
    fn 挪走_404_没有把它变成可重试() {
        let before_and_after = [
            classify(StatusCode::NOT_FOUND, "x".into()),
            classify(StatusCode::BAD_REQUEST, "x".into()),
            classify(StatusCode::UNAUTHORIZED, "x".into()),
        ];
        for e in before_and_after {
            assert!(
                !matches!(e, CortexError::Unavailable(_)),
                "{e:?} 不该被判成可重试 —— 重试它只会一直失败下去"
            );
        }
    }
}
