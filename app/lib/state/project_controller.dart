import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../models/chat_session.dart';
import '../models/project.dart';
import 'app_providers.dart';

class ProjectState {
  const ProjectState({
    this.projects = const [],
    this.loading = true,
    this.error,
    this.unsupported = false,
    this.selectedId,
    this.collapsed = const {},
  });

  final List<Project> projects;
  final bool loading;
  final String? error;

  /// 这个后端**没有** `/projects`。
  ///
  /// 与 [error] 分得很开，因为两者要求的反应相反：错误该显示、该给重试；
  /// 「没有这个功能」只该让分组这层界面安静地不存在。把 404 当错误画出来，
  /// 用户看到的是一条自己无论如何都消不掉的红字 —— 而会话列表本身好好的。
  final bool unsupported;

  /// 侧边栏里「当前所在」的项目，null = 未分组那一组。
  ///
  /// 只影响新建会话默认落在哪里与分组标题的高亮，不过滤列表：侧边栏一次
  /// 画出全部分组，选中某个项目不该让别的会话消失。
  final String? selectedId;

  /// 折叠起来的项目 id。
  ///
  /// 记的是**折叠**而不是展开，这样新出现的项目（自己刚建的、别的设备同步
  /// 过来的）默认是展开的 —— 反过来存展开集合的话，新建一个项目会得到一个
  /// 收起来的空壳，而用户刚刚才说他要用它。
  final Set<String> collapsed;

  bool get hasProjects => projects.isNotEmpty;

  /// 分组这层界面是否值得画出来。
  bool get showGrouping => hasProjects && !unsupported;

  bool isCollapsed(String id) => collapsed.contains(id);

  Project? byId(String? id) {
    if (id == null) return null;
    for (final p in projects) {
      if (p.id == id) return p;
    }
    return null;
  }

  ProjectState copyWith({
    List<Project>? projects,
    bool? loading,
    Object? error = _sentinel,
    bool? unsupported,
    Object? selectedId = _sentinel,
    Set<String>? collapsed,
  }) => ProjectState(
    projects: projects ?? this.projects,
    loading: loading ?? this.loading,
    error: error == _sentinel ? this.error : error as String?,
    unsupported: unsupported ?? this.unsupported,
    selectedId: selectedId == _sentinel
        ? this.selectedId
        : selectedId as String?,
    collapsed: collapsed ?? this.collapsed,
  );

  static const Object _sentinel = Object();
}

/// 项目列表 + 增删改。会话本身仍归 `ChatController` 管。
class ProjectController extends Notifier<ProjectState> {
  /// 作废序号：换后端时 +1，让在飞的请求的结果被丢掉。
  ///
  /// 见 `ChatController._stale` 的长注释 —— 换后端会 `dispose` 掉旧的
  /// `HttpCortexApi`（`_client.close()`），在飞的请求当场被掐断并抛异常，
  /// 而那个异常落地的时间**晚于**清空。不作废的话它会把「拉不到项目列表」
  /// 写回一个刚被清干净的界面，且用户没有做错任何事。
  int _requestSeq = 0;

  bool _stale(int seq) => seq != _requestSeq || !ref.mounted;

  CortexApi get _api => ref.read(cortexApiProvider);

  @override
  ProjectState build() {
    ref.listen(cortexApiProvider, (_, _) {
      _requestSeq++;
      // 折叠状态是**视图偏好**，不是后端数据，跟着一起清掉的话
      // 换一次后端所有人的分组都会重新展开一遍
      state = ProjectState(collapsed: state.collapsed);
      unawaited(load());
    });
    Future.microtask(load);
    return const ProjectState();
  }

  Future<void> load() async {
    if (!ref.mounted) return;
    final seq = _requestSeq;
    state = state.copyWith(loading: true, error: null);
    try {
      final projects = await _api.projects();
      if (_stale(seq)) return;
      state = state.copyWith(
        projects: projects,
        loading: false,
        unsupported: false,
        // 选中的项目可能已经被别的设备删了 —— 留着它会让「在这个项目里
        // 新建会话」把会话丢进一个不存在的 id
        selectedId: projects.any((p) => p.id == state.selectedId)
            ? state.selectedId
            : null,
      );
    } on CortexApiException catch (e) {
      if (_stale(seq)) return;
      state = e.isUnsupported
          ? state.copyWith(
              projects: const [],
              loading: false,
              unsupported: true,
              error: null,
              selectedId: null,
            )
          : state.copyWith(loading: false, error: e.message);
    } on Object catch (e) {
      if (_stale(seq)) return;
      state = state.copyWith(loading: false, error: '$e');
    }
  }

  /// 新建。返回建好的项目，让调用方能立刻选中它。
  ///
  /// 失败**抛给调用方**：用户刚点了确定，静默失败会让他以为建好了，
  /// 然后去侧边栏里找一个不存在的东西。
  Future<Project?> create(String name) async {
    final trimmed = name.trim();
    if (trimmed.isEmpty) return null;
    final seq = _requestSeq;
    final project = await _guard(() => _api.createProject(trimmed));
    if (project == null || _stale(seq)) return null;
    state = state.copyWith(
      projects: [...state.projects, project],
      selectedId: project.id,
    );
    return project;
  }

  Future<void> rename(String id, String name) async {
    final trimmed = name.trim();
    if (trimmed.isEmpty) return;
    final seq = _requestSeq;
    final updated = await _guard(
      () => _api.renameProject(id, trimmed),
      featureProbe: false,
    );
    if (updated == null || _stale(seq)) return;
    state = state.copyWith(
      projects: [
        for (final p in state.projects)
          if (p.id == id) updated else p,
      ],
    );
  }

  /// 删除分组本身。会话不动 —— 见 [CortexApi.deleteProject]。
  ///
  /// 界面上那些会话会立刻落进「未分组」，不必等下一次 `GET /sessions`：
  /// 分组是按「projectId 指得到一个还在的项目」算的，项目没了就归未分组
  /// （见 [groupSessionsByProject]）。
  Future<void> remove(String id) async {
    final seq = _requestSeq;
    try {
      await _guard(() async {
        await _api.deleteProject(id);
        return true;
      }, featureProbe: false);
    } on CortexApiException catch (e) {
      // 「它已经不在了」= 想要的结果已经达成。往下走，把本地那份也去掉。
      // 报错反而是错的：用户要的是「没有这个分组」，而现在正是如此。
      if (!e.isMissing) rethrow;
    }
    if (_stale(seq)) return;
    state = state.copyWith(
      projects: state.projects.where((p) => p.id != id).toList(growable: false),
      selectedId: state.selectedId == id ? null : state.selectedId,
      collapsed: state.collapsed.where((c) => c != id).toSet(),
    );
  }

  /// 三个写操作共用的失败处理。
  ///
  /// 端点不存在（老服务端）与「这次没成功」是两件事：前者要把分组这层界面
  /// 收起来，否则用户会对着一个每次点都报错的按钮反复试。两种都往上抛 ——
  /// 他刚刚才点了确定，界面必须说出发生了什么。
  ///
  /// # 404 在这里有两个意思，[featureProbe] 就是用来分开它们的
  ///
  /// [CortexApiException.isUnsupported] 把 404 / 405 / 501 一起读成
  /// 「这个后端没有这个端点」。对**集合**端点（`GET`/`POST /projects`）
  /// 这是对的：整条路由不存在时就是 404。
  ///
  /// 但对**单个项目**的端点（`PATCH`/`DELETE /projects/{id}`），404 说的是
  /// 「这个项目不在了」—— 两台设备各删一次就会撞上，而现在桌面端与 CLI
  /// 共用同一批会话，这不再是个假想的场景。按「不支持」处理的后果是
  /// 整条分组界面当场消失、且这个进程内再也回不来，而用户只是删了两次。
  Future<T?> _guard<T>(
    Future<T> Function() call, {
    bool featureProbe = true,
  }) async {
    try {
      return await call();
    } on CortexApiException catch (e) {
      if (ref.mounted && e.isUnsupported && (featureProbe || !e.isMissing)) {
        state = state.copyWith(
          projects: const [],
          unsupported: true,
          loading: false,
          error: null,
          selectedId: null,
        );
      }
      rethrow;
    }
  }

  void select(String? id) {
    if (state.selectedId == id) return;
    state = state.copyWith(selectedId: id);
  }

  void toggleCollapsed(String id) {
    final next = {...state.collapsed};
    if (!next.remove(id)) next.add(id);
    state = state.copyWith(collapsed: next);
  }

  /// 展开某一组。在收起的分组里新建会话时必须调用 —— 否则那个会话建出来了、
  /// 也选中了，但侧边栏里一行都没多，看起来像是按钮坏了。
  void expand(String id) {
    if (!state.collapsed.contains(id)) return;
    state = state.copyWith(
      collapsed: state.collapsed.where((c) => c != id).toSet(),
    );
  }
}

final projectControllerProvider =
    NotifierProvider<ProjectController, ProjectState>(ProjectController.new);

/// 侧边栏里的一组：一个项目（[project] 为 null 表示「未分组」）与它的会话。
class SessionGroup {
  const SessionGroup({required this.project, required this.sessions});

  final Project? project;
  final List<ChatSession> sessions;

  String? get projectId => project?.id;
  bool get isUngrouped => project == null;
}

/// 把会话按项目切成若干组，项目顺序照 [projects] 给的顺序，未分组永远在最后。
///
/// 两条不显然的规则：
///
/// - **指向不存在的项目 = 未分组。** 删完项目到下一次 `GET /sessions` 之间，
///   手上这批会话的 `projectId` 还指着那个已经没了的 id。按「找不到就丢掉」
///   处理的话，这段时间里那些会话会从侧边栏整个消失 —— 而用户刚被告知
///   「会话不会丢」。
/// - **空项目也留一行。** 刚建好的项目一条会话都没有，不画出来的话新建按钮
///   看起来什么也没做，而它恰恰是要被拖进东西的那个空篮子。
List<SessionGroup> groupSessionsByProject(
  List<ChatSession> sessions,
  List<Project> projects,
) {
  final buckets = <String, List<ChatSession>>{
    for (final p in projects) p.id: <ChatSession>[],
  };
  final ungrouped = <ChatSession>[];

  for (final s in sessions) {
    final bucket = s.projectId == null ? null : buckets[s.projectId];
    (bucket ?? ungrouped).add(s);
  }

  return [
    for (final p in projects)
      SessionGroup(project: p, sessions: buckets[p.id]!),
    SessionGroup(project: null, sessions: ungrouped),
  ];
}

/// 删除项目确认框里那句话。
///
/// 单独拎出来是为了能被测试逐字盯住：用户在这里唯一真正害怕的事情是
/// 「删项目把对话也删了」，而这恰恰是唯一**不会**发生的事。这句话说不清楚，
/// 代价不是一次误操作，是从此没人敢碰这个功能。
String deleteProjectWarning(Project project) {
  final count = project.sessionCount;
  final scope = count > 0 ? '里面的 $count 个会话' : '里面的会话';
  return '只删掉「${project.name}」这个分组本身。'
      '$scope不会被删除，它们会变成「未分组」，'
      '消息、附件和已抽取的记忆一概不动。';
}
