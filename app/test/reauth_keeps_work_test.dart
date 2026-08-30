/// **被掉出来的时候，用户的东西不许丢。**
///
/// 2026-08-30 实报：「我输入会话内容，整个会话丢失，然后窗口闪一下全部重置了，
/// 需要重新输入重来。为什么一定要过期呢，桌面端从启动到关闭前永远不存在过期
/// 才对啊」。
///
/// 「全部重置」是**两处**一起造成的，缺任何一处修复都只做了一半：
///
/// ① `ChatController` 监听 `cortexApiProvider`，凭据没了 api 换代 →
///    `_reload()` → `state = ChatState(...)`，正在看的那整段对话被清空。
/// ② `LoginGate` 把整棵 `AppShell` 换成登录屏，输入框连同它的
///    `TextEditingController` 一起被拆掉 —— 敲了一半的话没了。
///
/// 这个文件一条守一处。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/models/chat_session.dart';
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

class _Api extends MockCortexApi {
  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => [
    ChatSession(
      id: 's1',
      title: '正在看的这条',
      updatedAt: DateTime.utc(2026, 8, 31),
    ),
  ];
}

Future<void> _settle() async {
  for (var i = 0; i < 20; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

void main() {
  /// ① 凭据没了不许清空会话状态。
  ///
  /// 这条盯的是最容易被当成「顺带」的那一处：那条监听的本意是「换了后端就
  /// 重新拉一遍」，而**掉回登录门也会让 api 换代**。于是同一行代码把用户
  /// 正在看的对话一起清掉了 —— 而它看起来完全合理。
  test('掉回登录门时，会话状态原样留着', () async {
    var ready = true;
    final container = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        // 直接替掉 api provider：ready 时给真替身，掉门之后给那个「门没开」
        // 的桩 —— 与生产里 `cortexApiProvider` 的行为一致
        cortexApiProvider.overrideWith(
          (ref) => ready ? _Api() : GateClosedApi(),
        ),
      ],
    );
    addTearDown(container.dispose);

    final chat = container.read(chatControllerProvider.notifier);
    container.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
    await _settle();
    expect(
      container.read(chatControllerProvider).sessions,
      isNotEmpty,
      reason: '前提没成立：还没进来任何会话，测不出「会不会被清掉」',
    );

    // 掉回登录门
    ready = false;
    container.invalidate(cortexApiProvider);
    await _settle();

    expect(
      container.read(chatControllerProvider).sessions,
      isNotEmpty,
      reason:
          '凭据没了就把会话清空了 —— 用户正在看的那整段对话当场消失。'
          '没有凭据时该做的只有「作废在飞的请求」，屏幕上的东西要留着，'
          '重新登录之后接着看',
    );
    // 用一下 chat，免得 analyzer 说它没被使用
    expect(chat, isNotNull);
  });

  group('② 被掉出来 vs 冷启动，是两件事', () {
    ProviderContainer boot() {
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          // 探针不许打真网络 —— 不替的话 signIn 会去连 127.0.0.1:8080，
          // 于是「先得真的登录上」这个前提根本不成立，而失败信息看起来
          // 像是被测的行为坏了
          authProbeApiProvider.overrideWithValue((_) => _Api()),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);
      c.listen(authControllerProvider, (_, _) {}, fireImmediately: true);
      return c;
    }

    test('冷启动没登录 —— 整页登录屏', () async {
      final c = boot();
      await _settle();
      expect(
        c.read(authControllerProvider).reauth,
        isFalse,
        reason:
            '冷启动时用户手上什么都没有，一张整页登录屏是对的；'
            '盖一层在空界面上只会让他看着一片空白的产品',
      );
    });

    test('用着用着被掉出来 —— 盖一层，底下留着', () async {
      final c = boot();
      final ctrl = c.read(authControllerProvider.notifier);
      await _settle();
      await ctrl.signIn('a-token');
      await _settle();
      expect(
        c.read(authControllerProvider).isReady,
        isTrue,
        reason: '前提：先得真的登录上，否则下面那一步不是「被掉出来」',
      );

      // 服务端明确拒了这把 refresh —— 这才是真的要重新登录的那一档
      await ctrl.onUnauthorized();
      await _settle();

      expect(
        c.read(authControllerProvider).reauth,
        isTrue,
        reason:
            '从 ready 掉下来却没标成 reauth —— 界面会把整棵 AppShell 换掉，'
            '用户敲了一半的话连同 TextEditingController 一起被拆',
      );
    });

    /// **负对照。** 主动登出必须回整页登录屏：他要的就是离开这个账号，
    /// 把上一个人的对话留在覆盖层底下既没用也不该。
    test('主动登出 —— 回整页登录屏', () async {
      final c = boot();
      final ctrl = c.read(authControllerProvider.notifier);
      await _settle();
      await ctrl.signIn('a-token');
      await _settle();
      await ctrl.onUnauthorized();
      await _settle();
      expect(c.read(authControllerProvider).reauth, isTrue);

      await ctrl.signOut();
      await _settle();
      expect(
        c.read(authControllerProvider).reauth,
        isFalse,
        reason:
            '主动登出之后还盖着覆盖层 —— 上一个账号的对话留在底下，'
            '既没用也不该给下一个人看见',
      );
    });
  });
}
