/// Runtime configuration.
///
/// Compile-time defaults come from `--dart-define`; the settings sheet can
/// override them for the running session (handy for pointing a Web build at a
/// different daemon without rebuilding).
class AppConfig {
  const AppConfig({required this.useMock, required this.baseUrl});

  /// When true, [MockCortexApi] is used and no network call is ever made.
  final bool useMock;

  /// `cortexd` origin, e.g. `http://127.0.0.1:8080`.
  final String baseUrl;

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

  AppConfig copyWith({bool? useMock, String? baseUrl}) => AppConfig(
    useMock: useMock ?? this.useMock,
    baseUrl: baseUrl ?? this.baseUrl,
  );

  @override
  bool operator ==(Object other) =>
      other is AppConfig &&
      other.useMock == useMock &&
      other.baseUrl == baseUrl;

  @override
  int get hashCode => Object.hash(useMock, baseUrl);
}
