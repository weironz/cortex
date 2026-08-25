//! 分叉会话 —— 把一段历史**复制**成一条新会话，旧会话一个字节不动。
//!
//! # 为什么是复制，而不是「新会话引用旧消息」
//!
//! 引用方案要让每条读路径（分页、搜索、导出、同步）都学会「这条消息可能
//! 属于多个会话」——那是给一张桥表，外加把 `episodes.session_id` 从一列
//! 变成一个集合。而 append-only 之下复制的代价只是行数：blob 是内容寻址
//! 的，**字节一份都不复制**，复制的只有指向它的引用行。
//!
//! # 为什么走 `write_txn`（正常写入路径），而不是 `INSERT ... SELECT`
//!
//! 一条 `INSERT ... SELECT` 写得又快又好看，但它绕开了 [`crate::txn`] 的
//! 纪律：每写一行业务数据必须同事务追加一行 `sync_log`。绕开的下场不是
//! 报错，是**别的设备永远看不见这条新会话** —— 同步的唯一事实序里没有它。
//! 所以这里逐行走 `WriteTxn` 的写方法，让「业务行 + sync_log」那对
//! 不可拆的动作由结构保证。
//!
//! # 读在事务外
//!
//! 取号事务必须短小纯写（纪律三）。所以整段历史（消息、工具轨迹、附件
//! 引用）在进事务**之前**读完、映射完，事务里只剩顺序 INSERT。

use cortex_core::Id;

use crate::error::{Result, StoreError};
use crate::model::{Actor, NewEpisode, NewEpisodeBlob, NewEpisodeToolCall, NewSessionEvent};
use crate::store::Store;

/// 一次分叉复制了多少东西。测试与日志都要这份账。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkOutcome {
    /// 新会话的 id（新 ULID，由这里生成）。
    pub session_id: String,
    pub episodes: usize,
    pub tool_calls: usize,
    /// 附件**引用**行数。字节本身内容寻址，一份都没复制。
    pub attachments: usize,
}

/// 一次全量读取的上限。
///
/// 不是分页 —— 分叉必须**整段**拿到，截断一半的分叉比失败更糟（用户以为
/// 带着全部历史，实际上前一半没了，而且没有任何报错）。这个数只是给
/// 「读整段」一个防爆栓：一条会话到十万条消息早就该出别的问题了。
const FORK_READ_MAX: i64 = 100_000;

impl Store {
    /// 分叉一条会话：复制 episodes（新 ULID）+ 工具轨迹 + 附件引用，
    /// 给新会话记一条改名事件。**旧会话不动。**
    ///
    /// `up_to_episode_id` 给了就截到那条消息（**含**它）——「从这里分叉」。
    /// 消息顺序按展示序（`occurred_at ASC, id ASC`）判定，与会话详情
    /// 一页页画出来的顺序同一份，所以「这里」就是用户眼里的那一条。
    ///
    /// # Errors
    ///
    /// - 源会话一条消息都没有：分叉一段空历史没有意义，也复制不出东西；
    /// - `up_to_episode_id` 不在这条会话里：多半是客户端把别的会话的
    ///   消息 id 传了过来，静默按全量处理会让「从这里分叉」悄悄变成
    ///   「整段分叉」，而用户看不出差别。
    ///
    /// 两者都是 [`StoreError::Invalid`]（「你这么问不对」，重试无益）。
    pub async fn fork_session(
        &self,
        source_session_id: &str,
        up_to_episode_id: Option<&str>,
        new_title: &str,
        device_id: &str,
    ) -> Result<ForkOutcome> {
        // ── 事务外：读完整段历史 ──
        let mut episodes = self
            .episodes_by_session(source_session_id, FORK_READ_MAX)
            .await?;
        // ⚠️ 读满上限 = 可能被截断了 —— **报错，不带着半段历史继续**。
        // 上面 FORK_READ_MAX 的文档自己写着「截断一半的分叉比失败更糟」，
        // 而第一版恰恰在命中上限时静默截断（评审抓到的自相矛盾）。
        // 十万条的会话现实里几乎不存在，真撞上时一句明确的拒绝
        // 好过一份看起来完整、实际缺了最新一段的分叉。
        if episodes.len() as i64 >= FORK_READ_MAX {
            return Err(StoreError::Invalid(format!(
                "会话 {source_session_id} 的消息数达到分叉上限（{FORK_READ_MAX} 条），\
                 无法保证完整复制 —— 这段历史太长，不支持分叉"
            )));
        }
        if episodes.is_empty() {
            return Err(StoreError::Invalid(format!(
                "会话 {source_session_id} 还没有任何消息，没有可分叉的历史"
            )));
        }
        if let Some(up_to) = up_to_episode_id {
            let Some(at) = episodes.iter().position(|e| e.id == up_to) else {
                return Err(StoreError::Invalid(format!(
                    "消息 {up_to} 不在会话 {source_session_id} 里 —— 「从这里分叉」找不到那个「这里」"
                )));
            };
            episodes.truncate(at + 1);
        }

        let ids: Vec<String> = episodes.iter().map(|e| e.id.clone()).collect();
        let tool_calls = self.episode_tool_calls_bulk(&ids).await?;
        let attachments = self.episode_attachments_bulk(&ids).await?;
        // 项目归属、执行环境、工作区绑定都跟着走：分叉的语义是「带着历史
        // **接着聊**」，而接着聊的下一轮该跑在原来的地方 —— 一个钉在本机
        // 目录上的会话，分叉出来若静默回落成 cloud（视图对无事件会话的
        // 默认），下一轮就跑进了另一个执行环境，界面上没有任何提示
        // （评审抓到的第三条）。视图已把悬挂归属收敛成 None，这里读到
        // 什么就是什么，不必再验项目存不存在
        let source_state = self.session_state(source_session_id).await?;
        let project_id = source_state.as_ref().and_then(|s| s.project_id.clone());

        // ── 事务外：映射成新行 ──
        //
        // 新 id 现在生成、旧→新的对照表现在建：工具轨迹与附件引用都要
        // 挂到**新**的 episode 上，进了事务再算就违反「短小纯写」。
        //
        // 同一毫秒里的几条消息在旧会话里靠 ULID 决出先后；新 ULID 的
        // 随机位不保证复刻那个先后。展示序的主键是 occurred_at（原样
        // 保留），同刻平局的次序漂移只在「一次工具调用连落两行」这种
        // 场景里理论可见，不值得为它引入自定义的单调 ULID 生成器。
        let new_session_id = Id::new().to_string();
        let mut new_episodes = Vec::with_capacity(episodes.len());
        let mut id_map = std::collections::HashMap::with_capacity(episodes.len());
        for e in &episodes {
            let new_id = Id::new();
            id_map.insert(e.id.clone(), new_id);
            new_episodes.push(NewEpisode {
                id: new_id,
                session_id: new_session_id.clone(),
                role: e.role,
                content: e.content.clone(),
                text: e.text.clone(),
                domain: e.domain.clone(),
                // 保留原来的 device_id：它记的是「这条当初从哪儿来」，
                // 分叉没有改变那件事
                device_id: e.device_id.clone(),
                occurred_at: e.occurred_at,
                models: e.models.clone().unwrap_or_default(),
            });
        }
        let new_tool_calls: Vec<NewEpisodeToolCall> = tool_calls
            .iter()
            .map(|t| NewEpisodeToolCall {
                id: Id::new(),
                episode_id: id_map[t.episode_id.as_str()],
                ordinal: t.ordinal,
                name: t.name.clone(),
                path: t.path.clone(),
                summary: t.summary.clone(),
                ok: t.ok,
                device_id: t.device_id.clone(),
                diff: t.diff.clone(),
            })
            .collect();
        let new_links: Vec<NewEpisodeBlob> = attachments
            .iter()
            .map(|a| NewEpisodeBlob {
                episode_id: id_map[a.episode_id.as_str()],
                // 引用同一份字节 —— 内容寻址正是为这一刻准备的
                blob_hash: a.blob_hash.clone(),
                kind: a.kind.clone(),
                filename: a.filename.clone(),
            })
            .collect();

        let mut events = vec![NewSessionEvent::rename(
            &new_session_id,
            new_title,
            Actor::User,
            device_id,
        )];
        if let Some(project) = &project_id {
            events.push(NewSessionEvent::move_to_project(
                &new_session_id,
                project,
                Actor::User,
                device_id,
            ));
        }
        if let Some(state) = &source_state {
            // 非默认的 runtime 才写事件：视图对无事件会话回落 Cloud，
            // 给 Cloud 会话补一条 set_runtime(Cloud) 是纯噪音
            if state.runtime != crate::model::SessionRuntime::Cloud {
                events.push(NewSessionEvent::set_runtime(
                    &new_session_id,
                    state.runtime,
                    Actor::User,
                    device_id,
                ));
            }
            if let Some(ws) = &state.workspace {
                events.push(NewSessionEvent::bind_workspace(
                    &new_session_id,
                    ws,
                    Actor::User,
                    device_id,
                ));
            }
            if let Some(cw) = &state.container_workspace {
                events.push(NewSessionEvent::set_container_workspace(
                    &new_session_id,
                    cw,
                    Actor::User,
                    device_id,
                ));
            }
        }

        // ── 事务内：只剩顺序写。episodes 先行 —— 工具轨迹与附件引用都有
        // 指向它的外键，顺序反了整个事务回滚 ──
        //
        // 整个分叉一个事务：拆开的话，别的设备会先同步到「有消息没标题」
        // 的半成品；更糟的是中途断掉留一段谁也不认识的孤儿历史。
        self.write_txn(async |t| {
            for e in &new_episodes {
                t.insert_episode(e).await?;
            }
            for c in &new_tool_calls {
                t.insert_episode_tool_call(c).await?;
            }
            for l in &new_links {
                t.link_episode_blob(l).await?;
            }
            for ev in &events {
                t.insert_session_event(ev).await?;
            }
            Ok(())
        })
        .await?;

        Ok(ForkOutcome {
            session_id: new_session_id,
            episodes: new_episodes.len(),
            tool_calls: new_tool_calls.len(),
            attachments: new_links.len(),
        })
    }
}
