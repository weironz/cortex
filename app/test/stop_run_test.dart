/// 「停止生成」这个按钮到底停了什么。
///
/// # 它从前只停了客户端
///
/// `stopGeneration` 只 `cancel` 了 SSE 订阅 —— 服务端那一轮照跑：继续烧
/// token、继续按模型的意思改文件，而屏幕上写着「已停止生成」。
/// 一个说了假话的按钮比没有按钮更糟。
///
/// 所以这里钉的第一条就是**它真的发出了那个请求**。widget 测试抓不到这件事：
/// 界面上「已停止生成」那几个字在两种实现下一模一样。
library;

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
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

/// 记下 `stopRun` 被调了几次、针对哪个会话。
class _StopSpy extends MockCortexApi {
  _StopSpy({this.fail = false}) : super(instant: true);

  /// 让 `stopRun` 抛，验「服务端掐不掉时界面照样收」那一支。
  final bool fail;

  final List<String> stopped = [];

  @override
  Future<void> stopRun(String sessionId) async {
    stopped.add(sessionId);
    if (fail) {
      throw const CortexApiException('后端不认这条路由', statusCode: 404);
    }
  }
}

Future<void> _until(
  bool Function() condition, {
  String reason = '',
  Duration timeout = const Duration(seconds: 20),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 15));
  }
}

ProviderContainer _boot(_StopSpy api) {
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      cortexApiProvider.overrideWithValue(api),
    ],
  );
  addTearDown(container.dispose);
  return container;
}

void main() {
  test('按下停止会真的去掐服务端那一轮', () async {
    final api = _StopSpy();
    final container = _boot(api);
    final chat = container.read(chatControllerProvider.notifier);

    await _until(
      () => !container.read(chatControllerProvider).sessionsLoading,
      reason: '会话列表',
    );
    final id = container.read(chatControllerProvider).sessions.first.id;
    container.read(chatControllerProvider.notifier).selectSession(id);

    unawaited_(chat.send('说点什么'));
    await _until(
      () => container.read(chatControllerProvider).streaming != null,
      reason: '这一轮开始流式输出',
    );

    await chat.stopGeneration();

    expect(
      api.stopped,
      [id],
      reason:
          '只取消本地订阅的话，服务端那一轮会继续烧 token、继续改文件，'
          '而界面写着「已停止生成」',
    );
    expect(
      container.read(chatControllerProvider).streaming,
      isNull,
      reason: '界面这一半也要收干净',
    );
  });

  test('服务端掐不掉时，界面照样收，但把原因如实写上', () async {
    final api = _StopSpy(fail: true);
    final container = _boot(api);
    final chat = container.read(chatControllerProvider.notifier);

    await _until(
      () => !container.read(chatControllerProvider).sessionsLoading,
      reason: '会话列表',
    );
    final id = container.read(chatControllerProvider).sessions.first.id;
    container.read(chatControllerProvider.notifier).selectSession(id);

    unawaited_(chat.send('说点什么'));
    await _until(
      () => container.read(chatControllerProvider).streaming != null,
      reason: '这一轮开始流式输出',
    );

    // 不抛：最不该做的是把用户按在一个转着圈的界面上
    await chat.stopGeneration();

    expect(container.read(chatControllerProvider).streaming, isNull);
    final last = container.read(chatControllerProvider).activeTranscript.last;
    expect(
      last.error,
      contains('那一轮可能还在跑'),
      reason:
          '掐不掉就得说掐不掉。写一句干净的「已停止」是第二个谎 —— '
          '用户会以为它停了，而它在继续改他的文件',
    );
  });
}

// `unawaited` 要 dart:async，而这个文件只需要这一处
void unawaited_(Future<void> f) {
  f.ignore();
}
