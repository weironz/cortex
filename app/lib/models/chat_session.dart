import 'json.dart';
import 'workspace.dart';

/// A conversation thread (`GET /sessions`).
class ChatSession {
  const ChatSession({
    required this.id,
    required this.title,
    this.createdAt,
    this.updatedAt,
    this.titleIsCustom = false,
    this.messageCount = 0,
    this.preview,
    this.workspace,
    this.containerWorkspace,
    this.archived = false,
    this.projectId,
    this.isLocalDraft = false,
    this.hasLocalOverrides = false,
  });

  final String id;
  final String title;
  final DateTime? createdAt;
  final DateTime? updatedAt;

  /// [title] was set by the user rather than derived from the first message.
  ///
  /// Decides what the rename box is pre-filled with: a derived title is a
  /// placeholder, and pre-filling it would quietly promote it to a real name
  /// the moment the user pressed OK without editing anything.
  final bool titleIsCustom;

  /// Episode count. Drives the "这个会话有 N 条消息" hint and the decision to
  /// replay it in windows rather than all at once.
  final int messageCount;

  /// Last message excerpt, for the sidebar.
  final String? preview;

  /// The directory this session's agent may touch. Null = plain chat, no file
  /// tools. See [Workspace] for whose filesystem this is.
  final Workspace? workspace;

  /// 云沙箱那个卷里的子目录名。null = 根就是卷根（默认）。
  ///
  /// **与 [workspace] 不是一回事**：那个是桌面端的本机绝对路径，是设备本地
  /// 概念；这个是容器里的一段目录名，只对云端会话有意义。两个字段在同一个
  /// 会话上同时有值是正常的 —— 一个会话可以既在这台机器上绑过目录，
  /// 又在云端有过自己的子目录，取哪一个由**这一轮跑在哪儿**决定。
  ///
  /// 名字可以被多个会话共用：按会话分的话，「昨天让你生成的那份报告呢」
  /// 会得到一个空目录。
  final String? containerWorkspace;

  /// Hidden from the list unless "显示已归档" is on. Never deleted — the store
  /// is append-only, so "归档" is the only honest verb.
  final bool archived;

  /// 所属项目，null = 未分组。
  ///
  /// 指向一个**已经不存在**的项目也当作未分组处理（见
  /// `groupSessionsByProject`）：删项目之后、下一次 `GET /sessions` 之前，
  /// 手上这份数据正是这个形态。
  final String? projectId;

  /// True for a session created client-side that the server has not yet
  /// acknowledged. The first `POST /chat` with this id materialises it, so we
  /// keep it in the list optimistically and just mark it.
  final bool isLocalDraft;

  /// Title / archive / workspace were changed locally but the daemon could not
  /// persist them (`PATCH /sessions/{id}` does not exist yet). Surfaced in the
  /// sidebar so nobody believes a rename survived a restart.
  final bool hasLocalOverrides;

  /// True when the daemon returned fewer episodes than [messageCount] says
  /// exist. `GET /sessions/{id}` caps at 500 **oldest-first**, with no cursor,
  /// so a long session shows its beginning and silently omits its end. The
  /// transcript header says so rather than pretending it is complete.
  bool truncatedAt(int loaded) => messageCount > loaded;

  ChatSession copyWith({
    String? title,
    bool? titleIsCustom,
    DateTime? createdAt,
    DateTime? updatedAt,
    int? messageCount,
    String? preview,
    Object? workspace = _sentinel,
    // 同 projectId：`containerWorkspace: null` 是「回到卷根」，
    // 与「这次不改」是两件事
    Object? containerWorkspace = _sentinel,
    bool? archived,
    // 哨兵而不是可空参数：`projectId: null` 的意思是「移出项目」，
    // 与「这次不改分组」是两件事，而后者才是不传时该发生的
    Object? projectId = _sentinel,
    bool? isLocalDraft,
    bool? hasLocalOverrides,
  }) => ChatSession(
    id: id,
    title: title ?? this.title,
    titleIsCustom: titleIsCustom ?? this.titleIsCustom,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    messageCount: messageCount ?? this.messageCount,
    preview: preview ?? this.preview,
    workspace: workspace == _sentinel
        ? this.workspace
        : workspace as Workspace?,
    containerWorkspace: containerWorkspace == _sentinel
        ? this.containerWorkspace
        : containerWorkspace as String?,
    archived: archived ?? this.archived,
    projectId: projectId == _sentinel ? this.projectId : projectId as String?,
    isLocalDraft: isLocalDraft ?? this.isLocalDraft,
    hasLocalOverrides: hasLocalOverrides ?? this.hasLocalOverrides,
  );

  static const Object _sentinel = Object();

  /// Every field is read leniently: a daemon built before the session-lifecycle
  /// contract landed simply omits `workspace` / `archived` / `title_is_custom`,
  /// and the session then reads as "未绑定、未归档、标题是派生的" — which is
  /// exactly what such a daemon means.
  factory ChatSession.fromJson(Map<String, dynamic> json) {
    final root = asStringOrNull(json['workspace']);
    return ChatSession(
      id: asString(json['id']),
      title: asString(json['title'], '未命名会话'),
      titleIsCustom: json['title_is_custom'] == true,
      createdAt: asDateOrNull(json['created_at']),
      updatedAt: asDateOrNull(json['updated_at']),
      messageCount: asInt(json['message_count']),
      preview: asStringOrNull(json['preview']),
      workspace: root == null ? null : Workspace(root: root),
      // 老服务端不发 —— 读成 null，也就是「根就是卷根」，
      // 而那正是一个还没有这个能力的部署里每条会话的真实状态
      containerWorkspace: asStringOrNull(json['container_workspace']),
      archived: json['archived'] == true,
      // 老服务端不发这个字段，读成 null —— 也就是「未分组」，
      // 而那正是一个没有项目功能的部署里每条会话的真实状态
      projectId: asStringOrNull(json['project_id']),
    );
  }
}
