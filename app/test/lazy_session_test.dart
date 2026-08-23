/// 「新对话」不再当场建会话 —— 真开口了才建。
///
/// # 这一组守着什么
///
/// 2026-08-23 之前点一下「新聊天」就往左栏塞一条草稿，于是每次「我看看能
/// 干什么」都留下一行自己从没说过话的对话。惰性化把建会话的时刻推到了
/// 第一条消息（`ChatController.send` 里那个 `?? materializeSession()`）。
///
/// 这个改动有**三个不报错的失败面**，每一条都在下面有对应的用例：
///
/// 1. 白纸被别处顶掉 —— `loadSessions` 的「没有当前会话就挑第一条」，
///    以及 `_reload` 把整个 state 换新。两者都会把用户扔回上一条会话，
///    而他正打字打到一半。
/// 2. 上下文丢了 —— 从项目里点的新对话，建出来却落在项目外面。
/// 3. 建重了 —— 每次 `materializeSession` 都新开一条。
library;

import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot() {
  final c = ProviderContainer(
    overrides: [appConfigProvider.overrideWith(_MockConfig.new)],
  );
  c.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return c;
}

/// 把「开机拉一次列表」跑完。
///
/// ⚠️ 等**条件**而不是数轮次：数 microtask 的写法在 mock 慢一点时就会
/// 静默地跑在列表还没回来的时候，红在「sessions 是空的」上 ——
/// 而那条报错一个字都没提真正的原因。
Future<void> _settle(ProviderContainer c) async {
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (c.read(chatControllerProvider).sessionsLoading) {
    if (DateTime.now().isAfter(deadline)) fail('等开机那次会话列表超时');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

void main() {
  group('新对话是一张白纸', () {
    test('点新对话不建会话，左栏一行都不多', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);

      final before = c.read(chatControllerProvider).sessions.length;
      c.read(chatControllerProvider.notifier).startNewChat();

      final st = c.read(chatControllerProvider);
      expect(
        st.sessions.length,
        before,
        reason: '点「新对话」只是清空当前选中 —— 多出一行就是从前那个「一列空对话」的毛病',
      );
      expect(st.activeSessionId, isNull, reason: '白纸的定义就是没有当前会话');
    });

    test('⚠️ 刷新不能把白纸顶掉', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);
      // mock 后端有会话，所以「挑第一条」这条路是活的
      expect(c.read(chatControllerProvider).sessions, isNotEmpty);

      c.read(chatControllerProvider.notifier).startNewChat();
      // WebSocket 推一下、别的设备改了什么，都会走到这里
      await c.read(chatControllerProvider.notifier).loadSessions();

      expect(
        c.read(chatControllerProvider).activeSessionId,
        isNull,
        // `loadSessions` 里原先是 `activeSessionId ?? merged.first.id`。
        // 惰性化之后「没有当前会话」多了第二种含义（用户要的白纸），
        // 不分辨的话每一次刷新都会把他拉回上一条会话
        reason: '刷新把用户从他正要开的新对话里拽了回去，而他什么都没点',
      );
    });

    test('⚠️ 换后端/首次加载也不能把白纸顶掉', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);

      c.read(chatControllerProvider.notifier).startNewChat(projectId: 'p1');
      // `_reload` 会把整个 ChatState 换新。它在换数据源时跑，也在
      // `build()` 末尾的 microtask 里跑 —— 后者与「从智能体卡片点进来」
      // 只隔着一个 microtask，实测撞到过
      await c.read(chatControllerProvider.notifier).loadSessions();

      final st = c.read(chatControllerProvider);
      expect(st.activeSessionId, isNull);
      expect(
        st.pendingNewChat?.projectId,
        'p1',
        reason: '「我正要在这个项目里开一条新对话」是用户的意图，不是后端数据',
      );
    });

    test('从项目里点的新对话，建出来落在那个项目下', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);

      final chat = c.read(chatControllerProvider.notifier);
      chat.startNewChat(projectId: 'proj-42');
      final id = chat.materializeSession();

      final made = c
          .read(chatControllerProvider)
          .sessions
          .firstWhere((s) => s.id == id);
      expect(
        made.projectId,
        'proj-42',
        // 丢了的表现是对话建在项目外面，而用户是从项目里点进来的 ——
        // 没有任何报错，他只会觉得「怎么不见了」
        reason: '白纸攒着的项目归属必须跟着兑现',
      );
    });

    test('兑现是幂等的 —— 已经有会话时不再新开', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);

      final chat = c.read(chatControllerProvider.notifier);
      chat.startNewChat();
      final first = chat.materializeSession();
      final count = c.read(chatControllerProvider).sessions.length;

      final again = chat.materializeSession();
      expect(again, first, reason: '同一张白纸只该兑现成一条会话');
      expect(
        c.read(chatControllerProvider).sessions.length,
        count,
        reason: '再兑现一次又多一条的话，粘贴一张图就会凭空多出一条对话',
      );
    });

    test('兑现之后白纸清掉，不会把上下文带给下一条', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);

      final chat = c.read(chatControllerProvider.notifier);
      chat.startNewChat(projectId: 'proj-42');
      chat.materializeSession();

      expect(
        c.read(chatControllerProvider).pendingNewChat,
        isNull,
        // 留着的话，下一次「新对话」会莫名其妙地落进 proj-42
        reason: '白纸兑现完就该扔掉，否则这一次的项目归属会安到下一条会话头上',
      );
    });

    test('点开别的会话，白纸作废', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);
      final existing = c.read(chatControllerProvider).sessions.first.id;

      final chat = c.read(chatControllerProvider.notifier);
      chat.startNewChat(projectId: 'proj-42');
      chat.selectSession(existing);

      expect(
        c.read(chatControllerProvider).pendingNewChat,
        isNull,
        reason: '点开别的会话就是不要那张白纸了 —— 攒着的项目/人设跟着作废',
      );
    });
  });
}
