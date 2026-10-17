use serde::{Deserialize, Serialize};

/// 一条 `episode_blobs` 关联 —— **上行**方向（客户端 → 服务端）。
///
/// 只带客户端知道而服务端不知道的东西。`mime` 与 `size_bytes` 刻意不在这里：
/// 它们由字节唯一决定，登记 blob 时服务端已经嗅探/记下了，让客户端再报一遍
/// 只会多出一条「客户端说的和字节里写的不一样」要处理的路。
/// 下行方向见 [`AttachmentDto`]。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRef {
    /// 已登记 blob 的 SHA-256（小写十六进制）
    pub hash: String,
    /// 语义标签（image / audio / video / document …）。
    /// 与 `episode_blobs.kind` 一列直通，schema 里刻意不枚举，这里也就不枚举。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// 用户看到的文件名。
    ///
    /// 存在 `episode_blobs` 而不是 `blobs` 上：内容寻址下同一份字节可以有
    /// 多个文件名（同一张图今天叫「设计稿.png」，下周转发过来叫「IMG_2043.png」），
    /// 文件名是**这一次引用**的属性。
    ///
    /// 默认 `None`，因此老客户端不传也照常工作 —— 只是回放时显示不出名字。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

/// 一条附件 —— **下行**方向（服务端 → 客户端）。
///
/// 比 [`AttachmentRef`] 多 `mime` 与 `size_bytes`。在此之前这两样不下发，
/// 重开会话时一个 PDF 只能显示成「文档 · a1b2c3d4」：客户端手上只有哈希，
/// 要知道这是什么就得对每个附件再发一次请求，而在那之前界面上没有任何
/// 东西可画。字段是纯新增，老客户端忽略它们即可。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentDto {
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `null` = 未知（本次改动之前落库的老数据，原始文件名从未被记录过）。
    /// 客户端应回落到「文档 · <hash 前 8 位>」那套显示。
    pub filename: Option<String>,
    /// 由字节头嗅探得出的类型，来自 `blobs.mime`
    pub mime: String,
    pub size_bytes: i64,
}
