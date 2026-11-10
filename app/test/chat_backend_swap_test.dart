/// 冷启动时「拉不到这个会话的消息」，点一次重试就好了。
///
/// # 与 memory_backend_swap_test 是同一个 bug 的两条腿
///
/// 记忆面板那条修完了，会话与消息这两条没有 —— 而它们命中得更早：
/// 会话列表和首个会话的消息是**开机就发**的，比记忆面板还早。
///
/// 链条一模一样：
///
/// 1. 应用刚起来 → 会话恢复中 / 本地 agent 未就绪 → `cortexApiProvider`
///    指向一个客户端，`/sessions` 与 `/sessions/{id}` 发了出去
/// 2. 凭据续上 或 agent 起好 → provider 重建 → 旧 `HttpCortexApi` 被
///    `dispose`，而它的 dispose 是 `_client.close()` ——
///    **正在飞的请求当场被掐断**
/// 3. `_reload()` 已经把 state 重置成干净的了
/// 4. 被掐断的那个请求**之后**才抛出，把 `error` 又写了回去
///
/// 结果：界面停在「拉不到这个会话的消息 / Connection closed before full
/// header was received」，而 cortexd 好端端地活着。用户唯一的出路是手点重试。
library;

import 'dart:async';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/models/account.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/models/episode.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/models/session_detail.dart';
import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/assistant_controller.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

ChatSession _session(String id) =>
    ChatSession(id: id, title: '会话 $id', updatedAt: DateTime.utc(2026, 8, 12));

/// 一个「请求发出去之后就被掐断」的替身 —— 复现 `_client.close()`。
class _Aborting extends MockCortexApi {
  final sessions_ = Completer<List<ChatSession>>();
  final detail = Completer<SessionDetail>();

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) => sessions_.future;

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) => detail.future;
}

/// 换后端之后那个「好的」替身。
class _Working extends MockCortexApi {
  int sessionCalls = 0;
  int detailCalls = 0;

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async {
    sessionCalls++;
    return [_session('s1')];
  }

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async {
    detailCalls++;
    return SessionDetail(session: _session(id), episodes: const []);
  }
}

class _ApiSlot {
  _ApiSlot(this.current);

  CortexApi current;
}

ProviderContainer _bootSlot(_ApiSlot slot) => ProviderContainer(
  overrides: [cortexApiProvider.overrideWith((ref) => slot.current)],
);

void _swapApi(ProviderContainer container, _ApiSlot slot, CortexApi next) {
  slot.current = next;
  container.invalidate(cortexApiProvider);
  container.read(cortexApiProvider);
}

class _SubjectApi extends MockCortexApi {
  _SubjectApi({
    required this.account,
    required this.sessionId,
    this.identityGate,
    this.sentStream,
    this.attachStream,
    this.episodes = const [],
  }) : super(instant: true);

  final Account account;
  String sessionId;
  final Completer<Account?>? identityGate;
  final Stream<ChatEvent>? sentStream;
  final Stream<ChatEvent>? attachStream;
  final List<Episode> episodes;
  int identityCalls = 0;
  int attachCalls = 0;
  int bindCalls = 0;
  int projectMoveCalls = 0;

  @override
  Future<Account?> whoAmI() async {
    identityCalls++;
    if (identityGate != null) return identityGate!.future;
    return account;
  }

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => [_session(sessionId)];

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async => SessionDetail(session: _session(id), episodes: episodes);

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    Assistant? assistant,
    List<Skill> skills = const [],
    bool computerUse = false,
    ImagePrefs? imagePrefs,
  }) {
    final stream = sentStream;
    if (stream != null) return stream;
    return super.chat(
      sessionId: sessionId,
      message: message,
      attachments: attachments,
      permissionMode: permissionMode,
      model: model,
      source: source,
      assistant: assistant,
      skills: skills,
      computerUse: computerUse,
      imagePrefs: imagePrefs,
    );
  }

  @override
  Stream<ChatEvent> attachChat(String sessionId) {
    attachCalls++;
    return attachStream ?? super.attachChat(sessionId);
  }

  @override
  Future<String?> bindLocalWorkspace(String id, String? path) async {
    bindCalls++;
    return path;
  }

  @override
  Future<ChatSession> moveSessionToProject(
    String sessionId,
    String? projectId,
  ) async {
    projectMoveCalls++;
    return ChatSession(
      id: sessionId,
      title: '会话 $sessionId',
      updatedAt: DateTime.utc(2026, 8, 12),
      projectId: projectId,
    );
  }
}

class _HttpSubjectApi extends HttpCortexApi {
  _HttpSubjectApi({
    required super.baseUrl,
    required super.frontsDeployment,
    required this.account,
    required this.sessionId,
  });

  final Account account;
  String sessionId;
  int identityCalls = 0;
  int bindCalls = 0;

  @override
  Future<Account?> whoAmI() async {
    identityCalls++;
    return account;
  }

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => [_session(sessionId)];

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async => SessionDetail(session: _session(id), episodes: const []);

  @override
  Future<String?> bindLocalWorkspace(String id, String? path) async {
    bindCalls++;
    return path;
  }
}

class _BlockingWorkspaceApi extends MockCortexApi {
  _BlockingWorkspaceApi({required this.account}) : super(instant: true);

  final Account account;
  final autoBind = Completer<String?>();
  bool autoBindStarted = false;

  ChatSession get session => ChatSession(
    id: 'ses_shared',
    title: 'A 的白纸',
    updatedAt: DateTime.utc(2026, 8, 12),
    isLocalDraft: true,
  );

  @override
  Future<Account?> whoAmI() async => account;

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => [session];

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async => SessionDetail(session: session, episodes: const []);

  @override
  Future<String?> autoBindLocalWorkspace(String id) {
    autoBindStarted = true;
    return autoBind.future;
  }
}

class _BlockingProjectApi extends MockCortexApi {
  _BlockingProjectApi({required this.account}) : super(instant: true);

  final Account account;
  final projectMove = Completer<ChatSession>();
  bool projectMoveStarted = false;

  @override
  Future<Account?> whoAmI() async => account;

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => [_session('ses_seed')];

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async => SessionDetail(session: _session(id), episodes: const []);

  @override
  Future<String?> bindLocalWorkspace(String id, String? path) async => path;

  @override
  Future<ChatSession> moveSessionToProject(
    String sessionId,
    String? projectId,
  ) {
    projectMoveStarted = true;
    return projectMove.future;
  }
}

Account _account(String id) =>
    Account(userId: id, username: id, schemaName: 'schema_$id');

Episode _assistantEpisode(String sessionId, String text) => Episode(
  id: 'epi_$text',
  sessionId: sessionId,
  role: 'assistant',
  text: text,
  occurredAt: DateTime.utc(2026, 8, 12),
);

void main() {
  late _Aborting first;
  late _Working second;
  late bool swapped;

  ProviderContainer boot() {
    first = _Aborting();
    second = _Working();
    swapped = false;
    return ProviderContainer(
      overrides: [
        cortexApiProvider.overrideWith((ref) => swapped ? second : first),
      ],
    );
  }

  /// 让 microtask 队列跑干净 —— `_reload` 与几个 `unawaited` 都排在那里。
  Future<void> settle() async {
    for (var i = 0; i < 8; i++) {
      await Future<void>.delayed(Duration.zero);
    }
  }

  Future<void> swapTo(ProviderContainer container) async {
    swapped = true;
    container.invalidate(cortexApiProvider);
    container.read(cortexApiProvider);
    await settle();
  }

  Future<void> waitFor(bool Function() condition, String reason) async {
    for (var i = 0; i < 200; i++) {
      if (condition()) return;
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    fail('等待超时：$reason');
  }

  test('换后端时，被掐断的会话列表请求不许把错误写回界面', () async {
    final container = boot();
    addTearDown(container.dispose);
    container.read(chatControllerProvider); // 开机就发 /sessions
    await settle();

    await swapTo(container);
    // 旧客户端被 dispose，在飞的请求当场断掉
    first.sessions_.completeError(
      CortexApiException('Connection closed before full header was received'),
    );
    await settle();

    expect(
      container.read(chatControllerProvider).sessionsError,
      isNull,
      reason:
          '那个请求是被我们自己掐断的，不是 cortexd 真的不可用。'
          '把它的异常写进界面，用户看到的是「连不上 cortexd」—— '
          '而 cortexd 好端端地活着',
    );
  });

  test('换后端时，被掐断的消息请求不许把错误写回界面', () async {
    final container = boot();
    addTearDown(container.dispose);
    final notifier = container.read(chatControllerProvider.notifier);

    // 先让会话列表回来，这样首个会话的 sessionDetail 才会发出去
    first.sessions_.complete([_session('s1')]);
    await settle();
    expect(
      container.read(chatControllerProvider).activeSessionId,
      's1',
      reason: '前置条件：拉到列表之后会自动打开第一个会话',
    );

    await swapTo(container);
    first.detail.completeError(
      CortexApiException(
        'Connection closed before full header was received, '
        'uri=http://127.0.0.1:8080/sessions/s1?limit=40',
      ),
    );
    await settle();

    expect(
      container.read(chatControllerProvider).transcripts['s1']?.error,
      isNull,
      reason:
          '这是用户实际看到的那一条：「拉不到这个会话的消息」。'
          '每次冷启动都命中，而唯一的出路是手点重试',
    );
    expect(notifier, isNotNull);
  });

  test('换后端之后自己重新拉一遍，不该让用户去点重试', () async {
    final container = boot();
    addTearDown(container.dispose);
    container.read(chatControllerProvider);
    first.sessions_.complete([_session('s1')]);
    await settle();

    await swapTo(container);

    expect(
      second.sessionCalls,
      greaterThan(0),
      reason: '换到新后端之后要自己重来。不重来的话侧边栏是空的',
    );
    expect(
      second.detailCalls,
      greaterThan(0),
      reason: '消息也要重新拉 —— 否则会话开着，正文区永远是空的',
    );
  });

  test('A 的 send 在工作区 await 后切到 B，不得把同一 session_id 的消息发给 B', () async {
    final account = _account('user-a');
    final first = _BlockingWorkspaceApi(account: account);
    final second = _SubjectApi(account: account, sessionId: 'ses_shared');
    final slot = _ApiSlot(first);
    final container = _bootSlot(slot);
    addTearDown(container.dispose);
    final chat = container.read(chatControllerProvider.notifier);

    await waitFor(
      () =>
          container.read(chatControllerProvider).activeSessionId ==
          'ses_shared',
      'A 打开同一 session_id',
    );

    final sending = chat.send('只属于 A 的消息');
    await waitFor(() => first.autoBindStarted, 'A 进入工作区 await');

    _swapApi(container, slot, second);
    await settle();
    first.autoBind.complete('D:/workspace/a-isolated');

    expect(
      await sending,
      isFalse,
      reason: 'API 与 request generation 都已换代，旧 send 必须拒收',
    );
    final state = container.read(chatControllerProvider);
    expect(state.activeSessionId, 'ses_shared');
    expect(state.streaming, isNull);
    expect(
      state.activeTranscript,
      isEmpty,
      reason: '旧主体的消息不能因为 session_id 相同就写进 B 的转录',
    );
  });

  test('A 的 send 在项目归属 await 后切到 B，不得落到 B 的同一 session_id', () async {
    final first = _BlockingProjectApi(account: _account('user-a'));
    final second = _SubjectApi(
      account: _account('user-b'),
      sessionId: 'ses_seed',
    );
    final slot = _ApiSlot(first);
    final container = _bootSlot(slot);
    addTearDown(container.dispose);
    final chat = container.read(chatControllerProvider.notifier);

    await waitFor(
      () => !container.read(chatControllerProvider).sessionsLoading,
      'A 首屏加载',
    );
    final id = chat.createSession(projectId: 'prj-a');
    await chat.chooseCloud(id);
    second.sessionId = id;

    final sending = chat.send('项目里的消息');
    await waitFor(() => first.projectMoveStarted, 'A 进入项目归属 await');

    _swapApi(container, slot, second);
    await settle();
    first.projectMove.complete(
      ChatSession(
        id: id,
        title: 'A 的草稿',
        updatedAt: DateTime.utc(2026, 8, 12),
        projectId: 'prj-a',
      ),
    );

    expect(await sending, isFalse);
    final state = container.read(chatControllerProvider);
    expect(state.activeSessionId, id, reason: '前置条件：B 也返回同一 id');
    expect(state.streaming, isNull);
    expect(state.activeTranscript, isEmpty);
  });

  test('换账号时清掉同 session_id 的待建会话、项目、云端选择和会话助手', () async {
    final first = _SubjectApi(
      account: _account('user-a'),
      sessionId: 'ses_seed',
    );
    final second = _SubjectApi(
      account: _account('user-b'),
      sessionId: 'ses_seed',
    );
    final slot = _ApiSlot(first);
    final container = _bootSlot(slot);
    addTearDown(container.dispose);
    final chat = container.read(chatControllerProvider.notifier);

    await settle();
    expect(first.identityCalls, greaterThan(0));
    final id = chat.createSession(projectId: 'prj-a');
    await chat.chooseCloud(id);
    container.read(sessionAssistantProvider.notifier).bind(id, 'assistant-a');
    chat.startNewChat(projectId: 'prj-pending-a', assistantId: 'assistant-a');
    second.sessionId = id;

    _swapApi(container, slot, second);
    await waitFor(
      () =>
          second.identityCalls > 0 &&
          container.read(chatControllerProvider).pendingNewChat == null,
      'B 主体解析并清理 A 的白纸上下文',
    );
    await settle();

    final state = container.read(chatControllerProvider);
    expect(state.pendingNewChat, isNull);
    expect(
      chat.pendingProjectForTest(id),
      isNull,
      reason: 'A 的项目待办不能挂到 B 的同 id 会话上',
    );
    expect(
      chat.isCloudByChoice(id),
      isFalse,
      reason: '云端选择是本地内存，不会随 B 的会话详情自动清掉',
    );
    expect(container.read(sessionAssistantProvider), isEmpty);
  });

  test('B 的主体复核完成前，send 不得消费 A 的同一 session_id 内存状态', () async {
    final first = _SubjectApi(
      account: _account('user-a'),
      sessionId: 'ses_seed',
    );
    final identityGate = Completer<Account?>();
    final second = _SubjectApi(
      account: _account('user-b'),
      sessionId: 'ses_seed',
      identityGate: identityGate,
    );
    final slot = _ApiSlot(first);
    final container = _bootSlot(slot);
    addTearDown(container.dispose);
    final chat = container.read(chatControllerProvider.notifier);

    await settle();
    final id = chat.createSession(projectId: 'prj-a');
    await chat.chooseCloud(id);
    container.read(sessionAssistantProvider.notifier).bind(id, 'assistant-a');
    chat.startNewChat(projectId: 'prj-pending-a', assistantId: 'assistant-a');
    second.sessionId = id;

    _swapApi(container, slot, second);
    await waitFor(
      () =>
          second.identityCalls > 0 &&
          !container.read(chatControllerProvider).sessionsLoading,
      'B 载入同一 id，但身份仍未返回',
    );

    final sending = chat.send('只属于 B 的消息');
    await settle();
    expect(
      container.read(chatControllerProvider).streaming,
      isNull,
      reason: '身份未定时不得先调用 B.chat',
    );
    expect(container.read(chatControllerProvider).activeTranscript, isEmpty);
    expect(
      chat.pendingProjectForTest(id),
      'prj-a',
      reason: '复核返回前，旧状态还未被错误消费或清除',
    );

    identityGate.complete(_account('user-b'));
    expect(await sending, isTrue);
    await waitFor(
      () => container.read(chatControllerProvider).pendingNewChat == null,
      'B 身份返回后清掉 A 的白纸上下文',
    );

    expect(chat.pendingProjectForTest(id), isNull);
    expect(chat.isCloudByChoice(id), isFalse);
    expect(container.read(sessionAssistantProvider), isEmpty);
    expect(
      container.read(chatControllerProvider).streaming,
      isNotNull,
      reason: '清理完成后，B 自己的 send 才继续',
    );
  });

  test('同账号 token 续期更换 API 实例时保留主体内的本地状态', () async {
    final account = _account('user-a');
    final first = _SubjectApi(account: account, sessionId: 'ses_seed');
    final renewed = _SubjectApi(account: account, sessionId: 'ses_seed');
    final slot = _ApiSlot(first);
    final container = _bootSlot(slot);
    addTearDown(container.dispose);
    final chat = container.read(chatControllerProvider.notifier);

    await settle();
    final id = chat.createSession(projectId: 'prj-a');
    await chat.chooseCloud(id);
    container.read(sessionAssistantProvider.notifier).bind(id, 'assistant-a');
    chat.startNewChat(projectId: 'prj-pending-a', assistantId: 'assistant-a');
    renewed.sessionId = id;

    _swapApi(container, slot, renewed);
    await waitFor(() => renewed.identityCalls > 0, '续期后的 API 身份解析');
    await settle();

    final state = container.read(chatControllerProvider);
    expect(state.pendingNewChat?.projectId, 'prj-pending-a');
    expect(chat.pendingProjectForTest(id), 'prj-a');
    expect(chat.isCloudByChoice(id), isTrue);
    expect(container.read(sessionAssistantProvider)[id], 'assistant-a');
  });

  test('同账号 token 续期不清空当前会话和已经加载的转录', () async {
    final account = _account('user-a');
    final first = _SubjectApi(
      account: account,
      sessionId: 'ses_seed',
      episodes: [_assistantEpisode('ses_seed', '续期前的历史回答')],
    );
    final renewed = _SubjectApi(
      account: account,
      sessionId: 'ses_seed',
      episodes: [_assistantEpisode('ses_seed', '续期前的历史回答')],
    );
    final slot = _ApiSlot(first);
    final container = _bootSlot(slot);
    addTearDown(container.dispose);

    await waitFor(
      () =>
          container.read(chatControllerProvider).activeSessionId ==
              'ses_seed' &&
          container
                  .read(chatControllerProvider)
                  .transcripts['ses_seed']
                  ?.messages
                  .isNotEmpty ==
              true,
      '首屏加载当前会话与转录',
    );
    final before = container.read(chatControllerProvider);
    final originalMessages = before.transcripts['ses_seed']!.messages;

    _swapApi(container, slot, renewed);
    await waitFor(
      () =>
          renewed.identityCalls > 0 &&
          !container.read(chatControllerProvider).sessionsLoading,
      '同账号续期完成主体复核与会话刷新',
    );
    await settle();

    final after = container.read(chatControllerProvider);
    expect(after.activeSessionId, 'ses_seed');
    expect(
      after.transcripts['ses_seed']!.messages,
      same(originalMessages),
      reason: '同主体续期不应把已经加载的 transcript 换成空集合',
    );
    expect(after.transcripts['ses_seed']!.messages.single.text, '续期前的历史回答');
  });

  test('换账号时同一 session_id 的 A 转录不得留在界面上', () async {
    const id = 'ses_shared';
    final first = _SubjectApi(
      account: _account('user-a'),
      sessionId: id,
      episodes: [_assistantEpisode(id, '只属于 A 的回答')],
    );
    final second = _SubjectApi(
      account: _account('user-b'),
      sessionId: id,
      episodes: [_assistantEpisode(id, '只属于 B 的回答')],
    );
    final slot = _ApiSlot(first);
    final container = _bootSlot(slot);
    addTearDown(container.dispose);

    await waitFor(
      () =>
          container
              .read(chatControllerProvider)
              .transcripts[id]
              ?.messages
              .isNotEmpty ==
          true,
      'A 的转录先落地',
    );

    _swapApi(container, slot, second);
    await waitFor(
      () =>
          second.identityCalls > 0 &&
          container
                  .read(chatControllerProvider)
                  .transcripts[id]
                  ?.messages
                  .single
                  .text ==
              '只属于 B 的回答',
      'B 的主体边界清理并加载自己的转录',
    );

    final text = container
        .read(chatControllerProvider)
        .transcripts[id]!
        .messages
        .single
        .text;
    expect(text, isNot(contains('A')));
  });

  test('同账号续期时旧流事件丢弃，并从新 API 重挂正在跑的轮次', () async {
    const id = 'ses_running';
    final firstSent = StreamController<ChatEvent>.broadcast();
    final renewedAttached = StreamController<ChatEvent>.broadcast();
    addTearDown(firstSent.close);
    addTearDown(renewedAttached.close);

    final account = _account('user-a');
    final first = _SubjectApi(
      account: account,
      sessionId: id,
      sentStream: firstSent.stream,
    );
    final renewed = _SubjectApi(
      account: account,
      sessionId: id,
      attachStream: renewedAttached.stream,
    );
    final slot = _ApiSlot(first);
    final container = _bootSlot(slot);
    addTearDown(container.dispose);
    final chat = container.read(chatControllerProvider.notifier);

    await waitFor(
      () => container.read(chatControllerProvider).activeSessionId == id,
      '打开正在运行的会话',
    );
    expect(await chat.send('继续做'), isTrue);
    firstSent.add(const ChatDeltaEvent('旧流已收到的半句'));
    await settle();
    final oldTurn = container.read(chatControllerProvider).streaming!;

    _swapApi(container, slot, renewed);
    await waitFor(
      () => renewed.identityCalls > 0 && renewed.attachCalls > 0,
      '续期后重挂旧轮次',
    );

    firstSent.add(const ChatDeltaEvent('旧客户端迟到的事件'));
    renewedAttached.add(const ChatDeltaEvent('重挂后重放的事件'));
    await waitFor(
      () =>
          container
              .read(chatControllerProvider)
              .streamingTurns[id]
              ?.text
              .contains('重挂后重放的事件') ==
          true,
      '重挂后的增量刷到界面',
    );

    final recovered = container
        .read(chatControllerProvider)
        .streamingTurns[id]!;
    expect(recovered.messageId, oldTurn.messageId);
    expect(recovered.text, contains('重挂后重放的事件'));
    expect(
      recovered.text,
      isNot(contains('旧客户端迟到的事件')),
      reason: '旧 API 的迟到事件不能借同一账号和 session_id 重新写进界面',
    );

    renewedAttached.add(const ChatDoneEvent('epi_recovered'));
    await settle();
    final state = container.read(chatControllerProvider);
    expect(state.streamingTurns[id], isNull);
    expect(state.transcripts[id]!.messages.last.text, contains('重挂后重放的事件'));
  });

  test('同账号切部署时仍清掉同 session_id 的主体内存状态', () async {
    final account = _account('user-a');
    final first = _HttpSubjectApi(
      baseUrl: 'https://a.example/',
      frontsDeployment: 'https://deploy-a.example/',
      account: account,
      sessionId: 'ses_seed',
    );
    final second = _HttpSubjectApi(
      baseUrl: 'https://b.example/',
      frontsDeployment: 'https://deploy-b.example/',
      account: account,
      sessionId: 'ses_seed',
    );
    addTearDown(first.dispose);
    addTearDown(second.dispose);
    final slot = _ApiSlot(first);
    final container = _bootSlot(slot);
    addTearDown(container.dispose);
    final chat = container.read(chatControllerProvider.notifier);

    await settle();
    final id = chat.createSession(projectId: 'prj-a');
    await chat.chooseCloud(id);
    container.read(sessionAssistantProvider.notifier).bind(id, 'assistant-a');
    chat.startNewChat(projectId: 'prj-pending-a');
    second.sessionId = id;

    _swapApi(container, slot, second);
    await waitFor(
      () =>
          second.identityCalls > 0 &&
          container.read(chatControllerProvider).pendingNewChat == null,
      '不同部署的主体解析并清理',
    );
    await settle();

    expect(chat.pendingProjectForTest(id), isNull);
    expect(chat.isCloudByChoice(id), isFalse);
    expect(container.read(sessionAssistantProvider), isEmpty);
  });
}
