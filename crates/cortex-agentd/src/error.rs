//! 这个进程回给客户端的错误。
//!
//! # 为什么不直接用 `CortexError`
//!
//! `CortexError` 的 `Display` 会给每个变体挂前缀（「存储错误：」之类）。
//! 那对日志有用，对用户是噪声 —— 一句「用户名或密码不对」前面挂着
//! 「存储错误：」，读到的人会以为服务坏了而不是自己打错了字。
//!
//! 所以这里在它之上多两样东西：可覆盖的文案，与可覆盖的状态码。
//! 后者只在**领域错误分不出来**的那几处用（401 / 403 / 429 / 501 都是
//! 部署形态或凭据状态的属性，不是领域概念，为它们污染全局错误类型不划算）。
//!
//! 这一份是从 cortexd 搬过来的 —— 客户端 `api_exception.dart` 按状态码
//! 分支（尤其 501 会被当成「这条路永远不会开」而**不重试**），两边的
//! 语义必须逐条对上，重写一份只会漂开。

use axum::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use cortex_proto::dto::ErrorBody;

#[derive(Debug)]
pub struct ApiError {
    inner: cortex_core::CortexError,
    /// 覆盖 `inner` 的文案。见模块文档。
    message: Option<String>,
    /// 覆盖 [`cortex_core::CortexError::http_status`] 的默认映射。
    status: Option<StatusCode>,
}

impl ApiError {
    /// 400：客户端送来的东西不对。
    pub fn bad_request(message: impl Into<String>) -> Self {
        cortex_core::CortexError::Invalid(message.into()).into()
    }

    /// 401：没有有效凭据。
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("unauthorized".into()),
            status: Some(StatusCode::UNAUTHORIZED),
        }
    }

    /// 403：认得出你是谁，但这件事不允许。
    ///
    /// 与 401 分开：401 的意思是「去登录」，403 是「登录了也不行」。
    /// 注册关闭时回 403 而不是 401，否则客户端会去弹一个没用的登录框。
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("forbidden".into()),
            status: Some(StatusCode::FORBIDDEN),
        }
    }

    /// 409：撞上了并发的另一次操作，**重试有意义**。
    ///
    /// 与 401 分开是有具体后果的：客户端把 refresh 的 401/403 判为
    /// 「凭据被拒」并**删掉本机存的凭据**；而并发续期的输家撞进宽限缓存
    /// 还没被填上的那几毫秒时，凭据链完好、重试一次就能拿到同一对 ——
    /// 回 401 会让一个毫秒级竞态把用户登出。
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("conflict".into()),
            status: Some(StatusCode::CONFLICT),
        }
    }

    /// 404：这个东西不在。
    ///
    /// 与 400 分开是有具体后果的：分享链接被撤销之后再打开，回 400 的话
    /// 浏览器地址栏那一页写的是「错误的请求」——**而请求一点没错**，
    /// 是那张图不在了。用户会去检查自己有没有复制全。
    ///
    /// ⚠️ 这条消息**不该区分「从来没有」与「撤销了」**：区分开就是一个
    /// 可探测的信息面（拿一堆 token 来问，能问出哪些曾经存在过）。
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("not found".into()),
            status: Some(StatusCode::NOT_FOUND),
        }
    }

    /// 413：这个体积在这条路上搬不动。
    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            status: Some(StatusCode::PAYLOAD_TOO_LARGE),
            inner: cortex_core::CortexError::Invalid(String::new()),
        }
    }
    /// 429：额度用完了，或者来得太密（认证端点的限流，见 [`crate::rate_limit`]）。
    ///
    /// 与 403 分开：403 的意思是「这件事你永远不能做」，429 是
    /// 「现在不行，等等就行」。客户端据此决定要不要显示重试 ——
    /// 尤其 403 会被当成「凭据废了」触发登出，限流命中绝不能走那条路。
    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("quota".into()),
            status: Some(StatusCode::TOO_MANY_REQUESTS),
        }
    }

    // 503（暂时不可用）**还没有跟过来**：它的构造方在记忆那一侧的检索路径上
    // （`CortexError::Unavailable` 的默认映射），这个进程还没有会走到它的路由。
    // 搬一个没人构造的错误变体过来，只会多一条编译警告。

    /// 500：我们这边出了问题。
    pub fn internal(message: impl Into<String>) -> Self {
        cortex_core::CortexError::Store(message.into()).into()
    }

    /// 501：请求本身没错，是这个部署形态**永远**提供不了这个能力。
    ///
    /// 客户端把 501 当成「这条路不会开」，据此把功能降级掉并且**不重试**
    /// （`api_exception.dart` 的 `isUnsupported`）。所以只有真正的永久缺失
    /// 才配用它 —— 「数据库这会儿连不上」不是永久缺失，那该是 503。
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("unsupported".into()),
            status: Some(StatusCode::NOT_IMPLEMENTED),
        }
    }

    /// 给人看的那句话。
    ///
    /// SSE 那条路要把它塞进事件里 —— 那里没有 HTTP 状态码可用，
    /// 只有一个 `error` 事件。
    #[must_use]
    pub fn message(&self) -> String {
        self.message
            .clone()
            .unwrap_or_else(|| self.inner.to_string())
    }

    /// 最终会回出去的状态码。
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status.unwrap_or_else(|| {
            StatusCode::from_u16(self.inner.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        })
    }
}

impl From<cortex_core::CortexError> for ApiError {
    fn from(e: cortex_core::CortexError) -> Self {
        Self {
            inner: e,
            status: None,
            message: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let code = self.status();
        tracing::warn!(error = %self.inner, status = code.as_u16(), "请求失败");
        (
            code,
            Json(ErrorBody {
                error: self.message(),
            }),
        )
            .into_response()
    }
}
