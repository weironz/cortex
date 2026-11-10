import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter/foundation.dart' show visibleForTesting;

/// Windows 安装包附带的 Cua Driver；生产入口只接受这份 SHA。
const String kCuaDriverVersion = '0.38.0';
const String kCuaDriverSha256 =
    'ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2';

/// 安装应用不等于获得桌面权限；空表仍保留设置入口，让用户先选择精确路径。
/// 浏览器任务另用独立的站点 allowlist。
List<String> defaultComputerUseAllowlist() => const [];

/// 生成有界 manifest，不放宽 Cortex 的应用 allowlist。
/// 只有打包应用需要 ApplicationFrameHost；Rust 仍将 HWND 映射回已授权的包，
/// 不把系统窗口宿主本身当成已授权应用。
Map<String, Object> buildCuaCapabilityManifest({
  required Iterable<String> executables,
  required Iterable<({String kind, String identity, String executable})>
  packaged,
  String? applicationFrameHost,
}) {
  final hasPackaged = packaged.isNotEmpty;
  return {
    'version': 3,
    'expires_after': '8h',
    'idle_timeout': '30m',
    'resources': {
      'apps': [
        for (final app in executables)
          {'executable': app, 'windows': 'all', 'terminate': 'deny'},
        if (hasPackaged && applicationFrameHost != null)
          {
            'executable': applicationFrameHost,
            'windows': 'all',
            'terminate': 'deny',
          },
        for (final entry in packaged)
          {'bundle_id': entry.identity, 'windows': 'all', 'terminate': 'deny'},
      ],
    },
    'allow': {
      'tools': [
        'start_session',
        'get_window_state',
        'click',
        'type_text',
        'set_value',
        'press_key',
        'scroll',
        'end_session',
      ],
    },
  };
}

/// 进程入口可替换以验证准备流程确实没有 daemon/status/stop 子进程。
typedef ComputerUseProcessStarter =
    Future<Process> Function(
      String executable,
      List<String> arguments, {
      Map<String, String>? environment,
      String? workingDirectory,
    });

/// 生产入口只为本地 agent 准备有界配置，真正的 Job 与驱动进程由 Rust 托管。
/// 旧 start 仍保留独立 sidecar 生命周期；两种模式不能互相冒充已运行。
class ComputerUseRuntime {
  ComputerUseRuntime({required this.stateDir})
    : _driverPathOverride = null,
      _expectedDriverSha256 = kCuaDriverSha256,
      _startProcess = Process.start,
      _readDriverBytes = ((path) => File(path).readAsBytes()),
      _writeManifest = ((file, text) => file.writeAsString(text, flush: true));

  /// 只用于无真实 Cua 的文件、预算与进程调用回归，不接受模型工具参数。
  @visibleForTesting
  ComputerUseRuntime.forTesting({
    required this.stateDir,
    required String driverPath,
    required this._expectedDriverSha256,
    required this._startProcess,
    Future<List<int>> Function(String)? readDriverBytes,
    Future<File> Function(File, String)? writeManifest,
  }) : _driverPathOverride = driverPath,
       _readDriverBytes =
           readDriverBytes ?? ((path) => File(path).readAsBytes()),
       _writeManifest =
           writeManifest ??
           ((file, text) => file.writeAsString(text, flush: true));

  /// 配置准备共用这一笔预算；生产路径不等待 daemon 就绪握手。
  ///
  /// 失败只降级 Computer Use。legacy start 的探测仍有界，但普通 agent
  /// 启动不再承担 status 子进程与轮询的等待。
  static const startupTimeout = Duration(seconds: 4);
  static const _statusProbeTimeout = Duration(milliseconds: 500);
  static const _statusDrainTimeout = Duration(milliseconds: 100);
  static const _statusKillTimeout = Duration(seconds: 1);
  static const _pollInterval = Duration(milliseconds: 100);

  final String stateDir;
  final String? _driverPathOverride;
  final String _expectedDriverSha256;
  final ComputerUseProcessStarter _startProcess;
  final Future<List<int>> Function(String) _readDriverBytes;
  final Future<File> Function(File, String) _writeManifest;
  Process? _process;
  String? _activeDriverPath;
  String? _socket;
  String? _pidFile;
  String? _manifestPath;
  String? _tempManifestPath;
  String? _lastError;
  String? _lastStopError;
  _ComputerUseRuntimePhase _phase = _ComputerUseRuntimePhase.idle;
  Map<String, String>? _readyEnvironment;
  bool _agentOwned = false;
  Future<void>? _stopping;
  int _generation = 0;
  final List<String> _logTail = [];
  final List<StreamSubscription<void>> _pipes = [];

  bool get isRunning =>
      _phase == _ComputerUseRuntimePhase.running && _process != null;
  bool get isPrepared => _phase == _ComputerUseRuntimePhase.prepared;
  int? get processId => _process?.pid;
  String? get lastError => _lastError;
  String? get lastStopError => _lastStopError;
  String get diagnosticLog => _logTail.join('\n');

  /// 生产路径只校验 SHA、写 manifest 并选择专用 socket，不启动或探测驱动。
  /// 返回的 owner 标记仅选择 Rust 托管模式，不是权限或进程所有权证明。
  Future<Map<String, String>> prepareForAgent({
    required List<String> allowlist,
    List<String> packagedAllowlist = const [],
    Duration timeout = startupTimeout,
  }) => _start(
    allowlist: allowlist,
    packagedAllowlist: packagedAllowlist,
    timeout: timeout,
    agentOwned: true,
  );

  /// 兼容旧调用方的 sidecar 启动；生产 LocalAgent 只调用 prepareForAgent。
  Future<Map<String, String>> start({
    required List<String> allowlist,
    List<String> packagedAllowlist = const [],
    Duration timeout = startupTimeout,
  }) => _start(
    allowlist: allowlist,
    packagedAllowlist: packagedAllowlist,
    timeout: timeout,
    agentOwned: false,
  );

  Future<Map<String, String>> _start({
    required List<String> allowlist,
    required List<String> packagedAllowlist,
    required Duration timeout,
    required bool agentOwned,
  }) async {
    if (_phase != _ComputerUseRuntimePhase.idle) {
      final ready = _readyEnvironment;
      if (_agentOwned == agentOwned &&
          ready != null &&
          (_phase == _ComputerUseRuntimePhase.running ||
              _phase == _ComputerUseRuntimePhase.prepared)) {
        return ready;
      }
      _lastError = 'Computer Use 正在准备、启动或停止，不能切换运行模式';
      return const {};
    }
    _lastError = null;
    _lastStopError = null;
    _logTail.clear();
    if (!Platform.isWindows) return const {};
    final requested = allowlist.isNotEmpty || packagedAllowlist.isNotEmpty;
    final driver = _resolveDriverPath();
    if (driver == null) {
      if (requested) {
        _lastError = '找不到随应用安装的 Cua Driver';
      }
      return const {};
    }
    final apps = _validExecutables(allowlist);
    final packaged = _validPackagedEntries(packagedAllowlist);
    if (apps.isEmpty && packaged.isEmpty) {
      if (requested) {
        _lastError = 'Computer Use allowlist 中没有仍存在的有效 .exe';
      }
      return const {};
    }
    if (timeout <= Duration.zero) {
      _lastError = 'Computer Use 启动预算已经耗尽';
      return const {};
    }
    final effectiveTimeout = timeout < startupTimeout
        ? timeout
        : startupTimeout;
    final startupClock = Stopwatch()..start();
    final generation = ++_generation;
    _agentOwned = agentOwned;
    _phase = _ComputerUseRuntimePhase.starting;

    Duration remainingBudget() {
      final remaining = effectiveTimeout - startupClock.elapsed;
      if (remaining <= Duration.zero) {
        throw TimeoutException(
          'Computer Use sidecar 总启动预算已经耗尽',
          effectiveTimeout,
        );
      }
      return remaining;
    }

    try {
      final digest = sha256
          .convert(await _readDriverBytes(driver).timeout(remainingBudget()))
          .toString();
      if (digest.toLowerCase() != _expectedDriverSha256.toLowerCase()) {
        throw const FormatException('Cua Driver SHA-256 不匹配');
      }

      if (generation != _generation) {
        throw StateError('Computer Use 启动已被停止');
      }
      final root = Directory('$stateDir${Platform.pathSeparator}computer-use');
      await root.create(recursive: true).timeout(remainingBudget());
      final stamp = DateTime.now().microsecondsSinceEpoch.toRadixString(36);
      final socket =
          r'\\.\pipe\cortex-cua-'
          '$pid-$stamp';
      final pidFile =
          '${root.path}${Platform.pathSeparator}driver-$pid-$stamp.pid';
      final manifest =
          '${root.path}${Platform.pathSeparator}manifest-$pid-$stamp.json';
      final applicationFrameHost =
          '${Platform.environment['SystemRoot'] ?? r'C:\Windows'}'
          '${Platform.pathSeparator}System32${Platform.pathSeparator}'
          'ApplicationFrameHost.exe';
      final includeApplicationFrameHost =
          packaged.isNotEmpty && File(applicationFrameHost).existsSync();
      final temp = File('$manifest.tmp');
      // 在第一次等待前登记两个路径；超时后底层同步 I/O 可能稍后才完成，
      // stop 仍要能清掉临时文件和已经 rename 的正式 manifest。
      _manifestPath = manifest;
      _tempManifestPath = temp.path;
      await _manifestOperation(
        _writeManifest(
          temp,
          const JsonEncoder.withIndent('  ').convert(
            buildCuaCapabilityManifest(
              executables: apps,
              packaged: packaged,
              applicationFrameHost: includeApplicationFrameHost
                  ? applicationFrameHost
                  : null,
            ),
          ),
        ),
        generation,
        [temp.path, manifest],
        remainingBudget(),
      );
      if (generation != _generation) {
        throw StateError('Computer Use manifest 写入完成时启动已被停止');
      }
      await _manifestOperation(temp.rename(manifest), generation, [
        temp.path,
        manifest,
      ], remainingBudget());
      _tempManifestPath = null;
      if (generation != _generation) {
        try {
          final created = File(manifest);
          if (created.existsSync()) created.deleteSync();
        } on Object {
          // stop 已经尝试过清理；这里只处理“rename 在停止后才完成”的竞态。
        }
        throw StateError('Computer Use manifest 创建完成时启动已被停止');
      }
      _activeDriverPath = driver;
      _socket = socket;
      _pidFile = agentOwned ? null : pidFile;

      final daemonEnv = <String, String>{
        'CUA_DRIVER_RS_HOME':
            '$stateDir${Platform.pathSeparator}computer-use${Platform.pathSeparator}driver-home',
        'CUA_DRIVER_RS_TELEMETRY_ENABLED': 'false',
        'CUA_TELEMETRY_ENABLED': 'false',
        'CUA_TELEMETRY': '0',
        'CUA_TELEMETRY_FORBID_NETWORK': '1',
        'DO_NOT_TRACK': '1',
      };
      final environment = Map<String, String>.unmodifiable({
        ...daemonEnv,
        'CORTEX_CUA_DRIVER_PATH': driver,
        'CORTEX_CUA_DRIVER_SOCKET': socket,
        'CORTEX_CUA_EXECUTABLE_ALLOWLIST': apps.join(';'),
        'CORTEX_CUA_ALLOW_ACTIONS': '1',
        'CORTEX_CUA_MANIFEST_PATH': manifest,
        'CORTEX_CUA_DRIVER_SHA256': _expectedDriverSha256,
        if (agentOwned) 'CORTEX_CUA_RUNTIME_OWNER': 'agent',
        if (packaged.isNotEmpty)
          'CORTEX_CUA_PACKAGED_APP_ALLOWLIST': packaged
              .map(
                (entry) =>
                    '${entry.kind}|${entry.identity}|${entry.executable}',
              )
              .join('\n'),
      });
      if (agentOwned) {
        _readyEnvironment = environment;
        _phase = _ComputerUseRuntimePhase.prepared;
        return environment;
      }
      final startFuture = _startProcess(
        driver,
        [
          'serve',
          '--permission-mode',
          'bounded',
          '--capability-manifest',
          manifest,
          '--approve-capability-manifest',
          '--socket',
          socket,
          '--pid-file',
          pidFile,
          '--no-overlay',
        ],
        environment: daemonEnv,
        workingDirectory: File(driver).parent.path,
      );
      Process process;
      try {
        process = await startFuture.timeout(remainingBudget());
      } on TimeoutException {
        // Dart 不能取消已经发出的 Process.start。停止或超时会推进 generation；
        // 如果系统调用稍后才真正创建进程，这个回调会把迟到的进程杀掉。
        unawaited(
          startFuture.then((lateProcess) {
            if (generation != _generation) {
              lateProcess.kill();
            }
          }, onError: (_) {}),
        );
        rethrow;
      }
      if (generation != _generation) {
        process.kill();
        throw StateError('Computer Use 启动在进程句柄返回前已被停止');
      }
      _process = process;
      _pipes.addAll([_tail(process.stdout), _tail(process.stderr)]);

      if (!await _waitUntilReady(
        driver,
        socket,
        daemonEnv,
        effectiveTimeout,
        generation,
      )) {
        _lastError =
            'bounded daemon 未在 ${effectiveTimeout.inMilliseconds}ms 内就绪';
        await stop();
        return const {};
      }
      _watchExit(process);
      _readyEnvironment = environment;
      _phase = _ComputerUseRuntimePhase.running;
      return environment;
    } on Object catch (error) {
      _logTail.add('准备或启动 Computer Use 失败：$error');
      _lastError = '$error';
      await stop();
      return const {};
    }
  }

  /// 生产调用方须先确认 agent 退出；这里只删本对象配置，不发送 stop，
  /// 不查询 Rust 托管进程，也不声称 Rust Job 已排空。
  Future<void> stop() {
    final pending = _stopping;
    if (pending != null) return pending;
    late final Future<void> stopping;
    stopping = _stop().whenComplete(() {
      if (identical(_stopping, stopping)) _stopping = null;
    });
    _stopping = stopping;
    return stopping;
  }

  Future<void> _stop() async {
    _phase = _ComputerUseRuntimePhase.stopping;
    _generation += 1;
    final stopGeneration = _generation;
    _readyEnvironment = null;
    final failures = <String>[];
    final process = _process;
    _process = null;
    for (final pipe in _pipes) {
      try {
        await pipe.cancel();
      } on Object catch (error) {
        failures.add('取消 sidecar 日志管道失败：$error');
        // 单条日志管道取消失败不能阻断后续 driver/sidecar 清理。
      }
    }
    _pipes.clear();

    final driver = _activeDriverPath;
    final socket = _socket;
    final pidFile = _pidFile;
    final manifest = _manifestPath;
    final tempManifest = _tempManifestPath;
    _activeDriverPath = null;
    _socket = null;
    _pidFile = null;
    _manifestPath = null;
    _tempManifestPath = null;

    if (!_agentOwned && driver != null && socket != null) {
      try {
        await _runBoundedProcess(driver, [
          'stop',
          '--socket',
          socket,
          if (pidFile != null) ...['--pid-file', pidFile],
        ], timeout: const Duration(seconds: 3));
      } on Object catch (error) {
        failures.add('cua-driver stop 失败：$error');
        // legacy 模式仍以自己持有的进程句柄完成最终清理。
      }
    }
    if (process != null) {
      try {
        process.kill();
        final code = await process.exitCode.timeout(
          const Duration(seconds: 3),
          onTimeout: () => _stillRunningExitCode,
        );
        if (code == _stillRunningExitCode) {
          failures.add('sidecar 进程在 3 秒内没有退出');
        }
      } on Object catch (error) {
        failures.add('终止 sidecar 进程失败：$error');
        // driver stop 已尽力执行；进程句柄状态异常时仍继续删 manifest。
      }
    }
    if (manifest != null) {
      try {
        File(manifest).deleteSync();
      } on Object catch (error) {
        if (File(manifest).existsSync()) {
          failures.add('删除 sidecar manifest 失败：$error');
        }
        // Stale manifests contain no secret and are replaced on next launch.
      }
    }
    if (tempManifest != null) {
      try {
        final file = File(tempManifest);
        if (file.existsSync()) file.deleteSync();
      } on Object catch (error) {
        failures.add('删除临时 sidecar manifest 失败：$error');
      }
    }
    if (pidFile != null) {
      try {
        final file = File(pidFile);
        if (file.existsSync()) file.deleteSync();
      } on Object catch (error) {
        failures.add('删除 sidecar pid 文件失败：$error');
      }
    }
    _lastStopError = failures.isEmpty ? null : failures.join('；');
    if (_generation == stopGeneration) {
      _agentOwned = false;
      _phase = _ComputerUseRuntimePhase.idle;
    }
  }

  /// timeout 只停止 Dart 等待，文件操作仍可能迟到；只回收本轮登记的两个路径，
  /// 不能为了清尾部而停止下一轮进程或遍历删除其他实例文件。
  Future<T> _manifestOperation<T>(
    Future<T> operation,
    int generation,
    List<String> paths,
    Duration timeout,
  ) {
    void cleanLateFiles() {
      if (generation == _generation) return;
      for (final path in paths) {
        try {
          final file = File(path);
          if (file.existsSync()) file.deleteSync();
        } on Object catch (error) {
          _lastStopError = '迟到的 Computer Use 配置清理失败：$error';
        }
      }
    }

    unawaited(
      operation.then<void>(
        (_) => cleanLateFiles(),
        onError: (Object _, StackTrace _) => cleanLateFiles(),
      ),
    );
    return operation.timeout(timeout);
  }

  String? _resolveDriverPath() {
    final override =
        _driverPathOverride ?? Platform.environment['CORTEX_CUA_DRIVER_PATH'];
    final candidate = (override != null && override.trim().isNotEmpty)
        ? override
        : '${File(Platform.resolvedExecutable).parent.path}'
              '${Platform.pathSeparator}cua${Platform.pathSeparator}cua-driver.exe';
    return File(candidate).existsSync() ? candidate : null;
  }

  List<String> _validExecutables(List<String> candidates) {
    final seen = <String>{};
    return [
      for (final raw in candidates)
        if (_validAbsoluteFile(raw) && seen.add(raw.toLowerCase())) raw.trim(),
    ];
  }

  List<({String kind, String identity, String executable})>
  _validPackagedEntries(List<String> candidates) {
    final seen = <String>{};
    return [
      for (final raw in candidates)
        if (_parsePackagedEntry(raw) case final entry?)
          if (_validAbsoluteFile(entry.executable) &&
              seen.add('${entry.identity}|${entry.executable}'.toLowerCase()))
            entry,
    ];
  }

  ({String kind, String identity, String executable})? _parsePackagedEntry(
    String raw,
  ) {
    final parts = raw.split('|');
    if (parts.length != 3) return null;
    final kind = parts[0].trim().toLowerCase();
    final identity = parts[1].trim();
    final executable = parts[2].trim();
    if (kind != 'aumid' && kind != 'package-family') return null;
    if (identity.isEmpty || identity.length > 512) return null;
    if (kind == 'aumid' && !identity.contains('!')) return null;
    if (kind == 'package-family' && identity.contains('!')) return null;
    return (kind: kind, identity: identity, executable: executable);
  }

  bool _validAbsoluteFile(String raw) {
    final path = raw.trim();
    if (path.isEmpty || path.contains('\u0000')) return false;
    if (!RegExp(r'^[A-Za-z]:[\\/]').hasMatch(path) && !path.startsWith(r'\\')) {
      return false;
    }
    final file = File(path);
    return file.existsSync() && file.path.toLowerCase().endsWith('.exe');
  }

  Future<bool> _waitUntilReady(
    String driver,
    String socket,
    Map<String, String> env,
    Duration timeout,
    int generation,
  ) async {
    final deadline = Stopwatch()..start();
    var listening = false;
    while (deadline.elapsed < timeout) {
      if (generation != _generation) return false;
      if (_logTail.any((line) => line.contains('daemon listening'))) {
        listening = true;
        break;
      }
      if (_process == null) return false;
      await _sleepWithin(deadline, timeout);
    }
    if (!listening) return false;

    while (deadline.elapsed < timeout) {
      if (generation != _generation) return false;
      final process = _process;
      if (process == null) return false;
      final code = await process.exitCode.timeout(
        const Duration(milliseconds: 1),
        onTimeout: () => -1,
      );
      if (code != -1) return false;
      try {
        final remaining = timeout - deadline.elapsed;
        if (remaining <= Duration.zero) return false;
        final statusBudget = remaining < _statusProbeTimeout
            ? remaining
            : _statusProbeTimeout;
        final result = await _runBoundedProcess(
          driver,
          ['status', '--socket', socket],
          environment: env,
          timeout: statusBudget,
        );
        final output = '${result.stdout}\n${result.stderr}';
        if (output.trim().isNotEmpty) {
          _logTail.add('status(${result.exitCode}): ${output.trim()}');
          if (_logTail.length > 30) _logTail.removeAt(0);
        }
        if (result.exitCode == 0 &&
            output.contains('permission mode: bounded') &&
            output.contains('approved_at_startup=true') &&
            output.contains('valid=true')) {
          if (generation != _generation) return false;
          return true;
        }
      } on Object catch (error) {
        // The daemon may not have created its named pipe yet.
        _logTail.add('status exception: $error');
        if (_logTail.length > 30) _logTail.removeAt(0);
      }
      await _sleepWithin(deadline, timeout);
    }
    return false;
  }

  /// 运行一个有真正取消动作的短命令。
  ///
  /// `Process.run(...).timeout()` 只停止等待，不会杀子进程；一个卡住的
  /// `cua-driver status` / `stop` 会在 sidecar 已降级或已切换后继续存活，
  /// 下一次启动再叠一个。这里持有 handle，超时后先 kill，再回收管道。
  Future<ProcessResult> _runBoundedProcess(
    String executable,
    List<String> arguments, {
    Map<String, String>? environment,
    required Duration timeout,
  }) async {
    final process = await _startProcess(
      executable,
      arguments,
      environment: environment,
    );
    final stdoutBytes = <int>[];
    final stderrBytes = <int>[];
    final stdoutDone = Completer<void>();
    final stderrDone = Completer<void>();
    final stdoutSub = process.stdout.listen(
      stdoutBytes.addAll,
      onDone: () {
        if (!stdoutDone.isCompleted) stdoutDone.complete();
      },
      onError: (_) {
        if (!stdoutDone.isCompleted) stdoutDone.complete();
      },
    );
    final stderrSub = process.stderr.listen(
      stderrBytes.addAll,
      onDone: () {
        if (!stderrDone.isCompleted) stderrDone.complete();
      },
      onError: (_) {
        if (!stderrDone.isCompleted) stderrDone.complete();
      },
    );

    try {
      final code = await process.exitCode.timeout(timeout);
      try {
        await Future.wait([
          stdoutDone.future,
          stderrDone.future,
        ]).timeout(_statusDrainTimeout);
      } on TimeoutException {
        await stdoutSub.cancel();
        await stderrSub.cancel();
      }
      return ProcessResult(
        process.pid,
        code,
        utf8.decode(stdoutBytes, allowMalformed: true),
        utf8.decode(stderrBytes, allowMalformed: true),
      );
    } on TimeoutException {
      process.kill();
      final code = await process.exitCode.timeout(
        _statusKillTimeout,
        onTimeout: () => _stillRunningExitCode,
      );
      await stdoutSub.cancel();
      await stderrSub.cancel();
      if (code == _stillRunningExitCode) {
        throw TimeoutException('子进程在超时清理时已无法被 kill；退出状态仍未确认', timeout);
      }
      rethrow;
    }
  }

  /// 最后一轮睡眠也不能越过总 deadline。
  Future<void> _sleepWithin(Stopwatch deadline, Duration timeout) async {
    final remaining = timeout - deadline.elapsed;
    if (remaining <= Duration.zero) return;
    await Future<void>.delayed(
      remaining < _pollInterval ? remaining : _pollInterval,
    );
  }

  StreamSubscription<void> _tail(Stream<List<int>> pipe) => pipe
      .transform(const Utf8Decoder(allowMalformed: true))
      .transform(const LineSplitter())
      .listen((line) {
        _logTail.add(line);
        if (_logTail.length > 30) _logTail.removeAt(0);
      }, onError: (_) {});

  void _watchExit(Process process) {
    unawaited(
      process.exitCode.then((code) async {
        if (!identical(_process, process)) {
          // stop() owns intentional shutdown and records its own cleanup result.
          return;
        }
        _process = null;
        _phase = _ComputerUseRuntimePhase.idle;
        _readyEnvironment = null;
        _lastError = 'Cua bounded daemon 意外退出（code=$code）';
        for (final pipe in _pipes) {
          try {
            await pipe.cancel();
          } on Object {
            // The process is already gone; stale pipe state is best-effort cleanup.
          }
        }
        _pipes.clear();
        _deleteStateFiles();
      }),
    );
  }

  void _deleteStateFiles() {
    final manifest = _manifestPath;
    final pidFile = _pidFile;
    final tempManifest = _tempManifestPath;
    _activeDriverPath = null;
    _socket = null;
    _pidFile = null;
    _manifestPath = null;
    _tempManifestPath = null;
    for (final path in [manifest, pidFile, tempManifest]) {
      if (path == null) continue;
      try {
        final file = File(path);
        if (file.existsSync()) file.deleteSync();
      } on Object {
        // No secret is stored in these files; startup uses a new unique name.
      }
    }
  }
}

const int _stillRunningExitCode = -2;

enum _ComputerUseRuntimePhase { idle, starting, prepared, running, stopping }
