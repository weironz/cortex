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
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/models/session_detail.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

ChatSession _session(String id) => ChatSession(
  id: id,
  title: '会话 $id',
  updatedAt: DateTime.utc(2026, 8, 12),
);

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
  Future<SessionDetail> sessionDetail(String id, {int? limit, String? before}) =>
      detail.future;
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
      reason: '那个请求是被我们自己掐断的，不是 cortexd 真的不可用。'
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
      reason: '这是用户实际看到的那一条：「拉不到这个会话的消息」。'
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
}
