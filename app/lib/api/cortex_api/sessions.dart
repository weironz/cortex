part of '../cortex_api.dart';

mixin CortexApiSessions {
  /// `GET /episodes/{id}`
  Future<Episode> episode(String id);

  /// `GET /sessions?include_archived=&project_id=`
  ///
  /// Archived sessions are omitted by default. Archiving is not deletion —
  /// episodes, attachments and extracted memory are all untouched — so the
  /// toggle is the only thing standing between the user and their history.
  ///
  /// [projectId] 省略 = 全部会话（含未分组的）。侧边栏走的正是这一条：
  /// 它要一次画出所有分组，按项目拉 N 次是 N 倍的往返，而且**拉不到未分组
  /// 的那一组** —— 那一组没有 id 可以传。
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  });

  /// `GET /sessions/search?q=` —— 在标题与消息正文里找。
  ///
  /// 搜索在**服务端**做：全部消息只在那张库里，客户端手上只有当前会话
  /// 这一页，拿它去搜等于「只能搜到刚才看过的东西」。
  ///
  /// 老服务端没有这条路由，答 404 —— 调用方按
  /// [CortexApiException.isUnsupported] 优雅降级成「这个部署搜不了」，
  /// 而不是把侧边栏整块换成错误提示。
  Future<List<SessionSearchHit>> searchSessions(
    String query, {
    bool includeArchived = false,
  });

  /// `PATCH /sessions/{id} {"project_id": …}` —— 移入 / 移出项目。
  ///
  /// [projectId] 为 null 时发的是显式的 `null`（移出，变未分组），
  /// 与 [updateSession] 里 workspace 的三态是同一套道理：
  /// 「字段不在」与「字段是 null」必须表达两件不同的事。
  Future<ChatSession> moveSessionToProject(String sessionId, String? projectId);

  /// `PATCH /sessions/{id} {"container_workspace": …}` —— 换云沙箱里的子目录。
  ///
  /// [name] 为 null 时发的是**显式的** `null`，意思是「回到卷根」；
  /// 与 [moveSessionToProject] 是同一套三态约定（「字段不在」= 别动）。
  ///
  /// 这是**云端**那一支的「选工作区」。桌面端那一支走
  /// [bindLocalWorkspace]，两者不共用一条路：本机路径是设备本地概念，
  /// 而这个名字是服务端记着的会话状态。
  ///
  /// 服务端校验名字的形状（`^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`，与库里那条
  /// CHECK 是同一份规则），拒绝时的措辞是写给用户看的 —— 原样显示，别替换。
  Future<ChatSession> setContainerWorkspace(String sessionId, String? name);

  /// `GET /sessions/{id}?limit=&before=` — overview plus **one page** of
  /// episodes, oldest first within the page.
  ///
  /// With no [before] the page is the *newest* one. To walk backwards, pass the
  /// previous response's `nextCursor`; stop when `hasMore` is false. [limit] is
  /// clamped server-side to 500.
  ///
  /// The cursor is opaque — constructing one locally earns a 400, which is the
  /// point: it is validated before it reaches SQL.
  Future<SessionDetail> sessionDetail(String id, {int? limit, String? before});

  /// `PATCH /sessions/{id}` — rename, archive, bind a workspace.
  ///
  /// The three fields are independent and all optional; only what is passed is
  /// changed, in one server-side write transaction.
  ///
  /// Workspace is tri-state and the two "no path" cases mean opposite things:
  ///
  /// | call | wire | meaning |
  /// |---|---|---|
  /// | neither arg | field absent | leave the binding alone |
  /// | `clearWorkspace: true` | `"workspace": null` | unbind, back to plain chat |
  /// | `workspace: "D:/x"` | `"workspace": "D:/x"` | bind |
  ///
  /// The daemon validates the path (absolute, exists, is a directory, not a
  /// filesystem root / system directory / the home directory itself, checked
  /// after symlink resolution) and its rejection message is written to be shown
  /// to the user verbatim — so callers should surface it, not replace it.
  ///
  /// Throws with [CortexApiException.isUnsupported] against a daemon that
  /// predates the route; callers keep the change local and flag it unsynced.
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    bool? pinned,
    String? workspace,
    bool clearWorkspace = false,
  });

  /// `POST /sessions/{id}/fork` — 分叉：带着历史开一条新会话，旧会话不动。
  ///
  /// [upToEpisodeId] 给了就截到那条消息（**含**它）——「从这里分叉」；
  /// 不给就整段复制。返回**新会话**，标题是「原标题（分叉）」。
  ///
  /// 复制发生在服务端一个写事务里（消息、工具轨迹、附件引用；附件字节
  /// 内容寻址，不复制）。客户端拿到返回值后直接切过去即可 —— 历史按
  /// 正常的 [sessionDetail] 拉取，不需要任何本地拼装。
  Future<ChatSession> forkSession(String id, {String? upToEpisodeId});

  /// `GET /sessions/export` —— 把这个账号的全部会话导成一份 NDJSON。
  ///
  /// **导的是会话，不是记忆**：facts / 时间轴在 Cormex 那个服务里，
  /// 这一侧一列都没有。界面上必须说清这一点 —— 一个写着「导出我的数据」
  /// 的按钮如果只给一半，用户会以为另一半不存在。
  ///
  /// 附件只有 hash 与文件名，字节走 `/blobs/{hash}` —— 内联的话一份导出
  /// 可能有几个 GB。
  Future<Uint8List> exportSessions();
}
