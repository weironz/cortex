use serde::{Deserialize, Serialize};

// ─────────────────────────── /projects ─────────────────────────

/// 一个项目 —— 会话的分组容器。
///
/// # 它刻意没有的东西
///
/// 没有 `archived`：项目本身就是个轻量分组，再加一层「已归档的项目」
/// 只会让界面多一个没人用的过滤器。想收起来就删掉它 —— 而删项目**不删内容**，
/// 里面的会话只是变回未分组（见 [`ProjectsResponse`]）。
///
/// 没有 `description` / `instructions`：那是「项目级系统提示词」那一档功能，
/// 与分组是两件事。现在留一个永远为空的字段，只会让三端都先写一遍它的
/// 渲染与编辑，然后发现语义还没定下来。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    /// 创建时间（RFC 3339）
    pub created_at: String,
    /// 这个项目下**会出现在会话列表里**的会话数：已归档的不算，
    /// 一条消息都还没有的也不算。口径与 `GET /sessions?project_id=` 一致 ——
    /// 对不上的话，用户会看到「3 个会话」点进去只有 2 个。
    pub session_count: i64,
    /// 置顶 —— 左栏「项目」那一段只列这些，项目页上则全都列。
    ///
    /// 所以它**不改变可见性**，只决定「要不要一直摆在左栏」。
    /// 老服务端不发这个字段 = 全部按未置顶算，左栏那一段是空的。
    #[serde(default)]
    pub pinned: bool,
}

/// `GET /projects` 的响应。
///
/// **不含已删除的项目**。删项目是一条墓碑事件：项目从列表消失，
/// 里面的会话变成未分组，而消息、附件、已抽取的记忆一条没少 ——
/// 界面文案上别把它说成「彻底删除」，那是 redact / purge 的事。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectsResponse {
    pub projects: Vec<ProjectDto>,
}

/// `POST /projects` 的请求体。
#[derive(Debug, Deserialize)]
pub struct NewProjectRequest {
    /// trim 之后必须非空、不超过 100 字符。
    pub name: String,
}

/// `PATCH /projects/{id}` 的请求体。
///
/// 目前只有改名一件事可做。仍然用 PATCH + 可选字段而不是
/// `POST /projects/{id}/rename`：将来加第二个可改属性时，
/// 前者是加一个字段，后者是再开一条路径、再写一遍鉴权与租户解析。
#[derive(Debug, Default, Deserialize)]
pub struct ProjectPatch {
    #[serde(default)]
    pub name: Option<String>,
    /// true = 置顶，false = 取消置顶。
    ///
    /// 与 `name` 各自是一次事件：同一个请求里两个都给，就写两条 ——
    /// 「改名顺带置顶」在事件流里本来就是两件事。
    #[serde(default)]
    pub pinned: Option<bool>,
}
