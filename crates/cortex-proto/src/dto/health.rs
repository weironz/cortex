use serde::{Deserialize, Serialize};

// ─────────────────────────── /health ───────────────────────────

/// 从对端 `/health` 里**只取协议那两个数**。
///
/// # 为什么不直接反序列化 [`Health`]
///
/// 两个理由，第二个是硬的：
///
/// 1. `Health` 有一半字段是 `&'static str`，反序列化不了。
/// 2. 更重要的是**耦合面**：协议检查只关心两个数字，而 `Health` 还带着
///    数据库状态、对象存储后端、向量化健康度。用整个 `Health` 去解，
///    等于让那些字段的任何变动都能把协议检查本身弄坏 ——
///    而协议检查坏掉的表现，恰恰是「不兼容时没人拦」。
///
/// # 缺省值是 1，不是 0
///
/// **这是这个类型最容易写错的地方。** 这个检查上线之前的 cortexd
/// （包括此刻正跑在生产上的那个）压根不报这两个字段。缺省取 0 的话，
/// 检查生效当天所有桌面端都会拒绝启动 —— 而它们其实完全能用。
///
/// 字段缺席的正确含义是「这是个早于本检查的守护进程」，也就是协议 v1
/// （本检查引入时的版本），不是「协议版本为零」。
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PeerProtocol {
    #[serde(default = "protocol_before_this_check")]
    pub protocol: u32,
    #[serde(default = "protocol_before_this_check")]
    pub min_peer_protocol: u32,
}

const fn protocol_before_this_check() -> u32 {
    1
}

/// `/health` 的响应 —— **cortexd 与本地 agent 共用一份**。
///
/// # 为什么以前是三份
///
/// cortexd 有一个强类型的 `Health`，本地 agent 手拼一个 `serde_json::json!`，
/// 而 CLI 自己又猜了第三份（`database` 还是必填）。结果：`cortex --server`
/// 指向本地 agent 时，`health` 子命令直接报解码失败 —— 实测过。
///
/// 三份的根因是这个类型只有 `Serialize`：客户端读不回来，只好自己再写一个。
/// 现在双向，且**两端形状不同的字段一律 `Option`** —— 本地 agent 没有数据库
/// 与对象存储，cortexd 没有沙箱状态与 outbox 积压。用 `Option` 而不是各写
/// 一个类型：调用方要处理的是「这一项这个后端有没有」，而不是「我连的是谁」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub version: String,
    /// 构建时那个 git 提交（短 sha）。老服务端不报，那时是 `None`。
    ///
    /// **与 `version` 分开报，因为 semver 不够。** 它打完 tag 的下一秒就
    /// 不再唯一：之后每个提交都还报同一个版本号，于是「线上有没有这个修复」
    /// 只看版本号会得出错误结论。见 `cortex_core::BUILD_SHA`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// 本端是什么："cortexd" / "local-agent"。
    ///
    /// 客户端据此把话说对：连着本地 agent 时，「数据库未接」不是故障而是
    /// 本来就没有。老服务端不报这个字段，按 cortexd 处理 —— 那时本地 agent
    /// 还不存在，不会误判。
    #[serde(default = "role_cortexd")]
    pub role: String,
    /// 这个进程的 LLM 走哪条路：`"direct:{provider}/{model}"` 或
    /// `"proxy:{remote}"`。只有本地 agent 报。
    ///
    /// # 为什么值得一个字段
    ///
    /// 直连与代理的差别（谁的 key、打谁的账单、模型是哪个）从界面上
    /// **完全看不出来** —— `LlmRoute::from_env` 的注释担心的「以为自己在
    /// 直连、实际跑着服务端的模型」，2026-08-24 实测差点应验：判断当时
    /// 靠的是手起一个进程看 tracing。观测点要在 /health，不在日志堆里。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<String>,
    /// 线协议版本。见 [`crate::PROTOCOL_VERSION`]。
    ///
    /// 与 `version` 分开报：后者每发一版都变，而这个只在契约不兼容地
    /// 变了时才动。本地 agent 据此决定要不要启动。
    pub protocol: u32,
    /// 本端还能对话的最老对端协议版本。见 [`crate::MIN_PEER_PROTOCOL`]。
    ///
    /// **必须报出来**：「服务端已经不再支持这么老的客户端」这件事只有
    /// 服务端知道，不报的话客户端只能连上去之后在某个具体请求上莫名其妙地失败。
    pub min_peer_protocol: u32,
    /// "ok" / "not_wired" / 具体错误。**本地 agent 没有这一项**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// 对象存储走的是哪一路后端："s3" / "local_fs" / "unavailable"。
    /// 生产环境上看到 `local_fs` 就是一条告警：媒体只落在本机。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_backend: Option<String>,
    /// 认证形态："token" / "disabled"。
    ///
    /// **`/health` 本身不需要认证**，理由是它的消费者是 Docker HEALTHCHECK
    /// 与负载均衡探针，那些东西配不了凭据；给它加认证的直接后果是容器
    /// 一直是 unhealthy，然后有人会把 HEALTHCHECK 删掉。
    ///
    /// 代价是这几个字段对任何能连上端口的人可见。逐个看：版本号、数据库
    /// 通不通、对象存储后端、以及这一行 —— 都是**部署形态**，不是用户数据。
    /// 而这一行反过来是必需的：没有它，「我到底开没开认证」只能靠去翻服务器上
    /// 的环境变量，而那正是最该能远程一眼看到的东西。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// 向量化的当前状态。**换模型之后这里是唯一能一眼看出问题的地方。**
    ///
    /// 换模型时四条向量查询会按新的 `embedding_model` 过滤，旧事实**当天就
    /// 退出向量召回**；而 BM25 / 图遍历 / recency 三路照常工作，所以它
    /// 看起来不像坏了，只像「语义召回好像变差了点」。`stale` 不为 0
    /// 就是这件事正在发生。
    ///
    /// 老客户端忽略这个字段即可；`Option` 让 mock 后端（没有数据库可查）
    /// 也能照常返回 `/health`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingHealth>,
    /// 工具沙箱一句话。**只有本地 agent 有** —— 服务端那侧由启动日志承担。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// 上游服务端连不连得上、离线队列积压多少。**只有本地 agent 有**。
    ///
    /// 这个字段以前叫 `memory`，因为那时本地 agent 的上游就是记忆服务。
    /// 会话搬进 Cortex 自己的库、记忆整个去掉之后，它指的是 **agentd** ——
    /// 名字不改的话，健康页上会写着「记忆：已连接」，而这个部署根本没有记忆。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerHealth>,
    /// 这个 agent **做不做得到**操作电脑（截屏 + 键鼠）。**只有本地 agent 有**。
    ///
    /// # 界面据此决定摆不摆那个开关
    ///
    /// 做不到有两种情形：跑在容器里（没有屏幕），或者这个构建没编进那一组
    /// （Linux 桌面）。两种都不是「关着」而是「没有」—— 摆一个打开也没用的
    /// 开关，比没有这个开关更糟（CLAUDE.md 约束 2）。
    ///
    /// 老服务端不报，那时是 `None`，界面按「没有」处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<bool>,
    /// 电脑操作的细粒度能力。**只有本地 agent 能报。**
    ///
    /// [`Self::computer_use`] 是旧客户端的兼容字段，只能回答「这一组有没有
    /// 编进当前构建」。新客户端必须看这一份：截全屏、列窗口、执行动作、
    /// 浏览器控制和后台输入是五个不同边界，把它们压成一个 bool 会让界面
    /// 宣传一个必然失败的能力。
    ///
    /// 缺字段表示对端还是旧版本，客户端应回退到 `computer_use`；字段里的
    /// 每个 bool 都由 agent 的适配层按真实驱动能力填写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_capabilities: Option<ComputerCapabilities>,
}

fn role_cortexd() -> String {
    "cortexd".into()
}

/// `/health` 里的向量化一节。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingHealth {
    /// 当前配置的模型标识，形如 `api:bge-m3` / `fastembed:…` / `hash-stub-v1`。
    pub model: String,
    /// 在当前模型下**有**向量、因而进得了向量召回的有效事实数。
    pub covered: i64,
    /// 只有别的模型的向量、因而**进不了**向量召回的有效事实数。
    ///
    /// 回填器跑起来之后这个数会往下走。它长期不为 0 说明回填被关掉了
    /// （`CORTEX_REEMBED=off`），或者一直在失败 —— 后者日志里会有。
    pub stale: i64,
}
/// 本地 agent 与它的上游服务端之间那条线。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerHealth {
    /// 上游地址（`--remote`）。
    pub remote: String,
    /// 刚才那次探活通没通。
    pub reachable: bool,
    /// 离线队列里还压着多少条没刷出去。
    pub backlog: u64,
}

/// 本地 agent 对 computer use 的细粒度自报。
///
/// 这里没有 `Default = all true` 之类的捷径：缺省值会变成一次静默授权。
/// 构造方必须逐项说清驱动真的做到了什么。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerCapabilities {
    /// 能列出当前用户可访问的应用。
    pub list_apps: bool,
    /// 能列出窗口及其稳定身份。
    pub list_windows: bool,
    /// 能截取显示器或窗口并返回可验证的 frame。
    pub observe: bool,
    /// 能向 allowlisted 应用投递点击、输入、按键或滚动。
    pub act: bool,
    /// 有独立的浏览器结构化通道，而不是只能操作整屏。
    pub browser: bool,
    /// 能在不抢用户焦点的情况下操作已识别窗口。
    pub background_input: bool,
    /// 能识别 secure field / UAC / 锁屏等不可自动操作的状态。
    pub secure_input_detection: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health_json() -> serde_json::Value {
        serde_json::json!({
            "status": "ok",
            "version": "0.1.0",
            "role": "local-agent",
            "protocol": 1,
            "min_peer_protocol": 1,
            "computer_use": true,
            "computer_capabilities": {
                "list_apps": true,
                "list_windows": false,
                "observe": true,
                "act": false,
                "browser": false,
                "background_input": false,
                "secure_input_detection": true
            }
        })
    }

    #[test]
    fn computer_capabilities_round_trip_without_relaxing_unknown_fields() {
        let health: Health = serde_json::from_value(health_json()).expect("细粒度能力必须能解析");
        let capabilities = health
            .computer_capabilities
            .expect("测试 JSON 明确带了能力");
        assert!(capabilities.list_apps);
        assert!(!capabilities.list_windows);
        assert!(capabilities.observe);
        assert!(!capabilities.act);
        assert!(!capabilities.browser);
        assert!(!capabilities.background_input);
        assert!(capabilities.secure_input_detection);

        let wire = serde_json::to_value(health).expect("健康响应必须能序列化");
        assert_eq!(
            wire["computer_capabilities"]["observe"], true,
            "序列化后必须保留真实的能力边界"
        );
        assert_eq!(
            wire["computer_capabilities"]["act"], false,
            "不能把 observe 自动升格为 act"
        );
    }

    #[test]
    fn old_health_without_capabilities_stays_compatible() {
        let mut value = health_json();
        value
            .as_object_mut()
            .expect("测试 JSON 是对象")
            .remove("computer_capabilities");
        let health: Health =
            serde_json::from_value(value).expect("旧 agent 的健康响应必须继续可读");
        assert_eq!(health.computer_use, Some(true));
        assert!(
            health.computer_capabilities.is_none(),
            "缺字段必须保持 None，不能伪造细粒度能力"
        );
    }
}
