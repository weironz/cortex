import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../core/local_agent.dart';
import '../core/ulid.dart';
import '../models/attachment.dart';
import '../models/chat_event.dart';
import '../models/chat_message.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/project.dart';
import '../models/tool_call.dart';
import '../models/workspace.dart';
import 'app_providers.dart';
import 'chat_state.dart';
import 'confirm_controller.dart';
import 'project_controller.dart';

/// Owns sessions, transcripts and the in-flight generation.
class ChatController extends Notifier<ChatState> {
  StreamSubscription<ChatEvent>? _subscription;

  /// Deltas are coalesced into this buffer and flushed on a timer.
  ///
  /// A backend emitting one event per token can fire well above the display
  /// refresh rate; publishing state per event would schedule a rebuild that
  /// the framework then discards. Batching to one publish per frame keeps the
  /// typing effect visually identical while cutting rebuild count by ~4× on a
  /// fast stream. The buffer is always flushed before any terminal event, so
  /// no token can be dropped.
  final StringBuffer _pending = StringBuffer();
  Timer? _flushTimer;
  static const _flushInterval = Duration(milliseconds: 16);

  /// 作废序号：换后端时 +1，让在飞的请求的结果（尤其是**尸体**）被丢掉。
  int _requestSeq = 0;

  /// 这个结果是不是已经过期了 —— 每个 `await` 之后、写 state 之前都要问一次。
  ///
  /// 与 [Ref.mounted] 一起判：整个应用退出与「只是换了后端」是两件事，
  /// 而两者都不该让结果落地。
  bool _stale(int seq) => seq != _requestSeq || !ref.mounted;

  @override
  ChatState build() {
    ref.onDispose(() {
      _flushTimer?.cancel();
      _subscription?.cancel();
    });
    // Re-hydrate whenever the data source flips (mock ↔ live).
    //
    // # 为什么这里要先作废在飞的请求
    //
    // 换后端会 dispose 掉旧的 `HttpCortexApi`，而它的 dispose 是
    // `_client.close()` —— **正在飞的请求当场被掐断**，抛出
    // 「Connection closed before full header was received」。
    //
    // 那个异常在 `_reload()` 把 state 重置成干净的**之后**才落地，于是把
    // `sessionsError` / `Transcript.error` 又写了回去。界面停在
    // 「连不上 cortexd」，而 cortexd 好端端地活着 —— 用户唯一的出路是
    // 手点重试。
    //
    // 这条路径比记忆面板那条命中得更早：会话列表与首个会话的消息是
    // **开机就发**的，凭据一续上（或本地 agent 一就绪）就正好撞上。
    ref.listen(cortexApiProvider, (_, _) {
      _requestSeq++;
      _reload();
    });
    Future.microtask(_reload);
    return const ChatState();
  }

  CortexApi get _api => ref.read(cortexApiProvider);

  // -------------------------------------------------------------- lifecycle

  Future<void> _reload() async {
    await _cancelStream();
    if (!ref.mounted) return;
    state = ChatState(
      sessionsLoading: true,
      // The toggle is a view preference, not backend data — a source swap
      // should not silently re-hide what the user asked to see.
      showArchived: state.showArchived,
    );
    await loadSessions();
  }

  /// Every `state =` below sits behind a [Ref.mounted] check.
  ///
  /// Not defensive noise: this runs after an `await`, and the provider can be
  /// gone by then. Two ways in — the app shutting down while the first load is
  /// still in flight, and (much more likely now) `SyncController` calling this
  /// from a WebSocket bump at a moment it does not control. Writing to a
  /// disposed `Ref` throws out of a future nobody awaits, which surfaces as an
  /// unhandled async error rather than anything the UI can act on.
  Future<void> loadSessions() async {
    if (!ref.mounted) return;
    final seq = _requestSeq;
    state = state.copyWith(sessionsLoading: true, sessionsError: null);
    try {
      final remote = await _api.sessions(includeArchived: state.showArchived);
      if (_stale(seq)) return;
      final merged = _mergeSessions(remote);
      final active =
          state.activeSessionId ?? (merged.isNotEmpty ? merged.first.id : null);
      state = state.copyWith(
        sessions: merged,
        sessionsLoading: false,
        activeSessionId: active,
      );
      if (active != null) unawaited(_ensureTranscript(active));
    } on CortexApiException catch (e) {
      if (_stale(seq)) return;
      state = state.copyWith(
        sessionsLoading: false,
        sessionsError: e.message,
        // Drafts only exist on this device; dropping them because the daemon
        // is momentarily unreachable would destroy work the user can still see.
        sessions: state.sessions.where((s) => s.isLocalDraft).toList(),
      );
    } on Object catch (e) {
      if (_stale(seq)) return;
      state = state.copyWith(sessionsLoading: false, sessionsError: '$e');
    }
  }

  /// Reconciles the daemon's list with what is on screen.
  ///
  /// A plain replace loses three things:
  ///
  /// * **Local drafts.** [createSession] mints a ULID client-side and shows the
  ///   session immediately; the daemon only learns of it when the first turn
  ///   commits. Before that, a refresh — now triggered by any WebSocket bump,
  ///   including one caused by somebody else's device — would make the session
  ///   the user is typing in vanish.
  /// * **The "未同步" flag.** When the draft's id does come back from the
  ///   server, that *is* the write receipt: the remote row carries
  ///   `isLocalDraft == false`, so the badge clears itself.
  /// * **Local-only edits.** A rename against a daemon without
  ///   `PATCH /sessions/{id}` lives only here. Letting the next refresh
  ///   overwrite it would make the rename appear to work and then silently
  ///   undo itself a second later — worse than refusing outright.
  List<ChatSession> _mergeSessions(List<ChatSession> remote) {
    final remoteIds = remote.map((s) => s.id).toSet();
    final unconfirmed = state.sessions.where(
      (s) => s.isLocalDraft && !remoteIds.contains(s.id),
    );
    final overrides = {
      for (final s in state.sessions)
        if (s.hasLocalOverrides) s.id: s,
    };
    final reconciled = [
      for (final s in remote)
        if (overrides[s.id] case final local?)
          s.copyWith(
            title: local.title,
            titleIsCustom: local.titleIsCustom,
            archived: local.archived,
            workspace: local.workspace,
            projectId: local.projectId,
            hasLocalOverrides: true,
          )
        else
          s,
    ];
    return [...unconfirmed, ...reconciled];
  }

  // ---------------------------------------------------------------- sessions

  void selectSession(String id) {
    if (state.activeSessionId == id) return;
    // An in-flight generation belongs to the session that started it; keep it
    // running and just look away. Cancelling on every sidebar click would lose
    // work the user did not ask to discard.
    state = state.copyWith(
      activeSessionId: id,
      sendError: null,
      // 打开它就等于「看过了」。留着的话，那个「跑完了」的标记会一直挂在
      // 一个用户正盯着的会话上
      finished: state.finished.contains(id)
          ? ({...state.finished}..remove(id))
          : state.finished,
    );
    unawaited(_ensureTranscript(id));
    unawaited(_tryAttach(id));
  }

  /// 打开一个会话时，看看它是不是还有一轮在跑；在就挂上去接着看。
  ///
  /// # 这是「关掉浏览器活还在干」唯一看得见的出口
  ///
  /// 轮次跑在服务端一个独立的 task 里，客户端断开不会中止它。缺的一直是
  /// 回来的路 —— 在这条之前，人回来只能等 episode 落库。
  ///
  /// # 三种「不挂」都要静悄悄
  ///
  /// 没在跑（404）、这个后端答不了这条路由（旧 agent）、网络不好 —— 三种
  /// 都是常态而非故障。任何一种弹出错误框，代价是**每打开一个旧会话都弹
  /// 一次**，而它本来什么都不该发生。
  Future<void> _tryAttach(String sessionId) async {
    // 本地已经有一条流在跑（就是这个会话发起的那条）就别再挂一次 ——
    // 会收到两份同样的事件
    if (state.streaming != null) return;

    final Stream<ChatEvent> stream;
    try {
      stream = _api.attachChat(sessionId);
    } on CortexApiException {
      return;
    }

    // 先挂上再建 streaming 状态：反过来的话，挂失败会在界面上留下一个
    // 永远转圈的「正在生成」
    late final StreamSubscription<ChatEvent> sub;
    var started = false;
    sub = stream.listen(
      (event) {
        if (!started) {
          started = true;
          // 重挂拿到的第一条事件就意味着「它确实在跑」。这时才建 streaming
          // 状态，`_onEvent` 后面那一串才有东西可改
          state = state.copyWith(
            streaming: StreamingTurn(
              messageId: Ulid.generate(),
              sessionId: sessionId,
              startedAt: DateTime.now(),
            ),
            sendError: null,
          );
          _subscription = sub;
        }
        _onEvent(event);
      },
      // 挂不上（404 / 旧后端 / 断网）一律当成「没在跑」，见方法文档
      onError: (Object e, StackTrace st) {
        if (started) _onError(e, st);
      },
      onDone: () {
        if (started) _onDone();
      },
      cancelOnError: true,
    );
  }

  /// 新建一个本地草稿会话。[projectId] 非空时它属于那个项目。
  ///
  /// ## 分组是**在第一轮结束之后**才补到服务端的
  ///
  /// 草稿只活在这台设备上，服务端要到第一次 `POST /chat` 才知道有这个 id。
  /// 而 `POST /chat` 的请求体里没有 `project_id`（也不该有：那是会话的属性，
  /// 不是这一句话的属性）。所以这里只记下待办，等 [_commit] 看到这一轮真的
  /// 落了地，再补一次 `PATCH /sessions/{id}`。
  ///
  /// 反过来先 PATCH 是不行的：那时服务端上还没有这一行，只会拿回 404。
  String createSession({String? projectId}) {
    final session = ChatSession(
      id: Ulid.generate(),
      title: '新会话',
      updatedAt: DateTime.now(),
      projectId: projectId,
      isLocalDraft: true,
    );
    if (projectId != null) _pendingProjectAssignment[session.id] = projectId;
    state = state.copyWith(
      sessions: [session, ...state.sessions],
      activeSessionId: session.id,
      // Marked loaded so the transcript view does not try to fetch a session
      // the daemon has never heard of and render a 404 as a failure.
      transcripts: {
        ...state.transcripts,
        session.id: const Transcript(loadedFromServer: true),
      },
      sendError: null,
    );
    return session.id;
  }

  void setShowArchived(bool value) {
    if (state.showArchived == value) return;
    state = state.copyWith(showArchived: value);
    // Refetch rather than filter locally: archived sessions were never sent,
    // so there is nothing local to reveal.
    unawaited(loadSessions());
  }

  /// Renames a session.
  ///
  /// Rethrows a genuine failure so the dialog can show it; an *unsupported*
  /// endpoint is not a failure the user can act on, so the change is kept
  /// locally and flagged instead.
  Future<void> renameSession(String id, String title) async {
    final trimmed = title.trim();
    if (trimmed.isEmpty) return;
    await _patch(
      id,
      call: () => _api.updateSession(id, title: trimmed),
      local: (s) => s.copyWith(
        title: trimmed,
        titleIsCustom: true,
        hasLocalOverrides: true,
      ),
    );
  }

  /// Archives or restores. **Not** deletion: episodes, attachments and the
  /// memory extracted from them are untouched, which is why no confirmation
  /// dialog threatens permanence.
  Future<void> setArchived(String id, bool archived) async {
    await _patch(
      id,
      call: () => _api.updateSession(id, archived: archived),
      local: (s) => s.copyWith(archived: archived, hasLocalOverrides: true),
    );
    // An archived session that is still selected would leave the transcript on
    // screen with no matching row in the list.
    if (archived && !state.showArchived && state.activeSessionId == id) {
      final next = state.visibleSessions.where((s) => s.id != id).firstOrNull;
      state = state.copyWith(activeSessionId: next?.id);
      if (next != null) unawaited(_ensureTranscript(next.id));
    }
  }

  /// 移入项目，[projectId] 为 null 表示移出（变未分组）。
  ///
  /// 项目里的会话数变了，所以顺手让项目列表重拉一次 —— 那个数字会出现在
  /// 删除确认里（「里面的 N 个会话不会被删除」），而一个说错数的确认框
  /// 正好摧毁它唯一的作用。
  Future<void> moveSessionToProject(String id, String? projectId) async {
    _pendingProjectAssignment.remove(id);
    await _patch(
      id,
      call: () => _api.moveSessionToProject(id, projectId),
      local: (s) => s.copyWith(projectId: projectId, hasLocalOverrides: true),
    );
    if (!ref.mounted) return;
    unawaited(ref.read(projectControllerProvider.notifier).load());
  }

  /// 建在项目里、但服务端还不知道的草稿：会话 id → 项目 id。
  ///
  /// 只在内存里，重启即忘 —— 一个从没发过消息的草稿本来也不会留下。
  final Map<String, String> _pendingProjectAssignment = {};

  /// 把建会话时选的项目补给服务端。**在这一轮开始之前，而且要等它。**
  ///
  /// # 为什么不能等第一轮跑完
  ///
  /// 沙箱容器与工作区卷按「owner + 项目」分（服务端 `DelegatedScope::key`）。
  /// 服务端是在**收到 /chat 的那一刻**去库里读这个会话属于哪个项目的 ——
  /// 那时项目还没补上的话，这一轮就落进**未分组**那个容器，而它是所有
  /// 没分组会话共用的。
  ///
  /// 真机上就是这么表现的：新建项目、发第一句、让它写个文件，文件进了默认
  /// 沙箱；第二轮起才进项目沙箱，于是**第一轮写的东西再也看不见**。
  /// 生产上留下的证据是同一个 owner 名下并排两套容器与两个卷。
  ///
  /// 这与紧邻的 [`_ensureLocalWorkspace`] 是同一条道理，它的注释写着
  /// 「等 turn 造好再绑就晚了」—— 项目也一样晚。
  ///
  /// # 原来的理由（「服务端现在才确实有这一行会话」）是错的
  ///
  /// `PATCH /sessions/{id}` **不要求会话已经存在**：它只是往事件流里追加
  /// 一条，状态行随之物化。实测对一个从未出现过的 id PATCH 回 200，
  /// 响应里带着 `project_id`。404 的是 `GET`（它要有消息才有 transcript），
  /// 两者被混为一谈了。
  ///
  /// # 失败就不发这一轮
  ///
  /// 与 [`_ensureLocalWorkspace`] 同样的取舍：分组没存上就发出去，等于把
  /// 文件写进一个用户没打算用的工作区 —— 而那件事**没有任何提示**，
  /// 等他发现时东西已经在另一个卷里了。宁可当场说「没发出去，再试一次」。
  Future<bool> _flushPendingProject(String sessionId) async {
    final projectId = _pendingProjectAssignment[sessionId];
    if (projectId == null) return true;
    try {
      final updated = await _api.moveSessionToProject(sessionId, projectId);
      _pendingProjectAssignment.remove(sessionId);
      if (!ref.mounted) return false;
      _replaceSession(
        sessionId,
        (s) => updated.copyWith(isLocalDraft: s.isLocalDraft),
      );
      return true;
    } on Object catch (e) {
      if (!ref.mounted) return false;
      state = state.copyWith(
        sendError:
            '这个会话还没归到项目里，先发消息会让文件落进默认工作区。'
            '请再试一次（$e）',
      );
      return false;
    }
  }

  /// Binds a workspace. The daemon validates the path and its rejection message
  /// is written to be read by the user, so it is allowed to propagate.
  Future<void> bindWorkspace(String id, String root) async {
    final trimmed = root.trim();
    if (trimmed.isEmpty) return;
    await _bindWorkspace(id, trimmed);
  }

  Future<void> unbindWorkspace(String id) async => _bindWorkspace(id, null);

  /// 换云沙箱卷里的子目录。[name] 为 null = 回到卷根。
  ///
  /// # 为什么不走 [_patch] 那条「服务端不认就本地记着」的回落
  ///
  /// 那条回落是给「改名 / 归档」这类**纯客户端也说得通**的东西准备的：
  /// 服务端没这条路由时，本地留一份带标记的改动仍然对用户有意义。
  ///
  /// 这一条不一样：它决定的是**容器里 agent 的根目录**，而那个判断只在
  /// 服务端做。本地记一份「我选了 client-a」而服务端不知道，界面会显示
  /// 那个名字，agent 却仍然在卷根上写文件 —— 一个不报错的谎。
  /// 所以失败就让它失败，由调用方把服务端那句话原样显示出来。
  Future<void> setContainerWorkspace(String id, String? name) async {
    final updated = await _api.setContainerWorkspace(id, name);
    if (!ref.mounted) return;
    // 用服务端回来的那份，而不是把 `name` 直接写进状态：服务端才是权威，
    // 而它可能规范化过（比如去掉首尾空白）
    _replaceSession(
      id,
      (s) => s.copyWith(containerWorkspace: updated.containerWorkspace),
    );
  }

  /// 用户对这个会话显式选了「云端」。见 [_ensureLocalWorkspace]。
  final Set<String> _cloudByChoice = {};

  bool isCloudByChoice(String id) => _cloudByChoice.contains(id);

  /// 「云端」：这一轮交给远端 agent 跑，本机不落地任何目录。
  Future<void> chooseCloud(String id) async {
    _cloudByChoice.add(id);
    await unbindWorkspace(id);
  }

  /// 绑一个本机目录。会**取消**之前选的「云端」——两者互斥，
  /// 而让一个会话同时带着「用户要云端」和「绑着本机目录」两个事实，
  /// 下一次读到的人必然要在某处再判一遍谁赢。
  Future<void> chooseLocalWorkspace(String id, String path) async {
    _cloudByChoice.remove(id);
    await bindWorkspace(id, path);
  }

  /// 在默认根目录下开一个命名工作空间并绑上。
  ///
  /// [projectId] 给「这个工作空间就是那个项目在本机的落地目录」用：带上它，
  /// 项目改名之后不会再建第二个文件夹。
  Future<void> createLocalWorkspace(
    String id,
    String name, {
    String? projectId,
  }) async {
    final path = await _api.createLocalWorkspace(
      name: name,
      projectId: projectId,
    );
    await chooseLocalWorkspace(id, path);
  }

  /// 首轮之前，为没选工作区的**新建**会话在默认根目录下开一个按日期时间
  /// 命名的文件夹。
  ///
  /// # 三个前提缺一不可
  ///
  /// - `kLocalAgentSupported` —— Web 上没有本机可落。
  /// - `isLocalDraft` —— **这个会话是刚在这台机器上新建的**。少了它，一个在
  ///   浏览器里建、正在云端跑的会话只要在桌面端发过一句话，就被劫持到本机，
  ///   而两边指着完全不同的文件系统（那正是 `session.runtime` 要防的事）。
  /// - 没绑过、也没选过云端。
  ///
  /// # 为什么失败要挡住这一轮
  ///
  /// 桌面端的默认是「跑在本机」。落不下来还照发的话，这一轮会静默跑到云端 ——
  /// 文件写去了另一个文件系统，而界面上什么都没说。唯一的例外是**后端根本
  /// 没有这条路由**（对着一个纯 cortexd）：那种情况下「没有本机」是事实，
  /// 不是故障。
  Future<bool> _ensureLocalWorkspace(String id) async {
    if (!kLocalAgentSupported || _cloudByChoice.contains(id)) return true;
    final session = state.sessions.firstWhereOrNull((s) => s.id == id);
    if (session == null || !session.isLocalDraft) return true;
    if (session.workspace != null) return true;
    try {
      // 归属了项目就落在**那个项目的本机目录**里，而不是一个按时间命名的
      // 新文件夹 —— 「工作空间就是项目在本机的落地目录」，一个概念两个身体：
      // 项目 P 在云端是卷 cortex-ws-<owner>--p-<hash>，在这台机器上是 <根>/P。
      //
      // 带上 project_id，于是项目改名之后不会再建第二个文件夹（agent 那侧
      // 按 id 记账、按名字建）
      final project = _projectOf(session.projectId);
      final bound = project == null
          ? await _api.autoBindLocalWorkspace(id)
          : await _bindProjectDir(id, project);
      if (!ref.mounted) return false;
      if (bound != null) {
        _replaceSession(
          id,
          (s) => s.copyWith(workspace: Workspace(root: bound)),
        );
      }
      return true;
    } on CortexApiException catch (e) {
      if (e.isUnsupported) return true;
      state = state.copyWith(sendError: '开不出这次对话的工作目录：${e.message}');
      return false;
    }
  }

  Project? _projectOf(String? id) => id == null
      ? null
      : ref
            .read(projectControllerProvider)
            .projects
            .firstWhereOrNull((p) => p.id == id);

  /// 建（或找回）项目的本机目录并绑上，回绑定后的规范化路径。
  Future<String?> _bindProjectDir(String sessionId, Project project) async {
    final dir = await _api.createLocalWorkspace(
      name: project.name,
      projectId: project.id,
    );
    return _api.bindLocalWorkspace(sessionId, dir);
  }

  /// The local agent owns this binding, so ask it first.
  ///
  /// `PUT /local/workspaces/{id}` never touches the network — which is exactly
  /// why it exists. Binding used to ride `PATCH /sessions/{id}`, and that broke
  /// twice over: offline the forward 502'd *after* the binding had already been
  /// written to disk, and online the daemon answered `workspace: null` (the
  /// local agent nulls it on the way out) so [_patch]'s wholesale session
  /// replacement put the UI straight back to "unbound".
  ///
  /// Only the workspace field is merged here — everything else in the session
  /// (title, message count) still comes from the daemon, and this route
  /// deliberately does not return those.
  ///
  /// Against a plain cortexd the route is absent and the old `PATCH` path takes
  /// over — but **binding there is now refused**, deliberately. The server has
  /// no execution environment: its file tools would touch the *server's* disk,
  /// which is neither your machine nor a throwaway container. The daemon
  /// answers 400 with a message naming the two paths that do work (desktop app,
  /// or `cortex-local` on this machine), and that message is what the user sees.
  ///
  /// Unbinding still goes through: an old session may carry a server-side
  /// binding from before this change, and refusing to clear it would weld it on.
  Future<void> _bindWorkspace(String id, String? path) async {
    try {
      final bound = await _api.bindLocalWorkspace(id, path);
      if (!ref.mounted) return;
      _replaceSession(
        id,
        // `copyWith` takes a sentinel default, so an explicit `null` here
        // really does clear the binding rather than leaving it alone
        (s) => s.copyWith(
          workspace: bound == null ? null : Workspace(root: bound),
        ),
      );
      return;
    } on CortexApiException catch (e) {
      if (!e.isUnsupported) rethrow;
    }
    await _patch(
      id,
      call: () => path == null
          ? _api.updateSession(id, clearWorkspace: true)
          : _api.updateSession(id, workspace: path),
      local: (s) => s.copyWith(
        workspace: path == null ? null : Workspace(root: path),
        hasLocalOverrides: true,
      ),
    );
  }

  /// One shape for all four mutations: try the server, fall back to a flagged
  /// local edit **only** when the endpoint does not exist.
  ///
  /// The distinction matters. A 400 from the workspace validator ("整台机器不是
  /// 工作区") must reach the user unchanged — swallowing it and applying the
  /// binding locally would show a workspace that the agent will never honour.
  Future<void> _patch(
    String id, {
    required Future<ChatSession> Function() call,
    required ChatSession Function(ChatSession) local,
  }) async {
    try {
      final updated = await call();
      if (!ref.mounted) return;
      _replaceSession(
        id,
        (s) => updated.copyWith(isLocalDraft: s.isLocalDraft),
      );
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      if (!e.isUnsupported) rethrow;
      _replaceSession(id, local);
    }
  }

  void _replaceSession(String id, ChatSession Function(ChatSession) update) {
    final sessions = [...state.sessions];
    final index = sessions.indexWhere((s) => s.id == id);
    if (index == -1) return;
    sessions[index] = update(sessions[index]);
    state = state.copyWith(sessions: sessions);
  }

  // ------------------------------------------------------------ history replay

  /// Fetches a session's messages the first time it is opened.
  ///
  /// Only ever runs when nothing is held for the session yet. A session the
  /// user has already talked in this run has its turns in memory, and merging a
  /// server replay into them would need identity for the *user* message too —
  /// which the client does not have, because it never learns the episode id of
  /// its own prompt. Refetching would therefore duplicate every user turn.
  Future<void> _ensureTranscript(String id) async {
    if (state.transcripts.containsKey(id)) return;
    final session = state.sessions.firstWhereOrNull((s) => s.id == id);
    if (session != null && session.isLocalDraft) return;
    await loadTranscript(id);
  }

  /// Fetches the **newest** page of a session.
  Future<void> loadTranscript(String id) async {
    if (!ref.mounted) return;
    final current = state.transcripts[id] ?? const Transcript();
    if (current.loading) return;
    final seq = _requestSeq;
    _putTranscript(id, current.copyWith(loading: true, error: null));

    try {
      final detail = await _api.sessionDetail(id, limit: kEpisodePage);
      if (_stale(seq)) return;
      _putTranscript(
        id,
        Transcript(
          messages: _messagesFromEpisodes(detail.episodes),
          loadedFromServer: true,
          hasEarlier: detail.hasMore,
          cursor: detail.nextCursor,
        ),
      );
      // The detail response carries a fresher overview than the list did.
      _replaceSession(
        id,
        (s) => s.hasLocalOverrides
            ? s
            : detail.session.copyWith(isLocalDraft: s.isLocalDraft),
      );
    } on CortexApiException catch (e) {
      if (_stale(seq)) return;
      _putTranscript(
        id,
        (state.transcripts[id] ?? const Transcript()).copyWith(
          loading: false,
          // A session with no episodes yet is a 404 here, not a failure.
          error: e.statusCode == 404 ? null : e.message,
          loadedFromServer: e.statusCode == 404,
        ),
      );
    } on Object catch (e) {
      if (_stale(seq)) return;
      _putTranscript(
        id,
        (state.transcripts[id] ?? const Transcript()).copyWith(
          loading: false,
          error: '$e',
        ),
      );
    }
  }

  /// Fetches the page **before** what is held and prepends it.
  ///
  /// A failure here deliberately does not touch [Transcript.error]: that field
  /// drives a full-screen "拉不到这个会话" state, and blanking a conversation the
  /// user is already reading because one page up failed is a worse outcome than
  /// the button simply staying available to try again.
  Future<void> loadEarlier(String id) async {
    final current = state.transcripts[id];
    final cursor = current?.cursor;
    if (current == null || cursor == null) return;
    if (current.loading || current.loadingEarlier) return;
    final seq = _requestSeq;
    _putTranscript(id, current.copyWith(loadingEarlier: true));

    try {
      final page = await _api.sessionDetail(
        id,
        limit: kEpisodePage,
        before: cursor,
      );
      // 这里作废得比别处更要紧：这一页来自**旧后端**，而 prepend 是把它拼进
      // 新后端的正文里。换的若是账号，那就是两个人的消息混在同一屏上 ——
      // 比报一个错要糟得多。
      if (_stale(seq)) return;
      final held = state.transcripts[id] ?? const Transcript();
      _putTranscript(
        id,
        held.copyWith(
          // Prepend: the page is older than everything already on screen, and
          // each page is itself oldest-first.
          messages: [..._messagesFromEpisodes(page.episodes), ...held.messages],
          loadingEarlier: false,
          hasEarlier: page.hasMore,
          cursor: page.nextCursor,
        ),
      );
    } on Object {
      if (_stale(seq)) return;
      _putTranscript(
        id,
        (state.transcripts[id] ?? const Transcript()).copyWith(
          loadingEarlier: false,
        ),
      );
    }
  }

  /// Turns one page of archival records into view-models.
  ///
  /// ## The audit drawer moves from the question to the answer
  ///
  /// Server-side, `episode_memories` and `episode_tool_calls` are anchored on
  /// the **user** episode: that row is the turn's anchor and always exists,
  /// whereas the assistant episode is never written when the model errors.
  /// Drawing the drawer there would leave a replayed conversation looking
  /// different from the live one it just was — the drawer under the question
  /// instead of under the answer.
  ///
  /// So a user turn's attribution is carried onto the assistant turn that
  /// follows it. When none follows (the failed-turn case the server's anchoring
  /// exists for) it stays on the user message, where the drawer is still
  /// rendered — that turn is the one an audit most wants to look at.
  static List<ChatMessage> _messagesFromEpisodes(List<Episode> episodes) {
    final out = <ChatMessage>[];
    for (var i = 0; i < episodes.length; i++) {
      final e = episodes[i];
      final isUser = e.role != 'assistant';
      final carriesAttribution = e.toolCalls.isNotEmpty;
      // Only forward across a user → assistant boundary. Two user messages in a
      // row are two turns, and the second one's assistant answer is not this
      // one's.
      final handOff =
          isUser &&
          carriesAttribution &&
          i + 1 < episodes.length &&
          episodes[i + 1].role == 'assistant';

      out.add(
        ChatMessage(
          id: e.id,
          role: isUser ? MessageRole.user : MessageRole.assistant,
          text: e.text,
          createdAt: e.occurredAt ?? DateTime.now(),
          attachments: e.attachments,
          toolCalls: handOff ? const [] : e.toolCalls,
          episodeId: e.id,
        ),
      );

      if (handOff) {
        final answer = episodes[i + 1];
        out.add(
          ChatMessage(
            id: answer.id,
            role: MessageRole.assistant,
            text: answer.text,
            createdAt: answer.occurredAt ?? DateTime.now(),
            attachments: answer.attachments,
            toolCalls: [...e.toolCalls, ...answer.toolCalls],
            episodeId: answer.id,
          ),
        );
        i++;
      }
    }
    return out.toList(growable: false);
  }

  void _putTranscript(String id, Transcript transcript) {
    state = state.copyWith(transcripts: {...state.transcripts, id: transcript});
  }

  // -------------------------------------------------------------------- send

  Future<void> send(
    String rawText, {
    List<Attachment> attachments = const [],
  }) async {
    final text = rawText.trim();
    // An attachment with no words is a legitimate message ("看这张图")…
    if (text.isEmpty && attachments.isEmpty) return;
    if (state.streaming != null) return; // one generation at a time

    final sessionId = state.activeSessionId ?? createSession();

    // 工作目录要在**这一轮开始之前**定下来：`routes::chat` 拿「有没有本地
    // 绑定」分流，等 turn 造好再绑就晚了，那一轮已经送去云端了
    if (!await _ensureLocalWorkspace(sessionId)) return;
    if (!ref.mounted) return;
    // 同上：项目决定沙箱容器与工作区卷，服务端在收到 /chat 那一刻就要读到它
    if (!await _flushPendingProject(sessionId)) return;
    if (!ref.mounted) return;

    final userMessage = ChatMessage(
      id: Ulid.generate(),
      role: MessageRole.user,
      text: text,
      createdAt: DateTime.now(),
      attachments: attachments,
    );
    _appendMessage(sessionId, userMessage);
    _touchSession(sessionId, titleFrom: text.isEmpty ? null : text);

    final turn = StreamingTurn(
      messageId: Ulid.generate(),
      sessionId: sessionId,
      startedAt: DateTime.now(),
    );
    // 记下「我发出去了，还没见到收尾」。用于侧栏徽章 —— 用户关掉这个会话
    // 去别处逛的时候，那一格是他唯一的线索。见 `ChatState.unfinished`
    state = state.copyWith(
      streaming: turn,
      sendError: null,
      unfinished: {...state.unfinished, sessionId},
    );

    final stream = _api.chat(
      sessionId: sessionId,
      message: text,
      attachments: attachments,
      // 逐轮读，不缓存：用户在输入框底部随时能改，改完这一句就该按新档位走
      permissionMode: ref.read(permissionModeProvider),
    );
    _subscription = stream.listen(
      _onEvent,
      onError: _onError,
      onDone: _onDone,
      cancelOnError: true,
    );
  }

  /// 把某一条用户消息**再发一次**。
  ///
  /// # 为什么是「再发一次」而不是「改写那一轮」
  ///
  /// 这个项目的历史是 append-only —— `UPDATE` / `DELETE` 只在 redact/purge
  /// 里被允许。别家那种「编辑后重发把旧的那一轮换掉」在这里做不到，
  /// 而假装做到了（只在客户端把它藏起来）更糟：换一台设备回放，
  /// 那条「已经改掉」的消息原样还在，而用户以为它没了。
  ///
  /// 所以重发就是重发：新的一轮追加在末尾，旧的那一轮留在历史里。
  /// 界面上的文案必须把这件事说清楚，别叫「编辑」。
  ///
  /// 附件跟着一起带上 —— 一条「看这张图」重发时丢了图，那一轮必然答非所问。
  /// 它们是已登记的 blob（哈希在服务端），重发不需要再传一遍字节。
  /// 「一轮只跑一次」那道闸**在 [send] 里**，这里不再写一遍：写了的话
  /// 两处都得对，而删掉其中任何一处都没有测试会红 —— 那种防御只是让人
  /// 以为这里做了检查。
  Future<void> resend(String messageId) async {
    final sessionId = state.activeSessionId;
    if (sessionId == null) return;
    final messages = state.activeTranscript;
    final source = messages.where((m) => m.id == messageId).firstOrNull;
    if (source == null || source.role != MessageRole.user) return;
    await send(source.text, attachments: source.attachments);
  }

  /// 找到「这条回答是回应哪句话的」。
  ///
  /// 失败那一轮上的「重试」要它：屏幕上出错的是 assistant 那一块，
  /// 而要重发的是它**前面**那条用户消息。
  ///
  /// 往前找而不是按下标减一：工具行、被打断的轮次都可能插在中间，
  /// 而「上一条用户消息」这个说法在任何排布下都成立。
  String? userMessageBefore(String assistantMessageId) {
    final sessionId = state.activeSessionId;
    if (sessionId == null) return null;
    final messages = state.activeTranscript;
    final at = messages.indexWhere((m) => m.id == assistantMessageId);
    if (at < 0) return null;
    for (var i = at - 1; i >= 0; i--) {
      if (messages[i].role == MessageRole.user) return messages[i].id;
    }
    return null;
  }

  void _onEvent(ChatEvent event) {
    final turn = state.streaming;
    if (turn == null) return;

    switch (event) {
      // 排在别人后面。**不是终态** —— 同一条流接着会送这一轮自己的内容。
      // 记下来只为在气泡里说一句话：排队期间流上除了 keepalive 什么都没有，
      // 不说的话屏幕上就是一个转了几分钟的圈
      case ChatQueuedEvent(:final ahead):
        state = state.copyWith(streaming: turn.copyWith(queuedAhead: ahead));

      case ChatDeltaEvent(:final text):
        if (text.isEmpty) return;
        // 有自己的内容了 = 闸门已经放开。不清的话整轮都挂着「排队中」
        _leaveQueue();
        _pending.write(text);
        _scheduleFlush();

      case ChatToolEvent(:final name, :final summary, :final path, :final diff):
        _leaveQueue();
        _flushPending();
        // Call and result arrive as two events; [ToolCall.merge] folds them
        // into a single row instead of printing the same tool twice.
        final current = state.streaming;
        if (current == null) return;
        state = state.copyWith(
          streaming: current.copyWith(
            toolCalls: ToolCall.merge(
              current.toolCalls,
              name,
              summary,
              path: path,
              diff: diff,
            ),
          ),
        );

      case ChatConfirmEvent(:final request):
        // Flushed first so any text produced before the agent reached the tool
        // is on screen while the user decides — the prompt asks them to judge a
        // command, and the model's own reasoning about it is part of the
        // evidence.
        _flushPending();
        // **Not** terminal. The turn is suspended, not finished: deltas resume
        // once a receipt lands (or the timeout fires and the agent is told
        // nobody answered). Committing here would drop the live bubble and make
        // the resumed output look like a second, unprompted reply.
        ref
            .read(confirmControllerProvider.notifier)
            .offer(request, sessionId: turn.sessionId);

      case ChatDoneEvent(:final episodeId):
        _commit(episodeId: episodeId);

      case ChatErrorEvent(:final message):
        _commit(error: message);

      case ChatUnknownEvent():
        // Forward compatibility: ignore quietly rather than break the turn.
        break;
    }
  }

  /// 收到本轮自己的第一条内容就不再是「排队中」了。
  ///
  /// 幂等且便宜（已经清过就不动状态，免掉一次无谓的重建）—— 所以每条 delta
  /// 都可以无脑调它，不必额外记一个「清过没有」的布尔。
  void _leaveQueue() {
    final turn = state.streaming;
    if (turn == null || turn.queuedAhead == null) return;
    state = state.copyWith(streaming: turn.copyWith(queuedAhead: null));
  }

  void _scheduleFlush() {
    _flushTimer ??= Timer(_flushInterval, () {
      _flushTimer = null;
      _flushPending();
    });
  }

  void _flushPending() {
    if (_pending.isEmpty) return;
    final chunk = _pending.toString();
    _pending.clear();
    final turn = state.streaming;
    if (turn == null) return;
    // Append, never replace — this is what makes the bubble grow instead of
    // re-laying-out from scratch.
    state = state.copyWith(
      streaming: turn.copyWith(
        text: turn.text + chunk,
        awaitingFirstToken: false,
      ),
    );
  }

  void _onError(Object error, StackTrace _) {
    final message = error is CortexApiException ? error.message : '$error';
    _commit(error: message);
  }

  void _onDone() {
    // A stream that closes without an explicit `done` still has usable text.
    if (state.streaming != null) _commit();
  }

  /// Moves the in-flight turn into the transcript and clears streaming state.
  void _commit({String? episodeId, String? error}) {
    _flushTimer?.cancel();
    _flushTimer = null;
    _flushPending();

    final turn = state.streaming;
    if (turn == null) return;

    _subscription?.cancel();
    _subscription = null;

    final message = ChatMessage(
      id: turn.messageId,
      role: MessageRole.assistant,
      text: turn.text,
      createdAt: turn.startedAt,
      toolCalls: turn.toolCalls,
      episodeId: episodeId,
      error: error,
    );

    // 人不在这个会话上时，记一笔「跑完了、你还没看」。
    //
    // 在这一句之前，跑完的表现是徽章从「在跑」变成**什么都没有** ——
    // 而「什么都没有」与「从来没跑过」长得一模一样。于是「派出去干活」
    // 这个能力在观测侧缺最后一格：活干完了没人告诉你。
    final away = turn.sessionId != state.activeSessionId;
    state = state.copyWith(
      streaming: null,
      sendError: error,
      // 见到收尾了就把徽章撤掉。**出错那一轮也撤** —— 它同样不再跑了，
      // 而一个撤不掉的「正在跑」比没有徽章更糟
      unfinished: {...state.unfinished}..remove(turn.sessionId),
      finished: away ? {...state.finished, turn.sessionId} : state.finished,
    );
    _appendMessage(turn.sessionId, message);
    _touchSession(turn.sessionId);
  }

  /// User-initiated abort. Keeps whatever text already arrived.
  /// 停止这一轮。**先掐服务端，再收界面。**
  ///
  /// # 从前这里只取消了订阅
  ///
  /// 那只是「不看了」：服务端那一轮照跑，继续烧 token、继续按模型的意思改
  /// 文件，而屏幕上写着「已停止生成」。一个说了假话的按钮比没有按钮更糟。
  ///
  /// # 顺序：先服务端，后本地
  ///
  /// 反过来的话，取消订阅会让 `_subscription` 变 null，而中间那次网络往返
  /// 期间用户看到的是「已经停了」——若那次请求失败，他得到的仍是一个谎。
  /// 先等服务端确认，界面才敢说停了。
  ///
  /// # 服务端掐不掉时**照样收界面**
  ///
  /// 网断了、后端是个不认这条路由的旧版本 —— 那时最不该做的是把用户按在
  /// 一个转着圈的界面上。收掉本地这一半，并把原因如实写进那条消息里：
  /// 「界面停了，但服务端那一轮可能还在跑」比一句「已停止」诚实。
  Future<void> stopGeneration() async {
    final turn = state.streaming;
    if (turn == null) return;
    String? warning;
    try {
      await _api.stopRun(turn.sessionId);
    } on Object catch (e) {
      warning = '（服务端没能确认停止：$e —— 那一轮可能还在跑）';
    }
    await _subscription?.cancel();
    _subscription = null;
    _commit(error: '已停止生成${warning ?? ''}');
  }

  Future<void> _cancelStream() async {
    _flushTimer?.cancel();
    _flushTimer = null;
    _pending.clear();
    await _subscription?.cancel();
    _subscription = null;
  }

  void clearSendError() {
    if (state.sendError != null) state = state.copyWith(sendError: null);
  }

  /// 顶部那条错误横幅上的「重试」：把最后那句用户消息再发一次。
  ///
  /// # 它以前会把失败那一轮从屏幕上抹掉，那是骗人的
  ///
  /// 原来的实现先把本地 transcript 里的 assistant 与 user 两条删掉再重发，
  /// 于是屏幕上看起来「那一轮没发生过」。**服务端两条都在** —— 历史是
  /// append-only，客户端删的只是自己内存里那一份。换台设备、或者刷新一次
  /// 页面回放，两条原样回来，而用户以为它们没了。
  ///
  /// 现在与气泡上那个「重试」走同一条路：**追加**，不改写。
  Future<void> retryLast() async {
    final transcript = state.activeTranscript;
    if (transcript.isEmpty) return;
    final last = transcript.lastWhere(
      (m) => m.role == MessageRole.user,
      orElse: () => transcript.first,
    );
    if (last.role != MessageRole.user) return;
    state = state.copyWith(sendError: null);
    await resend(last.id);
  }

  // ----------------------------------------------------------------- helpers

  void _appendMessage(String sessionId, ChatMessage message) {
    final existing = state.transcripts[sessionId] ?? const Transcript();
    _putTranscript(
      sessionId,
      existing.copyWith(messages: [...existing.messages, message]),
    );
  }

  /// Bumps `updated_at` and, for a fresh draft, derives a title from the first
  /// user message so the sidebar is not a wall of "新会话".
  void _touchSession(String sessionId, {String? titleFrom}) {
    final sessions = [...state.sessions];
    final index = sessions.indexWhere((s) => s.id == sessionId);
    if (index == -1) return;

    final current = sessions[index];
    // A title the user set explicitly is never overwritten by a derived one.
    final shouldRename =
        titleFrom != null &&
        !current.titleIsCustom &&
        (current.isLocalDraft || current.title == '新会话');

    sessions[index] = current.copyWith(
      updatedAt: DateTime.now(),
      title: shouldRename ? _deriveTitle(titleFrom) : current.title,
      isLocalDraft: current.isLocalDraft && titleFrom == null,
    );
    state = state.copyWith(sessions: sessions);
  }

  static String _deriveTitle(String message) {
    final single = message.replaceAll(RegExp(r'\s+'), ' ').trim();
    if (single.length <= 24) return single;
    return '${single.substring(0, 24)}…';
  }
}

extension _FirstWhereOrNull<E> on Iterable<E> {
  E? firstWhereOrNull(bool Function(E) test) {
    for (final e in this) {
      if (test(e)) return e;
    }
    return null;
  }
}

final chatControllerProvider = NotifierProvider<ChatController, ChatState>(
  ChatController.new,
);
