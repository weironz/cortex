/// 一条挂着的聊天流必须自己走到尽头。
///
/// # 案发现场（2026-08-29）
///
/// 生产在 IO 风暴里，一条聊天 SSE **既不吐事件也不报错**。于是
/// `ChatState.streaming` 永久占线，「一次只跑一轮」那道闸从此拦下所有后续
/// 发送 —— 用户唯一的出路是重启 app。8fada8d 让拒收出了声（消息不再凭空
/// 消失），但流本身仍然没有尽头：那半在这里补。
///
/// # 这一组测试守的是**两个方向**
///
/// 挂住了要断（否则占线永远解不开），而正在干活的一轮**不许**被断
/// （否则这条看门狗自己变成新的故障源）。后者才是难的那一半：数据帧之间
/// 可以合法地静默十分钟 —— agent 在跑一条长命令、这一轮排在别人后面、
/// 或者停在一个等人点的确认上。所以判据是**心跳**，不是「多久没有回答」。
library;

import 'dart:async';
import 'dart:convert';

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

/// 故障注入用的后端：这一轮的流**由测试逐条喂**，而且永不自己结束。
///
/// `MockCortexApi(instant: true)` 那一轮会在下一个事件循环就跑完，读到的
/// 永远是终态 —— 而这里要验的恰恰是「跑着跑着不动了」。
class _StalledApi extends MockCortexApi {
  _StalledApi() : super(instant: true);

  final sent = StreamController<ChatEvent>.broadcast();

  /// 重挂那条路的流。与 [sent] 分开：重挂验的是「还没有 streaming 状态」
  /// 那一段，混用会把两条路的事件搅在一起。
  final attached = StreamController<ChatEvent>.broadcast();

  @override
  Stream<ChatEvent> attachChat(String sessionId) => attached.stream;

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
  }) => sent.stream;
}

void main() {
  Future<(ProviderContainer, ChatController, _StalledApi)> boot(
    WidgetTester tester,
  ) async {
    final api = _StalledApi();
    final c = ProviderContainer(
      overrides: [cortexApiProvider.overrideWithValue(api)],
    );
    final ctrl = c.read(chatControllerProvider.notifier);
    // 开机那次会话列表（`instant: true`，一个 microtask 就回来了）
    await tester.pump();
    return (c, ctrl, api);
  }

  group('空转看门狗', () {
    // 用 testWidgets 而不是 test：看门狗是一个 `Timer`，`tester.pump(d)` 推的
    // 正是它所在的那口假时钟。写成普通 test 就得真的等 75 秒
    testWidgets('连上了却不吐东西 —— 到点自己断开，并把占线解开', (tester) async {
      final (c, ctrl, api) = await boot(tester);

      ctrl.createSession();
      expect(await ctrl.send('跑一条会挂住的'), isTrue);
      // 先回了一句，然后再也不出声 —— 与真实现场一致：连接是好的、
      // TCP 没断、服务端那一轮还挂在 IO 上
      api.sent.add(const ChatDeltaEvent('好的，我先看一下'));
      await tester.pump();
      expect(
        c.read(chatControllerProvider).streaming,
        isNotNull,
        reason: '前置不成立：流根本没开起来，后面验不到「挂住」',
      );

      // 差一秒到点：**不许断**。慢不是死，早一秒开口就是误杀
      await tester.pump(
        ChatController.idleTimeout - const Duration(seconds: 1),
      );
      expect(
        c.read(chatControllerProvider).streaming,
        isNotNull,
        reason: '没到点就断，等于把「正在跑一条长命令」判成故障',
      );

      await tester.pump(const Duration(seconds: 2));

      final s = c.read(chatControllerProvider);
      expect(s.streaming, isNull, reason: '这一条就是整块的意义：占线必须自己解开，而不是靠重启 app');

      // 再发一条能被收下 —— 「闸从此拦下所有后续发送」那个症状没了
      expect(
        await ctrl.send('第二条现在该发得出去'),
        isTrue,
        reason: '解开占线之后还发不出去的话，这条看门狗白装了',
      );
      // 收掉第二轮再拆容器：留着的话，那条 75 秒的表会活过 test body，
      // 被 flutter_test 判成「还有 timer 没清」（那个检查跑在 teardown 之前）
      api.sent.add(const ChatDoneEvent(null));
      await tester.pump();
      c.dispose();
    });

    testWidgets('一个字节都没回过的流也有尽头 —— 表从发出去那一刻就开始走', (tester) async {
      // 与上一条的差别只有一个：这条流**一条事件都没有过**。看门狗若只在
      // 收到第一条事件时才上表，这一支就永远没人管 —— 而它恰恰是最像
      // 2026-08-29 现场的那一支（连上了，然后什么都不发生）
      final (c, ctrl, _) = await boot(tester);

      ctrl.createSession();
      expect(await ctrl.send('这条连回音都没有'), isTrue);
      await tester.pump(
        ChatController.idleTimeout + const Duration(seconds: 1),
      );

      expect(
        c.read(chatControllerProvider).streaming,
        isNull,
        reason: '表要从「发出去」那一刻开始走，不是从第一条事件开始走',
      );
      c.dispose();
    });

    testWidgets('断开时说的是实话：文字留着、服务端那一轮还在跑', (tester) async {
      final (c, ctrl, api) = await boot(tester);

      final id = ctrl.createSession();
      await ctrl.send('问一句');
      api.sent.add(const ChatDeltaEvent('已经流回来的这半句'));
      await tester.pump();
      await tester.pump(
        ChatController.idleTimeout + const Duration(seconds: 1),
      );

      final message = c
          .read(chatControllerProvider)
          .transcripts[id]!
          .messages
          .last;
      expect(
        message.text,
        contains('已经流回来的这半句'),
        reason: '断开不该把已经收到的文字扔掉 —— 它就在用户眼前',
      );
      final error = message.error ?? '';
      expect(error, contains('空转'), reason: '要说清是「没人说话」而不是「服务端报错了」—— 两者的下一步不同');
      expect(error, contains('心跳'), reason: '判据要写出来：没有心跳才是死了，没有回答不是');
      expect(error, contains('仍在继续'), reason: '不说这句的话用户会重发一遍，然后得到两份回答');
      c.dispose();
    });

    testWidgets('只有心跳的五分钟不许被判死 —— agent 在跑长命令', (tester) async {
      final (c, ctrl, api) = await boot(tester);

      ctrl.createSession();
      await ctrl.send('跑一条十分钟的命令');
      api.sent.add(
        const ChatToolEvent(
          name: 'shell',
          summary: 'cargo build',
          phase: ToolPhase.call,
        ),
      );
      await tester.pump();

      // 服务端每 15 秒一条 `: ping`，五分钟里**只有**它。这正是长命令期间
      // 这条流上真实的样子 —— 把它判死是这条看门狗最容易犯的错
      for (var i = 0; i < 20; i++) {
        await tester.pump(const Duration(seconds: 15));
        api.sent.add(const ChatHeartbeatEvent());
        await tester.pump();
      }
      expect(
        c.read(chatControllerProvider).streaming,
        isNotNull,
        reason: '心跳还在就是活着。断了它，用户看到的是一轮跑到一半凭空失败',
      );

      // 命令跑完，这一轮照常收尾
      api.sent.add(const ChatDeltaEvent('编完了'));
      api.sent.add(const ChatDoneEvent('ep-1'));
      await tester.pump();
      expect(c.read(chatControllerProvider).streaming, isNull);
      c.dispose();
    });

    test('阈值要留得下抖动，也要让代理先开口', () {
      // 服务端每 15 秒一条 ping（`cortex-local` 的 `routes::sse`）。
      // 下界与 agentd `sandbox_proxy` 那条
      // `the_read_timeout_leaves_room_for_the_keepalive_ping` 算的是同一笔帐
      const ping = Duration(seconds: 15);
      expect(
        ChatController.idleTimeout,
        greaterThanOrEqualTo(ping * 3),
        reason:
            '阈值贴着 ping 间隔的话，一次 GC 停顿或网络抖动就会把正在干活的'
            '一轮判死 —— 这条看门狗于是自己成了新的故障源',
      );
      // agentd 的 READ_TIMEOUT 是 60 秒。云端那条路上僵死时该先由代理发现
      // 并给出说得清是谁的错的报错；抢在它前面开口只会把它盖掉
      expect(
        ChatController.idleTimeout,
        greaterThan(const Duration(seconds: 60)),
        reason:
            '早于 agentd 的读超时开口，用户拿到的是一句更含糊的「空转」，'
            '而真正的原因（沙箱僵死）被盖住了',
      );
    });
    testWidgets('重挂：只有心跳不算「它在跑」—— 不转圈，也不落空气泡', (tester) async {
      final (c, ctrl, api) = await boot(tester);

      // ⚠️ 建**两条**再切回来：`createSession` 已经把新建的那条设成当前，
      // 而 `selectSession` 对同一个 id 直接 return —— 只建一条的话根本
      // 走不到 `_tryAttach`（第一版就是这样，测试因此是空的）
      final id = ctrl.createSession();
      ctrl.createSession();
      ctrl.selectSession(id);
      // `_tryAttach` 是 unawaited 的：等它真的订上再喂，别数轮次
      for (var i = 0; i < 10 && !api.attached.hasListener; i++) {
        await tester.pump();
      }
      // **正对照**：`_tryAttach` 是 unawaited 的，而 `attached` 是广播流 ——
      // 没订上就 add，事件被静默丢弃，这条测试会变成一条永远绿的空测。
      // （第一版就是这样：把被测的判断整个去掉，它照样通过。）
      expect(
        api.attached.hasListener,
        isTrue,
        reason: '重挂还没订上流，下面喂的心跳会被广播流丢掉 —— 这条验不到东西',
      );

      // 重挂连上了，但这条 run 只吐心跳、永远没有内容
      for (var i = 0; i < 3; i++) {
        api.attached.add(const ChatHeartbeatEvent());
        await tester.pump(const Duration(seconds: 15));
      }
      expect(
        c.read(chatControllerProvider).streaming,
        isNull,
        reason:
            '心跳只说明连接开着，说明不了这一轮有内容。拿它提升 started 的话，'
            '界面会为一条永不产出的 run 转圈',
      );

      // 到点之后那条连接被静悄悄收掉，转录里不许多出一条空气泡
      await tester.pump(ChatController.idleTimeout);
      await tester.pump();
      final s = c.read(chatControllerProvider);
      expect(s.streaming, isNull);
      expect(
        (s.transcripts[id]?.messages ?? const []).where((m) => m.text.isEmpty),
        isEmpty,
        reason: '纯心跳的重挂不该在转录里落下一条空的助手消息',
      );
      c.dispose();
    });
  });

  group('心跳一路走到 controller', () {
    test('SSE 的 `: ping` 变成 ChatHeartbeatEvent', () async {
      // 整条链最容易断的一环：解析层默认按规范把注释丢掉，丢了的话
      // 上面那条看门狗就只剩「多久没有 delta」可判
      final client = MockClient.streaming((request, _) async {
        return http.StreamedResponse(
          Stream.fromIterable(
            [
              ':ping\n\n',
              'data: {"type":"delta","text":"嗨"}\n\n',
              ': ping\n\n',
              'data: {"type":"done"}\n\n',
            ].map(utf8.encode),
          ),
          200,
          request: request,
          headers: const {'content-type': 'text/event-stream'},
        );
      });
      final api = HttpCortexApi(baseUrl: 'http://127.0.0.1:1', client: client);
      addTearDown(api.dispose);

      final events = await api.chat(sessionId: 'S1', message: 'hi').toList();
      expect(
        events.whereType<ChatHeartbeatEvent>(),
        hasLength(2),
        reason: '心跳被丢在解析层的话，判活就没有证据了',
      );
      expect(
        events.whereType<ChatDeltaEvent>().single.text,
        '嗨',
        reason: '浮出心跳不该动到数据帧本身',
      );
      expect(events.last, isA<ChatDoneEvent>());
    });
  });
}
