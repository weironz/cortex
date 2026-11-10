/// Web has no agent of its own. See `local_agent.dart` for why that is the
/// honest answer rather than a missing feature.
///
/// Every name here mirrors the desktop side so callers need no `kIsWeb`.
library;

const bool kLocalAgentSupported = false;

List<String> defaultLocalComputerUseAllowlist() => const [];

/// Always null on Web — there is no binary to find and nowhere to run it.
LocalAgent? discoverLocalAgent() => null;

/// Present only so the types line up. Constructing it is a programming error;
/// [discoverLocalAgent] never hands one out.
class LocalAgent {
  String? get origin => null;
  bool get isRunning => false;

  Future<String> start({
    required String remote,
    required String token,
    String? userToken,
    String? llmRoute,
    bool allowRemoteAttach = false,
    List<String> computerUseAllowlist = const [],
    List<String> packagedComputerAllowlist = const [],
    List<String> browserSiteAllowlist = const [],
    Map<String, String>? extraEnv,
    Duration timeout = const Duration(seconds: 20),
    void Function(int code, String logTail)? onExit,
  }) async => throw const LocalAgentException('Web 构建不运行本地 agent');

  Future<void> stop() async {}
}

class LocalAgentException implements Exception {
  const LocalAgentException(this.message);
  final String message;
  @override
  String toString() => message;
}
