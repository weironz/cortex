/// **本机 agent 的 401 必须摇到过期那个铃。**
///
/// 2026-08-30 用户实报：桌面端用一段时间之后，带工具的那一句永远失败，
/// 红框里是 `cortexd 401 Unauthorized`，**要重启应用才恢复**。
///
/// 根因是两条线之间没有人接：本机 agent 的出站凭据（access token）15 分钟
/// 过期，而它自己换不了那把钥匙 —— 由桌面端热推进来。桌面端的过期探测
/// (`HttpCortexApi._failure`) 接的是**它自己发的 HTTP 的 401**，而这条失败是
/// 以一段文本混在**一条成功的 SSE 流**里回来的，那条线上根本没有状态码。
/// 于是不续期、不推新凭据，本机 agent 抱着死 token 用到重启。
///
/// 这个文件钉住修法的最后一环：事件带着 `needsReauth` 回来时，
/// controller 真的去摇 `AuthController.onUnauthorized()`。
///
/// # 怎么观测「铃响了」
///
/// 手上没有 refresh token 时，`onUnauthorized()` 会走 `noRefreshToken`
/// 那一支把人退回登录门 —— **phase 变成 needsToken 就是铃响过的证据**。
/// 这比塞一个假的 AuthController 更硬：它走的是真实现那条路。
library;

import 'dart:async';

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 一轮**只吐一条错误事件**的替身。`needsReauth` 由每条用例给。
class _FailingApi extends MockCortexApi {
  _FailingApi({required this.needsReauth});
  final bool needsReauth;

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
    yield ChatErrorEvent(
      '非法输入：cortexd 401 Unauthorized：未认证',
      needsReauth: needsReauth,
    );
  }
}

Future<void> _settle() async {
  for (var i = 0; i < 12; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

Future<void> _until(
  bool Function() cond, {
  required String reason,
  Duration timeout = const Duration(seconds: 10),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!cond()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 15));
  }
}

ProviderContainer _boot({required bool needsReauth}) {
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      cortexApiProvider.overrideWith(
        (ref) => _FailingApi(needsReauth: needsReauth),
      ),
    ],
  );
  container.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  container.listen(authControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

void main() {
  group('本机 agent 报凭据被拒时', () {
    test('摇了过期那个铃 —— 否则没人会去续期，坏到重启为止', () async {
      final container = _boot(needsReauth: true);
      addTearDown(container.dispose);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );
      await container
          .read(chatControllerProvider.notifier)
          .send('桌面上新建 test.txt');
      await _settle();

      expect(
        container.read(authControllerProvider).phase,
        AuthPhase.needsToken,
        reason:
            '铃没响：本机 agent 的 401 没有触发续期。它自己换不了那把钥匙，'
            '于是会抱着一把死 token 一直用下去 —— 用户看到的就是'
            '「过一会儿就不能用了，重启才好」',
      );
    });

    /// **负对照。** 少了它，把 `if (needsReauth)` 那道判断整个删掉，
    /// 上面那条照样绿 —— 那时每一次普通失败都会去摇续期的铃，
    /// 而用户会被一次供应商超时送去重新登录。
    test('普通失败不摇那个铃 —— 摇错了比不摇更糟', () async {
      final container = _boot(needsReauth: false);
      addTearDown(container.dispose);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );
      final before = container.read(authControllerProvider).phase;
      await container.read(chatControllerProvider.notifier).send('随便说一句');
      await _settle();

      expect(
        container.read(authControllerProvider).phase,
        before,
        reason:
            '一次与凭据无关的失败把人踢去了登录页 —— 用户会重新登录一遍，'
            '回来发现还是不行，而真正的原因被这一步盖住了',
      );
    });
  });

  /// 线协议那一位要真的读得出来。
  ///
  /// 这条盯的是最安静的那种坏法：字段名对不上 ⇒ 永远 `false` ⇒ 上面两条
  /// 照样绿（负对照那条本来就期望 false），而线上一切照旧坏着。
  test('needs_reauth 从线协议里读得出来', () {
    final on = ChatEvent.fromJson({
      'type': 'error',
      'message': 'x',
      'needs_reauth': true,
    });
    expect(
      on,
      isA<ChatErrorEvent>().having((e) => e.needsReauth, 'needsReauth', isTrue),
    );

    // 老服务端不发这个字段 = 维持从前的行为，不是崩
    final absent = ChatEvent.fromJson({'type': 'error', 'message': 'x'});
    expect(
      absent,
      isA<ChatErrorEvent>().having(
        (e) => e.needsReauth,
        'needsReauth',
        isFalse,
      ),
      reason: '不发这个字段的老服务端该退回从前的行为，而不是被当成凭据问题',
    );
  });
}
