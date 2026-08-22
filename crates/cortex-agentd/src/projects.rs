//! 项目 —— 会话的分组容器。
//!
//! # 为什么它跟着会话一起过来
//!
//! 判据仍然只有一句：**这张表离开记忆能力还有没有意义。**「把哪几个会话
//! 归成一组」问的是产品的组织方式，与「抽出了哪些事实」毫无关系 ——
//! 它留在记忆服务那侧唯一的理由曾经是「那边有库」。
//!
//! 而它必须与会话**同一批**搬：`PATCH /sessions/{id}` 的 `project_id` 要
//! 先确认目标项目存在（见 [`crate::sessions`] 里那段），拆成两批的话，
//! 中间那一版会向一个自己读不到的库问「这个项目在不在」。
//!
//! # 这里没有「已归档的项目」
//!
//! 想收起来就删掉它 —— 而**删项目不删内容**：里面的会话变回未分组，消息、
//! 附件、已抽取的事实一条不少。界面文案上别把它说成「彻底删除」。

use axum::extract::{Path, State};
use axum::{Json, http::HeaderMap};
use cortex_core::{CortexError, Id};
use cortex_proto::dto::{NewProjectRequest, ProjectDto, ProjectPatch, ProjectsResponse};
use cortex_store::Store;

use crate::error::ApiError;
use crate::sessions::store_err;
use crate::state::{AgentState, DEVICE_ID};

// ─────────────────────────── 路由 ───────────────────────────

/// `GET /projects` —— 现存的项目。已删除的不在其中。
pub async fn list(
    State(st): State<AgentState>,
    headers: HeaderMap,
) -> Result<Json<ProjectsResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(ProjectsResponse {
        projects: list_projects(tenant.store()?).await?,
    }))
}

/// `POST /projects` —— 建一个项目，id 由服务端生成并在响应里给回。
pub async fn create(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<NewProjectRequest>,
) -> Result<Json<ProjectDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(create_project(tenant.store()?, &req.name).await?))
}

/// `PATCH /projects/{id}` —— 改名。回的是改完之后的末态。
pub async fn patch(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(patch): Json<ProjectPatch>,
) -> Result<Json<ProjectDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(patch_project(tenant.store()?, &id, patch).await?))
}

/// `DELETE /projects/{id}` —— **解散分组，不动内容**。
///
/// 回一个空对象而不是 204：客户端的 JSON 解码路径因此保持划一，
/// 少一处「这个响应没有 body」的特例。
pub async fn delete(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    delete_project(tenant.store()?, &id).await?;
    Ok(Json(serde_json::json!({})))
}

// ─────────────────────────── 数据 ───────────────────────────

async fn list_projects(store: &Store) -> cortex_core::Result<Vec<ProjectDto>> {
    let rows = store.projects().await.map_err(store_err)?;
    Ok(rows.into_iter().map(project_dto).collect())
}

/// 建一个项目。id 由**服务端**生成。
///
/// 与会话相反（那边 id 由客户端生成，因为它要在离线时先建起来）：
/// 项目只在联网时能建 —— 它的全部意义是「服务端知道这个分组存在」，
/// 离线建一个本地项目再等着合并，会引出「两台设备各建了一个同名项目」
/// 这种要人工消歧的状态，而分组这件事不值得。
async fn create_project(store: &Store, name: &str) -> cortex_core::Result<ProjectDto> {
    let name = validate_project_name(name)?;
    let id = Id::new().to_string();
    let event =
        cortex_store::NewProjectEvent::create(&id, &name, cortex_store::Actor::User, DEVICE_ID);

    store
        .write_txn(async |t| t.insert_project_event(&event).await)
        .await
        .map_err(store_err)?;

    // 不拿手上的 name 拼一个 DTO 回去，而是读回末态：并发下另一台设备
    // 可能刚好改了名，此时回显本次请求的名字就是在告诉客户端一件
    // 已经不成立的事。多一次读换掉一整类「界面显示的与库里存的不一样」
    project_overview(store, &id).await
}

async fn patch_project(
    store: &Store,
    id: &str,
    patch: ProjectPatch,
) -> cortex_core::Result<ProjectDto> {
    // 改名与置顶是两台**独立**的状态机，所以是两条并列的分支而不是二选一：
    // 同一个 PATCH 里两个都给就写两条事件，各自生效
    let mut events = Vec::new();

    if let Some(raw) = patch.name.as_deref() {
        let name = validate_project_name(raw)?;
        events.push(cortex_store::NewProjectEvent::rename(
            id,
            &name,
            cortex_store::Actor::User,
            DEVICE_ID,
        ));
    }

    if let Some(pinned) = patch.pinned {
        events.push(if pinned {
            cortex_store::NewProjectEvent::pin(id, cortex_store::Actor::User, DEVICE_ID)
        } else {
            cortex_store::NewProjectEvent::unpin(id, cortex_store::Actor::User, DEVICE_ID)
        });
    }

    if events.is_empty() {
        return Err(CortexError::Invalid(
            // 清单要跟着字段涨。漏一个的症状是「明明改了却报没改」——
            // 而那条错误信息本身就是排查这件事时唯一的线索
            "请求体里没有任何要改的字段（name / pinned）".into(),
        ));
    }

    // 动手之前先确认它还在。少了这一句，给一个已删除项目改名会**静默成功**
    // （事件照样落库），而列表里什么都不会出现 —— 用户看到的是「改完就没了」
    require_project(store, id).await?;

    store
        .write_txn(async |t| {
            for e in &events {
                t.insert_project_event(e).await?;
            }
            Ok(())
        })
        .await
        .map_err(store_err)?;

    project_overview(store, id).await
}

/// 删项目 —— **解散分组，不动内容**。
///
/// 里面的会话变成未分组（`session_state` 把指向已删除项目的归属收敛成
/// NULL），消息、附件、已抽取的事实一条不少。刻意不级联写一批
/// `remove_from_project`：那是 N 条写入，而且会让「撤销误删」变成不可能 ——
/// 现在只要用同一个 id 再 create 一次，所有会话的归属原样回来。
///
/// **重复删是 404 而不是 200**：客户端在两台设备上各删一次是常见的，
/// 但把「已经不在了」说成成功，会让「我删的是哪一个」这种错误永远查不出来。
async fn delete_project(store: &Store, id: &str) -> cortex_core::Result<()> {
    require_project(store, id).await?;

    let event = cortex_store::NewProjectEvent::delete(id, cortex_store::Actor::User, DEVICE_ID);
    store
        .write_txn(async |t| t.insert_project_event(&event).await)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// 项目此刻的末态。不存在（或已删）时 404。
async fn project_overview(store: &Store, id: &str) -> cortex_core::Result<ProjectDto> {
    Ok(project_dto(require_project(store, id).await?))
}

/// 「这个项目现在还在吗」—— 读一次末态，不在就 404。
///
/// 单独抽出来是因为改名与删除都要它，而两处都必须在**进写事务之前**问：
/// 写事务持着 advisory lock，纪律要求它短小纯写
/// （`cortex-store::txn` 的第三条）。
///
/// 把会话移进项目那一处**不**走这里：那条路上项目不存在是 400 不是 404，
/// 理由见 [`crate::sessions::patch`] 背后那段注释。
async fn require_project(store: &Store, id: &str) -> cortex_core::Result<cortex_store::Project> {
    store
        .project(id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| CortexError::NotFound {
            kind: "project",
            id: id.into(),
        })
}

// ─────────────────────────── 映射 ───────────────────────────

fn project_dto(p: cortex_store::Project) -> ProjectDto {
    ProjectDto {
        id: p.project_id,
        name: p.name,
        created_at: p.created_at.to_rfc3339(),
        session_count: p.session_count,
        pinned: p.pinned,
    }
}

/// 校验一个项目名，返回 trim 之后的那份。
///
/// # 为什么是一个独立的纯函数
///
/// 真正的项目路径要一个数据库才进得去，于是这条判断在那里测不到 ——
/// 而它恰恰是最该有测试的一条。
///
/// # 为什么 trim 之后判空，而不是直接判空
///
/// 输入框里敲一个空格就能造出一个名字是空白的项目，而它在侧边栏上是一行
/// 什么都没有的可点区域 —— 看起来像界面坏了。
///
/// 上限按**字符**而非字节数：数据库那道 CHECK 用的是 `length()`，
/// 它数的也是字符。两处口径不同的症状是「明明没超，服务端却回 500」。
fn validate_project_name(raw: &str) -> cortex_core::Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(CortexError::Invalid("项目名不能为空白".into()));
    }
    if name.chars().count() > cortex_store::PROJECT_NAME_MAX_CHARS {
        return Err(CortexError::Invalid(format!(
            "项目名过长（上限 {} 字符）",
            cortex_store::PROJECT_NAME_MAX_CHARS
        )));
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_name_must_survive_trimming() {
        assert_eq!(
            validate_project_name("  记忆引擎  ").expect("两侧空白应当被去掉而不是拒绝"),
            "记忆引擎"
        );

        for bad in ["", "   ", "\t\n"] {
            assert!(
                validate_project_name(bad).is_err(),
                "{bad:?} 应当被拒 —— 一个名字全是空白的项目在侧边栏上是一行\
                 什么都没有的可点区域，看起来像界面坏了"
            );
        }
    }

    #[test]
    fn the_project_name_limit_counts_characters_not_bytes() {
        let max = cortex_store::PROJECT_NAME_MAX_CHARS;

        // 恰好顶格的中文名：按字节算的话它是 3 倍长度，会被误拒
        let full = "记".repeat(max);
        assert_eq!(
            validate_project_name(&full)
                .expect("恰好顶格应当通过 —— 上限按字符数，与数据库 length() 一致"),
            full
        );

        assert!(
            validate_project_name(&"记".repeat(max + 1)).is_err(),
            "超一个字符就该在这里被拒，而不是让数据库的 CHECK 变成一句 500"
        );
    }
}
