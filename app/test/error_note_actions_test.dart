/// 「这一轮失败了」那个红框里给不给出路按钮。
///
/// # 为什么这值得一组测试
///
/// 「重试」与「换模型」是给**这个模型这一次不行**准备的出路（配额、超时、
/// 供应商挂了）—— 那些情况下它们真的管用，所以不能一刀切掉。
///
/// 但有一类失败它们一件都帮不上：会话钉在一台关着的电脑上。重发一万次是
/// 同一句话，换个模型也不会把那台电脑唤醒。**一个摆在那儿的按钮本身就在
/// 说「点我可能有用」** —— 用户点完还是同一句话，于是开始怀疑是不是自己
/// 网不好，而真正该做的事（把那台电脑唤醒）反被这两个按钮挡住了视线。
///
/// 判据由服务端给（`ErrorBody.retryable == false`），客户端不猜 —— 409 在
/// 这条路上身兼数职，按状态码或文案关键词猜是「改一个字就静默失效」的判据。
library;

import 'dart:convert';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 一条失败的回答。`deterministic` = 服务端说了「重发没用」。
ChatMessage _failed({required bool deterministic}) => ChatMessage(
  id: 'a1',
  role: MessageRole.assistant,
  text: '',
  createdAt: DateTime.utc(2026, 8, 27),
  error: '这个会话绑在 WILLOPTPC 上的一个目录里。\n那台机器刚才还连着，现在断开了。',
  errorIsDeterministic: deterministic,
);

Future<void> _pump(WidgetTester tester, ChatMessage msg) async {
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
      ],
      child: MaterialApp(
        home: Scaffold(
          body: SizedBox(
            width: 900,
            child: SingleChildScrollView(
              // retryTarget 给上 —— 这样「重试」的另一个前提成立，
              // 测出来的差别就只来自 deterministic 这一位
              child: MessageBubble(message: msg, retryTarget: 'u1'),
            ),
          ),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 100));
}

void main() {
  group('确定性失败不给出路按钮', () {
    testWidgets('机器够不着这类失败：两个按钮都不画', (tester) async {
      await _pump(tester, _failed(deterministic: true));

      expect(
        find.text('重试'),
        findsNothing,
        reason:
            '那台机器不上线之前重发一万次是同一句话 —— 摆一个确定失败的按钮，'
            '用户点完会怀疑是自己网不好',
      );
      expect(
        find.text('换模型'),
        findsNothing,
        reason: '失败的原因是文件在一台关着的电脑上，换个模型不会把它唤醒',
      );
      // 那三行说明本身才是出路，必须还在
      expect(find.textContaining('WILLOPTPC'), findsOneWidget);
    });

    /// ⚠️ **反方向同样要钉。** 只测「该消失时消失」的话，
    /// 一刀切掉两个按钮也能让上面那条绿 —— 而那会把配额、超时、
    /// 供应商挂了这些**真的能靠换模型走通**的情况一起废掉。
    testWidgets('普通失败：两个按钮照旧', (tester) async {
      await _pump(tester, _failed(deterministic: false));

      expect(
        find.text('重试'),
        findsOneWidget,
        reason: '普通失败重发可能就好了 —— 这条出路不能跟着一起没',
      );
      expect(
        find.text('换模型'),
        findsOneWidget,
        reason: '失败往往是这个模型的事（配额、超时、看不懂图），换一个立刻能走',
      );
    });
  });

  group('这一位从服务端来，不在客户端猜', () {
    test('没说的时候按「可能有用」对待', () {
      const plain = CortexApiException('出错了', statusCode: 500);
      expect(
        plain.isDeterministic,
        isFalse,
        reason:
            '默认必须是「给出路」—— 反过来的话，服务端漏说一次就把'
            '一整类失败的出路全砍了',
      );
    });

    test('服务端说了 false 才算确定性失败', () {
      const said = CortexApiException(
        '机器离线',
        statusCode: 409,
        retryable: false,
      );
      expect(said.isDeterministic, isTrue);
    });

    /// ⚠️ 409 **不能**当判据：同一个码在这条路上身兼数职 ——
    /// 「沙箱刚被回收了」（重发会把它拉起来）、「那一端认着上一代令牌」
    /// （下一轮换掉）、「你的会话钉在一台关着的电脑上」（没救）。
    test('光看 409 不算数', () {
      const conflict = CortexApiException('沙箱刚被回收了', statusCode: 409);
      expect(
        conflict.isDeterministic,
        isFalse,
        reason: '按状态码猜的话，「沙箱被回收了，重发会拉起来」也会被误判成没救',
      );
    });
  });

  /// ⚠️ **接线本身要有测试。**
  ///
  /// 上面那两条 widget 测试直接构造 `ChatMessage`，所以「服务端说的那一位
  /// 怎么走到气泡上」这一段**一行都没被覆盖**——2026-08-27 故障注入验出来的：
  /// 把 `ChatController._onError` 里那一位改成恒 false，上面全绿。
  ///
  /// 那正是这个仓库最忌讳的形状（造好了没人调用的另一面：接好了没人验）。
  group('异常里那一位真的走到消息上', () {
    test('确定性失败：committed 的那条消息带着这一位', () async {
      final container = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(
            _FailingApi(
              const CortexApiException(
                '这个会话绑在 WILLOPTPC 上的一个目录里。',
                statusCode: 409,
                retryable: false,
              ),
            ),
          ),
        ],
      );
      addTearDown(container.dispose);
      container.listen(
        chatControllerProvider,
        (_, _) {},
        fireImmediately: true,
      );

      final ctrl = container.read(chatControllerProvider.notifier);
      await _untilTrue(
        () => !container.read(chatControllerProvider).sessionsLoading,
        '会话列表',
      );
      await ctrl.send('喂');
      await _untilTrue(
        () => container.read(chatControllerProvider).streaming == null,
        '流式结束',
      );

      final last = container.read(chatControllerProvider).activeTranscript.last;
      expect(last.error, isNotNull, reason: '这一轮该是失败的');
      expect(
        last.errorIsDeterministic,
        isTrue,
        reason: '服务端说的「重发没用」没走到消息上 —— 红框会照样摆两个死按钮',
      );
    });

    test('普通失败：这一位不该被点亮', () async {
      final container = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(
            _FailingApi(const CortexApiException('模型超时了', statusCode: 504)),
          ),
        ],
      );
      addTearDown(container.dispose);
      container.listen(
        chatControllerProvider,
        (_, _) {},
        fireImmediately: true,
      );

      final ctrl = container.read(chatControllerProvider.notifier);
      await _untilTrue(
        () => !container.read(chatControllerProvider).sessionsLoading,
        '会话列表',
      );
      await ctrl.send('喂');
      await _untilTrue(
        () => container.read(chatControllerProvider).streaming == null,
        '流式结束',
      );

      final last = container.read(chatControllerProvider).activeTranscript.last;
      expect(last.error, isNotNull);
      expect(
        last.errorIsDeterministic,
        isFalse,
        reason: '一律点亮的话，配额/超时这些真能靠换模型走通的失败也没了出路',
      );
    });
  });

  /// ⚠️ **最后一环：服务端那一位真的解析得出来。**
  ///
  /// 上面那几条都从 `CortexApiException` 往下走，所以「HTTP 响应体里那个
  /// `retryable` 怎么变成异常上的一位」**一行都没被覆盖** ——
  /// 2026-08-27 故障注入验出来的：把 `_unwrapRetryable` 改成恒 null，
  /// 上面全绿。
  ///
  /// 这一条走的是**用户那条路**：真的 `HttpCortexApi`、真的 JSON 响应体，
  /// 只把传输换成 `MockClient`。
  group('服务端那一位解析得出来', () {
    /// `/chat` 是 SSE 那条路（`_events`），与普通 JSON 路不是同一段代码 ——
    /// 而这个 409 恰恰出在那条路上，所以要测的就是它。
    Future<CortexApiException> chatFailure(String body, int status) async {
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:9',
        client: MockClient.streaming((request, bodyStream) async {
          return http.StreamedResponse(
            Stream.value(utf8.encode(body)),
            status,
            request: request,
          );
        }),
      );
      try {
        await api.chat(sessionId: 's1', message: '喂').drain<void>();
        fail('该抛异常');
      } on CortexApiException catch (e) {
        return e;
      }
    }

    test('retryable:false 一路带到异常上', () async {
      final e = await chatFailure(
        jsonEncode({'error': '机器离线', 'retryable': false}),
        409,
      );
      expect(e.message, '机器离线', reason: '正文照旧要剥出来');
      expect(
        e.isDeterministic,
        isTrue,
        reason: '服务端说了「重发没用」而客户端没接住 —— 红框会照样摆两个死按钮',
      );
    });

    test('没有这一位时不点亮', () async {
      final e = await chatFailure(jsonEncode({'error': '沙箱刚被回收了'}), 409);
      expect(
        e.isDeterministic,
        isFalse,
        reason: '默认必须是「给出路」：服务端漏说一次不该把一整类失败的出路砍掉',
      );
    });
  });
}

Future<void> _untilTrue(bool Function() cond, String what) async {
  final deadline = DateTime.now().add(const Duration(seconds: 20));
  while (!cond()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$what');
    await Future<void>.delayed(const Duration(milliseconds: 15));
  }
}

/// 除了 `chat` 一律走 mock —— 只把这一条换成「抛指定的异常」。
///
/// 继承 [MockCortexApi] 而不是从零实现 `CortexApi`：那个接口有上百个方法，
/// 手写一份的下场是每加一个端点这条测试就编不过。
class _FailingApi extends MockCortexApi {
  _FailingApi(this.failure) : super(instant: true);

  final CortexApiException failure;

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
  }) async* {
    throw failure;
  }
}
