/// 本机 agent **不许被反复重启**。
///
/// # 这是哪次事故
///
/// 2026-08-21：用户报「dev 桌面端总是连不上 agent、看不到会话」。
/// 启动记录里 13.7 分钟内 **730 对 spawn/ready**，周期稳定 1.13 秒，
/// 没有一条 exit —— 也就是说 agent 一次都没崩，是**我们在杀它**。
///
/// 环路：远端凭据失效 → 请求 401 → 客户端把任何 401 都当成「本机 agent
/// 凭据错位」→ `invalidate(localAgentOriginProvider)` → agent 被停掉重拉
/// → `ConfirmController` 监听到 api 重建、调 `recover()` 拉一次待确认
/// → 又 401 → 再杀。重启治不了远端，于是永动。
///
/// 这里钉的是让那个环路**跑不起来**的三个条件。它们全是「改坏了不报错、
/// 只是又开始转」的那一类 —— 而转起来的表现是用户看不到会话。
library;

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

void main() {
  group('重启预算：最后一道防线', () {
    /// 连着敲，看第几次被拦下。
    List<bool> burst(int times, {Duration gap = const Duration(seconds: 1)}) {
      final out = <bool>[];
      var now = DateTime(2026, 8, 21, 9);
      DateTime? last;
      var kicks = 0;
      for (var i = 0; i < times; i++) {
        final r = allowRestartKick(now: now, lastKick: last, kicks: kicks);
        out.add(r.allowed);
        last = now;
        kicks = r.kicks;
        now = now.add(gap);
      }
      return out;
    }

    test('窗口内最多放行 2 次', () {
      expect(
        burst(5),
        [true, true, false, false, false],
        reason:
            '上游那道「谁拒的」判据 2026-08-21 之前是错的，后果是 13.7 分钟'
            '杀了 730 次。判据可以再错，而这里保证**错的后果有上界** —— '
            '坏也只坏成「重启两次没用」，不是永动机',
      );
    });

    test('隔得久了重新计数', () {
      expect(
        burst(4, gap: const Duration(minutes: 5)),
        [true, true, true, true],
        reason:
            '真的隔了五分钟又错位一次，那是**新的一次故障**，'
            '不该被上一次的额度拖累 —— 否则用一天就永远修不了了',
      );
    });

    test('第一次永远放行', () {
      final r = allowRestartKick(
        now: DateTime(2026, 8, 21),
        lastKick: null,
        kicks: 0,
      );
      expect(r.allowed, isTrue, reason: '凭据错位重启一次就该好 —— 连第一次都拦掉的话，这个自愈动作等于不存在');
    });
  });

  group('401 按「谁拒的」分流', () {
    /// 起一个只会答 401 的后端，记下两个铃各响了几次。
    Future<({int remote, int localAgent})> ring({
      Map<String, String> responseHeaders = const {},
    }) async {
      var remote = 0;
      var localAgent = 0;
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:1',
        token: 'x',
        client: MockClient(
          (_) async => http.Response('nope', 401, headers: responseHeaders),
        ),
        onUnauthorized: () => remote++,
        onLocalAgentRejected: () => localAgent++,
      );
      try {
        await api.sessions();
      } on Object {
        // 401 必然抛，这里只关心铃
      }
      // 铃走 scheduleMicrotask，让出一次事件循环才数得到
      await Future<void>.delayed(Duration.zero);
      return (remote: remote, localAgent: localAgent);
    }

    test('agent 自己拒的（带 x-cortex-denied-by）只敲本机那只铃', () async {
      final r = await ring(
        responseHeaders: {'x-cortex-denied-by': 'local-agent'},
      );
      expect(
        (r.localAgent, r.remote),
        (1, 0),
        reason:
            '入站凭据错位重启一次就该好。敲错铃会把用户登出 —— '
            '而他的远端凭据完全有效',
      );
    });

    test('远端转回来的 401（没有那个头）只敲登录态那只铃', () async {
      final r = await ring();
      expect(
        (r.localAgent, r.remote),
        (0, 1),
        reason:
            '**这一条就是 2026-08-21 那次事故**：远端凭据失效的 401 被当成'
            '本机错位，于是每次轮询都重启一次 agent，13.7 分钟杀了 730 次。'
            '重启治不了远端 —— 走错这条铃，环路就会重新转起来',
      );
    });

    test('别人自称的 denied-by 不算数', () async {
      final r = await ring(responseHeaders: {'x-cortex-denied-by': '别的什么'});
      expect(
        (r.localAgent, r.remote),
        (0, 1),
        reason: '只认 local-agent 这一个值 —— 判据松一点，环路就多一条回来的路',
      );
    });
  });
}
