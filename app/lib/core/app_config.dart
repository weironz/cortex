/// Runtime configuration.
///
/// Compile-time defaults come from `--dart-define`; the settings sheet can
/// override them for the running session (handy for pointing a Web build at a
/// different daemon without rebuilding).
class AppConfig {
  const AppConfig({
    required this.useMock,
    required this.baseUrl,
    this.offline = false,
  });

  /// When true, [MockCortexApi] is used and no network call is ever made.
  final bool useMock;

  /// `cortexd` origin, e.g. `http://127.0.0.1:8080`.
  final String baseUrl;

  /// 离线模式：**没有 cortexd**，只有本地 agent。
  ///
  /// # 它与 [useMock] 的区别
  ///
  /// mock 是**假数据**——为了在没有后端时看界面。离线模式里一切都是真的：
  /// 真的模型、真的工具、真的读写你本机的文件。唯一缺的是记忆
  /// （没有 cortexd 就没有记忆库），而这一轮轮对话会排进本地队列，
  /// 以后接上服务器时自动灌回去。
  ///
  /// # 为什么这个模式值得存在
  ///
  /// 「装了就能用」与「记忆是核心」这两件事有张力，而把它藏起来解决不了：
  /// 一个连不上服务器的人现在停在登录界面，什么也做不了。给他一条明说
  /// 「没有记忆」的路，比让他对着一个连不上的地址反复重试要诚实。
  final bool offline;

  /// `--dart-define=USE_MOCK=true|false`
  ///
  /// Defaults to **false**: `cortexd` is running locally, so the honest default
  /// is to talk to it. CI and offline demos pass `USE_MOCK=true`.
  static const bool defaultUseMock = bool.fromEnvironment('USE_MOCK');

  /// `--dart-define=CORTEX_BASE_URL=http://host:port`
  static const String defaultBaseUrl = String.fromEnvironment(
    'CORTEX_BASE_URL',
    defaultValue: 'http://127.0.0.1:8080',
  );

  static const AppConfig initial = AppConfig(
    useMock: defaultUseMock,
    baseUrl: defaultBaseUrl,
  );

  AppConfig copyWith({bool? useMock, String? baseUrl, bool? offline}) =>
      AppConfig(
        useMock: useMock ?? this.useMock,
        baseUrl: baseUrl ?? this.baseUrl,
        offline: offline ?? this.offline,
      );

  @override
  bool operator ==(Object other) =>
      other is AppConfig &&
      other.useMock == useMock &&
      other.baseUrl == baseUrl &&
      other.offline == offline;

  @override
  int get hashCode => Object.hash(useMock, baseUrl, offline);
}
