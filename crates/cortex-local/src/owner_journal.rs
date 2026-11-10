//! 有固定 owner 的会话 journal。旧设备级 outbox 不进入这条重放路径。
//!
//! 一个受保护实例锁覆盖单写者；每条正文与 ack 都先 sync_data 再更新内存。
//! 崩溃留下的不完整尾行可以截掉，完整但损坏的记录必须失败关闭。

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cortex_core::{CortexError, Result};
use cortex_proto::episodes::NewEpisodeRequest;
use serde::{Deserialize, Serialize};

use crate::local_owner::{OwnerKey, io_error, reject_link};

/// 上行附件引用没有 MIME 证据，只说明文件名，不能冒充已经重新读过文件内容。
pub(crate) fn history_text(request: &NewEpisodeRequest) -> String {
    if request.role != "user" || request.attachments.is_empty() {
        return request.text.clone();
    }
    let names = request
        .attachments
        .iter()
        .map(|attachment| {
            attachment
                .filename
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "附件-{}",
                        attachment.hash.chars().take(8).collect::<String>()
                    )
                })
        })
        .collect::<Vec<_>>();
    format!(
        "{}\n[当时附带的文件引用：{}]",
        request.text,
        names.join("、")
    )
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Entry {
    Episode {
        version: u32,
        owner: OwnerKey,
        request: Box<NewEpisodeRequest>,
    },
    Ack {
        owner: OwnerKey,
        episode_id: String,
    },
}

#[derive(Default)]
struct JournalState {
    poisoned: bool,
    episodes: Vec<(OwnerKey, NewEpisodeRequest)>,
    acknowledged: HashSet<(OwnerKey, String)>,
}

pub(crate) struct OwnerJournal {
    path: PathBuf,
    state: Mutex<JournalState>,
    pub(crate) replay: tokio::sync::Mutex<()>,
    #[cfg(test)]
    pub(crate) fail_append: Mutex<Option<(String, AppendFault)>>,
}

/// 在真实文件写入/同步边界注入故障；不在 save 入口绕开 append。
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum AppendFault {
    PartialWrite,
    SyncData,
}

impl OwnerJournal {
    pub(crate) fn open(root: &Path) -> Result<Self> {
        let path = root.join("episodes-v1.jsonl");
        reject_link(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(io_error)?;
        let data = std::fs::read(&path).map_err(io_error)?;
        let complete = data.iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
        let mut state = JournalState::default();
        for line in data[..complete]
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
        {
            let entry: Entry = serde_json::from_slice(line).map_err(|e| {
                CortexError::Config(format!("owner journal 已损坏，拒绝以空历史继续：{e}"))
            })?;
            match entry {
                Entry::Episode {
                    version: 1,
                    owner,
                    request,
                } => {
                    owner.validate()?;
                    if state
                        .episodes
                        .iter()
                        .any(|(o, e)| o == &owner && e.id == request.id)
                    {
                        return Err(CortexError::Config("owner journal 有重复正文".into()));
                    }
                    state.episodes.push((owner, *request));
                }
                Entry::Ack { owner, episode_id } => {
                    if !state
                        .episodes
                        .iter()
                        .any(|(o, e)| o == &owner && e.id == episode_id)
                    {
                        return Err(CortexError::Config(
                            "owner journal ack 没有匹配的 owner 正文".into(),
                        ));
                    }
                    state.acknowledged.insert((owner, episode_id));
                }
                _ => return Err(CortexError::Config("owner journal 版本不支持".into())),
            }
        }
        if complete != data.len() {
            file.set_len(complete as u64).map_err(io_error)?;
        }
        // reopen 对不确定的完整尾行也重新建立持久化边界，不能只验证 JSON。
        file.sync_data().map_err(io_error)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
            replay: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            fail_append: Mutex::new(None),
        })
    }

    fn append(&self, entry: &Entry) -> Result<()> {
        reject_link(&self.path)?;
        let mut bytes =
            serde_json::to_vec(entry).map_err(|e| CortexError::Config(e.to_string()))?;
        bytes.push(b'\n');
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(io_error)?;
        #[cfg(test)]
        let fault = {
            let configured = self.fail_append.lock().expect("append 故障锁可用");
            configured.as_ref().and_then(|(role, fault)| match entry {
                Entry::Episode { request, .. } if &request.role == role => Some(*fault),
                Entry::Ack { .. } if role == "ack" => Some(*fault),
                _ => None,
            })
        };
        #[cfg(test)]
        if matches!(fault, Some(AppendFault::PartialWrite)) {
            file.write_all(&bytes[..bytes.len() / 2])
                .map_err(io_error)?;
            return Err(io_error(std::io::Error::other(
                "实际 append 部分写入后故障",
            )));
        }
        file.write_all(&bytes).map_err(io_error)?;
        #[cfg(test)]
        if matches!(fault, Some(AppendFault::SyncData)) {
            return Err(io_error(std::io::Error::other(
                "实际 append 写完，在 sync_data 边界注入不确定结果",
            )));
        }
        file.sync_data().map_err(io_error)
    }

    pub(crate) fn save(&self, owner: &OwnerKey, request: &NewEpisodeRequest) -> Result<()> {
        let mut state = self.state.lock().expect("journal 锁可用");
        if state.poisoned {
            return Err(CortexError::Config(
                "本机会话未保存：journal 已 poison，必须在独占锁下重新打开并验证".into(),
            ));
        }
        if let Some((_, previous)) = state
            .episodes
            .iter()
            .find(|(o, e)| o == owner && e.id == request.id)
        {
            if serde_json::to_value(previous).map_err(|e| CortexError::Config(e.to_string()))?
                == serde_json::to_value(request).map_err(|e| CortexError::Config(e.to_string()))?
            {
                return Ok(());
            }
            return Err(CortexError::Invalid(
                "同 owner、同 episode id 的正文冲突，拒绝覆盖或丢弃内容".into(),
            ));
        }
        if let Err(error) = self.append(&Entry::Episode {
            version: 1,
            owner: owner.clone(),
            request: Box::new(request.clone()),
        }) {
            state.poisoned = true;
            return Err(error);
        }
        state.episodes.push((owner.clone(), request.clone()));
        Ok(())
    }

    pub(crate) fn acknowledge(&self, owner: &OwnerKey, id: &str) -> Result<()> {
        let mut state = self.state.lock().expect("journal 锁可用");
        if state.poisoned {
            return Err(CortexError::Config(
                "本机会话未保存：journal 已 poison，禁止继续 ack".into(),
            ));
        }
        if !state.episodes.iter().any(|(o, e)| o == owner && e.id == id) {
            return Err(CortexError::Invalid(
                "ack 没有匹配的已持久正文及 owner".into(),
            ));
        }
        let key = (owner.clone(), id.to_owned());
        if state.acknowledged.contains(&key) {
            return Ok(());
        }
        if let Err(error) = self.append(&Entry::Ack {
            owner: owner.clone(),
            episode_id: id.into(),
        }) {
            state.poisoned = true;
            return Err(error);
        }
        state.acknowledged.insert(key);
        Ok(())
    }

    pub(crate) fn pending(&self, owner: &OwnerKey) -> Vec<NewEpisodeRequest> {
        if matches!(owner, OwnerKey::Local { .. }) {
            return Vec::new();
        }
        let state = self.state.lock().expect("journal 锁可用");
        state
            .episodes
            .iter()
            .filter(|(o, e)| o == owner && !state.acknowledged.contains(&(o.clone(), e.id.clone())))
            .map(|(_, e)| e.clone())
            .collect()
    }

    pub(crate) fn episodes(&self, owner: &OwnerKey, session: &str) -> Vec<NewEpisodeRequest> {
        self.state
            .lock()
            .expect("journal 锁可用")
            .episodes
            .iter()
            .filter(|(o, e)| o == owner && e.session_id == session)
            .map(|(_, e)| e.clone())
            .collect()
    }

    pub(crate) fn history(&self, owner: &OwnerKey, session: &str) -> crate::remote::RemoteSession {
        crate::remote::RemoteSession {
            episodes: self
                .episodes(owner, session)
                .into_iter()
                .map(|e| crate::remote::RemoteHistoryEpisode {
                    text: history_text(&e),
                    id: e.id,
                    is_user: e.role == "user",
                    occurred_at: e
                        .occurred_at
                        .as_deref()
                        .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                        .map(|date| date.with_timezone(&chrono::Utc)),
                })
                .collect(),
            container_workspace: None,
        }
    }

    pub(crate) fn session_json(
        &self,
        owner: &OwnerKey,
        session: &str,
    ) -> Result<serde_json::Value> {
        let episodes = self.episodes(owner, session);
        if episodes.is_empty() {
            return Err(CortexError::NotFound {
                kind: "session",
                id: session.into(),
            });
        }
        let mut result = self.summary(session, &episodes);
        result["has_more"] = serde_json::json!(false);
        result["next_cursor"] = serde_json::Value::Null;
        result["episodes"] =
            serde_json::to_value(episodes).map_err(|e| CortexError::Config(e.to_string()))?;
        Ok(result)
    }

    fn summary(&self, session: &str, episodes: &[NewEpisodeRequest]) -> serde_json::Value {
        serde_json::json!({"id":session,"title":episodes.first().map(|e| e.text.chars().take(60).collect::<String>()).unwrap_or_default(),"episode_count":episodes.len(),"message_count":episodes.len(),"title_is_custom":false,"preview":episodes.last().map(|e|e.text.chars().take(120).collect::<String>()),"archived":false,"pinned":false,"created_at":episodes.first().and_then(|e|e.occurred_at.clone()),"updated_at":episodes.last().and_then(|e|e.occurred_at.clone()),"workspace":null,"runtime":"local","persistence":"local_only"})
    }

    pub(crate) fn search_json(&self, owner: &OwnerKey, query: &str) -> serde_json::Value {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return serde_json::json!({"hits":[]});
        }
        let sessions = self.sessions_json(owner);
        let hits: Vec<_> = sessions["sessions"].as_array().expect("session 列表").iter().filter_map(|session| {
            let id = session["id"].as_str()?;
            let episodes = self.episodes(owner, id);
            let matched: Vec<_> = episodes.iter().filter(|e|e.text.to_lowercase().contains(&query)).collect();
            let title_match = session["title"].as_str().unwrap_or_default().to_lowercase().contains(&query);
            if matched.is_empty() && !title_match { return None; }
            Some(serde_json::json!({"session_id":id,"title":session["title"],"archived":false,"title_match":title_match,"hit_count":matched.len(),"excerpt":matched.last().map(|e|e.text.chars().take(160).collect::<String>())}))
        }).collect();
        serde_json::json!({"hits":hits})
    }

    pub(crate) fn sessions_json(&self, owner: &OwnerKey) -> serde_json::Value {
        let mut ids: Vec<String> = self
            .state
            .lock()
            .expect("journal 锁可用")
            .episodes
            .iter()
            .filter(|(o, _)| o == owner)
            .map(|(_, e)| e.session_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        serde_json::json!({"sessions":ids.iter().map(|id|self.summary(id,&self.episodes(owner,id))).collect::<Vec<_>>()})
    }
}
