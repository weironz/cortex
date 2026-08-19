/// 重启之后不用重新登录，也不用重填地址。
///
/// # 这两件事此前都不成立
///
/// **登录状态**：登录时把 refresh token 存进了系统凭据库
/// （`rememberToken`），而启动时**从来没有读回来**。`restoreSession`
/// 写好了、注释写得很详细、全项目零调用 —— 于是「登录一次管 30 天」
/// 在产品里是不存在的，桌面端与 Web 都是每次重启回到登录页。
///
/// 漏掉它的原因值得记下来：`_seeded()` 是同步的，它读了
/// `authSeedTokenProvider`（只有环境变量那一份），**看起来已经在读凭据了**。
/// 而真正存下来的那把要跨进程问钥匙串，是异步的，只能排微任务 ——
/// 那一步从来没有人写。
///
/// **地址**：`AppConfig` 完全活在内存里，重启回到编译期默认值。
/// 一个把 cortexd 部署在别处的人，每次打开都要重填一遍 ——
/// 而他还没登录，连「登录状态记住了」都轮不到。
library;

import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/models/auth_tokens.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/replay_api.dart';

/// 一个只关心「有没有人来续会话」的替身。
class _Refreshing extends ReplayApi {
  _Refreshing({this.shouldSucceed = true}) : super(episodeCount: 0);

  final bool shouldSucceed;
  int refreshCalls = 0;
  String? sawToken;

  @override
  Future<AuthTokens> refreshSession(String refreshToken) async {
    refreshCalls++;
    sawToken = refreshToken;
    if (!shouldSucceed) {
      throw CortexApiException('凭据已失效', statusCode: 401);
    }
    return const AuthTokens(
      accessToken: 'fresh-access',
      accessExpiresInSecs: 900,
      refreshToken: 'rotated-refresh',
      refreshExpiresInSecs: 2592000,
      userId: 'u1',
    );
  }
}

void main() {
  test('启动时会拿存下来的凭据去续会话', () async {
    final api = _Refreshing();
    final container = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_LiveConfig.new),
        authSeedTokenProvider.overrideWithValue(null),
        authProbeApiProvider.overrideWithValue((_) => api),
        // 凭据库里躺着上次登录留下的那把
        rememberedTokenProvider.overrideWith((ref) async => 'stored-refresh'),
      ],
    );
    addTearDown(container.dispose);

    container.read(authControllerProvider);
    for (var i = 0; i < 6; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    expect(
      api.refreshCalls,
      greaterThan(0),
      reason:
          '启动时没有人拿存下来的凭据去续 —— 这正是「每次重启都要重新登录」。'
          'rememberToken 存了、restoreSession 写了，中间那一步没人写',
    );
    expect(api.sawToken, 'stored-refresh');
    expect(
      container.read(authControllerProvider).phase,
      AuthPhase.ready,
      reason: '续上了就该直接进主界面，用户什么都不用做',
    );
  });

  /// **这条是线上那个「每次启动都要重新登录」的复现。**
  ///
  /// 上面那条测试用 `_LiveConfig` 替掉了真的 `AppConfigNotifier`，于是它
  /// 拿到的地址从第一帧起就是终值 —— 而真实启动**不是那样**：
  ///
  /// 1. `AppConfigNotifier.build()` 先返回编译期默认地址，再排一个微任务
  ///    从磁盘把上次那个读回来（异步）
  /// 2. `AuthController` 监听 baseUrl，一变就 `_reset()` → `++_generation`
  /// 3. `_bootstrap` 此时正卡在读凭据库（跨进程，比读设置慢），回来一看
  ///    代次变了，判定「有人接手了」**直接返回 —— 续期一次都没发出去**
  ///
  /// 所以只要用户存过一个与编译期默认值不同的地址（也就是所有自建部署的
  /// 用户），登录状态就永远续不上。服务端那侧看得很清楚：token 签发之后
  /// `rotated_at` 一直是空的，一次轮换都没有。
  test('地址是异步读回来的，续期不能被它挤掉', () async {
    final api = _Refreshing();
    final container = ProviderContainer(
      overrides: [
        // **不替 appConfigProvider** —— 要的就是它那段异步恢复
        settingsReaderProvider.overrideWithValue(
          () async => {'base_url': 'https://cortex.example.com/api'},
        ),
        settingsWriterProvider.overrideWithValue((_) async {}),
        authSeedTokenProvider.overrideWithValue(null),
        authProbeApiProvider.overrideWithValue((_) => api),
        rememberedTokenProvider.overrideWith((ref) async => 'stored-refresh'),
      ],
    );
    addTearDown(container.dispose);

    container.read(authControllerProvider);
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    expect(
      api.refreshCalls,
      greaterThan(0),
      reason:
          '存过自定义地址的用户每次启动都回到登录页 —— 因为地址恢复把'
          '正在读凭据库的那次续期判成了「已被取代」。'
          '服务端那侧的证据：token 的 rotated_at 一直是空的',
    );
    expect(
      container.read(authControllerProvider).phase,
      AuthPhase.ready,
      reason: '续上了就该直接进主界面',
    );
  });

  test('凭据失效时落回登录页，而不是卡在转圈', () async {
    final api = _Refreshing(shouldSucceed: false);
    final container = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_LiveConfig.new),
        authSeedTokenProvider.overrideWithValue(null),
        authProbeApiProvider.overrideWithValue((_) => api),
        rememberedTokenProvider.overrideWith((ref) async => 'expired'),
      ],
    );
    addTearDown(container.dispose);

    container.read(authControllerProvider);
    for (var i = 0; i < 8; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    expect(
      container.read(authControllerProvider).phase,
      isNot(AuthPhase.probing),
      reason:
          '续不上就要落回去让人登录。停在 probing 的话界面一直转圈，'
          '而用户没有任何办法',
    );
  });

  test('地址会被存下来', () async {
    var saved = <String, String>{};
    final container = ProviderContainer(
      overrides: [
        // 读也要替掉：存一个键现在是**读—改—写**（见 settingsPatcherProvider），
        // 不替的话这个单测会去碰真实的 %APPDATA%\cortex\settings.json
        settingsReaderProvider.overrideWithValue(() async => Map.of(saved)),
        settingsWriterProvider.overrideWithValue((values) async {
          saved = Map.of(values);
        }),
      ],
    );
    addTearDown(container.dispose);

    container
        .read(appConfigProvider.notifier)
        .setBaseUrl('https://mine.example');
    // 读—改—写有两次 await，一个 Duration.zero 跨不过去
    for (var i = 0; i < 4; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    expect(
      saved['base_url'],
      'https://mine.example',
      reason:
          '不存的话重启回到编译期默认地址 —— 一个把 cortexd 部署在别处的人，'
          '每次打开都要重填一遍',
    );
  });

  test('存下来的地址在启动时被读回来', () async {
    final container = ProviderContainer(
      overrides: [
        settingsReaderProvider.overrideWithValue(
          () async => {'base_url': 'https://restored.example'},
        ),
      ],
    );
    addTearDown(container.dispose);

    container.read(appConfigProvider);
    await Future<void>.delayed(Duration.zero);
    await Future<void>.delayed(Duration.zero);

    expect(
      container.read(appConfigProvider).baseUrl,
      'https://restored.example',
      reason:
          '读不回来的话，「存下来」这一步等于没做 —— '
          '这正是登录凭据那半犯过的错',
    );
  });
}

class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:8080');
}
