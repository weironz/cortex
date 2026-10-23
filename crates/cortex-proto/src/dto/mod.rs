//! HTTP 数据传输对象。
//!
//! 这是 CLI、Flutter 桌面/Web 三端共用的**唯一契约**。
//! 改这里等于改三个客户端，慎重。

mod attachments;
mod auth;
mod blobs;
mod chat;
mod confirmations;
mod errors;
mod health;
mod projects;
mod sessions;
mod sync;

pub use attachments::*;
pub use auth::*;
pub use blobs::*;
pub use chat::*;
pub use confirmations::*;
pub use errors::*;
pub use health::*;
pub use projects::*;
pub use sessions::*;
pub use sync::*;

#[cfg(test)]
mod tests {

    /// 终帧要把这一轮**画出来的图**一起带走。
    ///
    /// 不带的话，客户端在流式那条路上从来不知道这一轮往 episode 上挂了什么
    /// blob —— 用户看到模型说「画好啦」而屏幕上一张图都没有，要重新拉一次
    /// 这条会话才出现。2026-08-23 在桌面端实测到的就是这个。
    ///
    /// 同时盯住**老客户端/老服务端**那两个方向：字段缺席要解成空表，
    /// 空表要不出现在线上（否则老客户端会多收一个它不认识的键）。
    #[test]
    fn done_carries_what_the_turn_drew() {
        let ev = super::ChatEvent::Done {
            episode_id: "ep1".into(),
            models: vec!["qwen-image".into()],
            attachments: vec![super::AttachmentRef {
                hash: "a".repeat(64),
                kind: Some("image".into()),
                filename: Some("生成的图.png".into()),
            }],
        };
        let json = serde_json::to_string(&ev).expect("终帧可序列化");
        assert!(
            json.contains("生成的图.png"),
            "画出来的图必须出现在终帧里，否则这一轮的界面上一张图都没有：{json}"
        );

        let old: super::ChatEvent = serde_json::from_str(r#"{"type":"done","episode_id":"ep1"}"#)
            .expect("老服务端不发 attachments/models，仍要解得出来");
        match old {
            super::ChatEvent::Done { attachments, .. } => assert!(
                attachments.is_empty(),
                "字段缺席 = 这一轮没画东西，不是解析失败"
            ),
            other => panic!("解成了别的事件：{other:?}"),
        }

        let empty = serde_json::to_string(&super::ChatEvent::Done {
            episode_id: "ep1".into(),
            models: Vec::new(),
            attachments: Vec::new(),
        })
        .expect("可序列化");
        assert!(
            !empty.contains("attachments"),
            "空表不该上线 —— 老客户端会多收一个它不认识的键：{empty}"
        );
    }

    /// 用量事件：报了什么就带什么，**没报的键不要上线**。
    ///
    /// 线上少一个键 = 客户端解成「不知道」= 那一项不画。带上一个 0 就变成
    /// 「缓存命中 0%」，而那是另一句话 —— 用户据此会以为缓存坏了。
    #[test]
    fn usage_only_carries_what_the_provider_reported() {
        let ev = super::ChatEvent::Usage {
            model: Some("qwen-flash".into()),
            input_tokens: Some(1200),
            output_tokens: Some(240),
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            generation_ms: 320,
            subagent: None,
        };
        let json = serde_json::to_string(&ev).expect("用量事件可序列化");
        assert!(json.contains(r#""type":"usage""#), "判别式写错了：{json}");
        assert!(
            !json.contains("cache_read_input_tokens") && !json.contains("subagent"),
            "没报的字段不该上线 —— 带上一个 0 会被读成「供应商报了零缓存」，而它其实什么都没说：{json}"
        );

        // **Dart 客户端**看到不认识的 type 会落到 `ChatUnknownEvent`，安静
        // 忽略；这里守的是反方向：新客户端读一条**字段最少**的用量事件解不炸。
        //
        // ⚠️ Rust CLI 没有这层兜底（`client.rs` 把解码失败当错误、整轮中断），
        // 所以**命令行客户端与 agentd 必须同一版本线一起换** —— 这是加这个
        // 变体时的一笔已知代价，不是这里守着的判据
        let minimal: super::ChatEvent =
            serde_json::from_str(r#"{"type":"usage","generation_ms":0}"#)
                .expect("字段最少也要解得出来");
        match minimal {
            super::ChatEvent::Usage { input_tokens, .. } => {
                assert_eq!(input_tokens, None, "缺席 = 供应商没报，不是 0");
            }
            other => panic!("解成了别的事件：{other:?}"),
        }

        // **键名逐个钉住。** Dart 那边按这些蛇形键读
        // （`tokenCount(json['input_tokens'])` 之类）。改名不会让任何一侧
        // 编译失败 —— Dart 读到的是 null，界面整项不画，看起来只是「这家
        // 供应商没报数」。两个客户端各自绿着，而这条链早就断了
        let full = serde_json::to_string(&super::ChatEvent::Usage {
            model: Some("m".into()),
            input_tokens: Some(1),
            output_tokens: Some(2),
            cache_read_input_tokens: Some(3),
            cache_write_input_tokens: Some(4),
            generation_ms: 5,
            subagent: Some(super::SubagentTag {
                index: 1,
                task: "t".into(),
            }),
        })
        .expect("用量事件可序列化");
        for key in [
            r#""model""#,
            r#""input_tokens""#,
            r#""output_tokens""#,
            r#""cache_read_input_tokens""#,
            r#""cache_write_input_tokens""#,
            r#""generation_ms""#,
            r#""subagent""#,
        ] {
            assert!(
                full.contains(key),
                "线上少了 {key} 这个键 —— Dart 读它，读到 null 就整项不画：{full}"
            );
        }
    }

    /// **三态的每个字段都要用 `explicit_option`，一个都不能漏。**
    ///
    /// serde 默认把「字段为 null」与「字段没出现」都解成 `None`，而这三个字段
    /// 靠这个区别表达「清空」与「别动」。漏掉那个属性不会有编译错误 ——
    /// 症状是 `{"x": null}` 被当成「什么都没提」，于是整条 PATCH 报
    /// 「请求体里没有任何要改的字段」。`container_workspace` 第一版就漏了，
    /// 而它是在真服务端上打出 400 才发现的。
    ///
    /// 逐字段断言而不是只测一个：漏的那个永远是新加的那个。
    #[test]
    fn every_tri_state_field_tells_null_apart_from_absent() {
        let absent: SessionPatch = serde_json::from_str("{}").expect("空对象合法");
        assert!(absent.workspace.is_none() && absent.project_id.is_none());
        assert!(
            absent.container_workspace.is_none(),
            "字段没出现应当是外层 None"
        );

        let nulled: SessionPatch = serde_json::from_str(
            r#"{"workspace":null,"project_id":null,"container_workspace":null}"#,
        )
        .expect("显式 null 合法");
        assert_eq!(
            nulled.workspace,
            Some(None),
            "workspace: null 是「解绑」，不是「别动」"
        );
        assert_eq!(
            nulled.project_id,
            Some(None),
            "project_id: null 是「移出项目」"
        );
        assert_eq!(
            nulled.container_workspace,
            Some(None),
            concat!(
                "container_workspace: null 是「回到卷根」—— 解成 None 会让这条 PATCH ",
                "报「没有任何要改的字段」",
            )
        );
    }

    use super::*;

    /// `workspace` 的三态必须分得清楚。
    ///
    /// serde 默认会把「字段没出现」与「字段是 null」都解成 `None`，
    /// 而这两者在这里语义相反：前者是「别动工作区」，后者是「解绑」。
    /// 混掉的话，客户端每次改个标题都会顺手把工作区解绑掉 ——
    /// 而那个 bug 只在「用户既绑了工作区又改了名」时才现形。
    #[test]
    fn workspace_distinguishes_absent_from_null() {
        let absent: SessionPatch = serde_json::from_str(r#"{"title":"改个名"}"#).expect("应能解析");
        assert!(absent.workspace.is_none(), "字段没出现应当是「不动工作区」");

        let unbind: SessionPatch = serde_json::from_str(r#"{"workspace":null}"#).expect("应能解析");
        assert_eq!(
            unbind.workspace,
            Some(None),
            "显式的 null 应当是「解绑」，不能与「字段没出现」混为一谈"
        );

        let bind: SessionPatch =
            serde_json::from_str(r#"{"workspace":"D:/codes/x"}"#).expect("应能解析");
        assert_eq!(bind.workspace, Some(Some("D:/codes/x".into())));
    }

    /// `project_id` 的三态与 `workspace` 同款，混掉的后果同样是静默的：
    /// 用户每改一次标题，会话就被顺手踢出项目。
    #[test]
    fn project_id_distinguishes_absent_from_null() {
        let absent: SessionPatch = serde_json::from_str(r#"{"title":"改个名"}"#).expect("应能解析");
        assert!(
            absent.project_id.is_none(),
            "字段没出现应当是「不动归属」—— 解成「移出项目」的话，改标题会把会话踢出项目"
        );

        let out: SessionPatch = serde_json::from_str(r#"{"project_id":null}"#).expect("应能解析");
        assert_eq!(
            out.project_id,
            Some(None),
            "显式的 null 应当是「移出项目」，不能与「字段没出现」混为一谈"
        );

        let into: SessionPatch =
            serde_json::from_str(r#"{"project_id":"01JPROJECT0000000000000001"}"#)
                .expect("应能解析");
        assert_eq!(
            into.project_id,
            Some(Some("01JPROJECT0000000000000001".into()))
        );
    }

    #[test]
    fn an_empty_patch_parses_and_changes_nothing() {
        // 空 body 不该解析失败 —— 失败会让「服务端不认识我发的字段」
        // 与「我什么都没发」变成同一个 400，查起来全无线索
        let p: SessionPatch = serde_json::from_str("{}").expect("空对象应能解析");
        assert!(
            p.title.is_none()
                && p.archived.is_none()
                && p.workspace.is_none()
                && p.project_id.is_none()
        );
    }

    #[test]
    fn list_query_without_project_means_every_session() {
        let q: ListSessionsQuery = serde_json::from_str("{}").expect("缺参数应当走默认值");
        assert!(
            q.project_id.is_none(),
            "不传 project_id 必须是「全部会话」而不是「未分组」—— \
             默认值一旦是后者，老客户端会突然只看得见未分组的会话"
        );
    }

    #[test]
    fn list_query_defaults_to_hiding_archived() {
        // 用 JSON 代替查询串：起作用的是 `#[serde(default)]`，
        // 与具体的 Deserializer 无关，而 serde_urlencoded 不是本 crate 的依赖
        let q: ListSessionsQuery = serde_json::from_str("{}").expect("缺参数应当走默认值");
        assert!(
            !q.include_archived,
            "不传参数时必须隐藏归档会话 —— 归档的产品语义就是从列表消失"
        );
    }
}

#[cfg(test)]
mod permission_mode_tests {
    use super::PermissionMode;
    use std::str::FromStr as _;

    /// **认不出的档要报错，不能回落成默认。**
    ///
    /// 回落的后果取决于默认是哪一档，而两个方向都很难看：要么用户以为开了
    /// 免确认却每条都被问，要么反过来。空串同样拒 —— 这是本仓库数到第六次的
    /// 「空串顶掉默认值」。
    #[test]
    fn an_unknown_mode_is_refused_rather_than_defaulted() {
        for bad in ["", "  ", "yes", "bypas", "ASK", "accept_edit"] {
            assert!(
                PermissionMode::from_str(bad).is_err(),
                "{bad:?} 应当被拒，而不是悄悄变成默认档"
            );
        }
    }

    /// 连字符与下划线两种写法都收：命令行里连字符更顺手，而线上格式是
    /// snake_case（serde）。拒绝任一种，都会让人对着一个「看起来完全正确」
    /// 的参数发呆。
    #[test]
    fn both_spellings_parse_to_the_same_mode() {
        assert_eq!(
            PermissionMode::from_str("accept-edits").unwrap(),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            PermissionMode::from_str("accept_edits").unwrap(),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            PermissionMode::from_str(" ask ").unwrap(),
            PermissionMode::Ask
        );
    }

    /// `as_str` 与 serde 的 snake_case 必须是**同一批字面量**。
    ///
    /// 两处各写一份的话，`--help` 里印的名字与线上 JSON 里的值会漂开，
    /// 而那种不一致没有任何东西会报错 —— 用户照着帮助敲，服务端说认不出。
    #[test]
    fn the_wire_format_and_the_cli_spelling_are_the_same_words() {
        for m in [
            PermissionMode::Ask,
            PermissionMode::AcceptEdits,
            PermissionMode::Bypass,
        ] {
            let wire = serde_json::to_string(&m).expect("序列化");
            let wire = wire.trim_matches('"');
            assert_eq!(
                wire,
                m.as_str(),
                "线上格式与命令行字面量漂开了：serde 给 {wire:?}，as_str 给 {:?}",
                m.as_str()
            );
            assert_eq!(PermissionMode::from_str(m.as_str()).unwrap(), m);
        }
    }
}
