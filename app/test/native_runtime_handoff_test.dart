@TestOn('vm')
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:cortex_app/core/computer_use_runtime_io.dart';
import 'package:cortex_app/core/local_agent_io.dart';
import 'package:crypto/crypto.dart';
import 'package:flutter_test/flutter_test.dart';

/// 只有假进程句柄；任何 Cua 进程调用都会记录并令测试失败。
class _AgentProcess extends Fake implements Process {
  final done = Completer<int>();
  int kills = 0;
  bool exitOnKill = true;
  bool failKill = false;
  void Function()? onKill;

  @override
  int get pid => 48201;
  @override
  Future<int> get exitCode => done.future;
  @override
  Stream<List<int>> get stdout => const Stream.empty();
  @override
  Stream<List<int>> get stderr => const Stream.empty();
  @override
  bool kill([ProcessSignal signal = ProcessSignal.sigterm]) {
    kills++;
    onKill?.call();
    if (failKill) throw StateError('注入退出未确认');
    if (exitOnKill) exit();
    return true;
  }

  void exit([int code = 0]) {
    if (!done.isCompleted) done.complete(code);
  }
}

/// 预算只在指定生命周期阶段推进，避免磁盘或调度负载抢先耗尽预算。
class _StartupClock extends Fake implements Stopwatch {
  @override
  Duration elapsed = Duration.zero;

  @override
  void start() => elapsed = Duration.zero;
}

class _Files {
  _Files() : root = Directory.systemTemp.createTempSync('native-handoff-') {
    driver = File('${root.path}/fake-driver.exe')..writeAsBytesSync([1, 2, 3]);
    app = File('${root.path}/allowed.exe')..writeAsBytesSync([4]);
    packaged = File('${root.path}/packaged.exe')..writeAsBytesSync([5]);
    agent = File('${root.path}/fake-agent.exe')..writeAsBytesSync([6]);
  }

  final Directory root;
  late final File driver;
  late final File app;
  late final File packaged;
  late final File agent;
  final cuaCalls = <List<String>>[];
  String get hash => sha256.convert(driver.readAsBytesSync()).toString();

  ComputerUseRuntime runtime({
    String? expectedHash,
    Future<List<int>> Function(String)? readDriverBytes,
    Future<File> Function(File, String)? writeManifest,
  }) => ComputerUseRuntime.forTesting(
    stateDir: root.path,
    driverPath: driver.path,
    expectedDriverSha256: expectedHash ?? hash,
    readDriverBytes: readDriverBytes,
    writeManifest: writeManifest,
    startProcess: (executable, arguments, {environment, workingDirectory}) {
      cuaCalls.add([executable, ...arguments]);
      throw StateError('准备流程不允许创建 Cua 子进程');
    },
  );

  List<File> get manifests => root
      .listSync(recursive: true)
      .whereType<File>()
      .where((f) => f.path.contains('manifest-'))
      .toList();
}

Future<void> _until(bool Function() condition) async {
  final clock = Stopwatch()..start();
  while (!condition()) {
    if (clock.elapsed > const Duration(seconds: 2)) fail('假进程生命周期没有按预期完成');
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
}

List<Map<String, dynamic>> _logs(LocalAgent agent) => File(agent.launchLogPath)
    .readAsLinesSync()
    .map((line) => jsonDecode(line) as Map<String, dynamic>)
    .toList();

void main() {
  group('Native 运行时配置移交', () {
    late _Files files;
    setUp(() => files = _Files());
    tearDown(() => files.root.deleteSync(recursive: true));

    test('prepare 生成原有 bounded 配置但不创建或探测 daemon', () async {
      final runtime = files.runtime();
      addTearDown(runtime.stop);
      final apps = [files.app.path, files.app.path, 'relative.exe'];
      final packaged = ['aumid|Example.App!Main|${files.packaged.path}'];
      final environment = await runtime.prepareForAgent(
        allowlist: apps,
        packagedAllowlist: packaged,
      );
      expect(environment, isNotEmpty, reason: runtime.lastError);
      expect(files.cuaCalls, isEmpty, reason: 'serve/status/stop 均不能调用');
      expect(runtime.isPrepared, isTrue);
      expect(runtime.isRunning, isFalse, reason: 'prepared 不能冒充 running');
      expect(runtime.processId, isNull);
      expect(environment['CORTEX_CUA_RUNTIME_OWNER'], 'agent');
      expect(environment['CORTEX_CUA_DRIVER_PATH'], files.driver.path);
      expect(environment['CORTEX_CUA_DRIVER_SHA256'], files.hash);
      expect(environment['CORTEX_CUA_EXECUTABLE_ALLOWLIST'], files.app.path);
      expect(environment['CORTEX_CUA_PACKAGED_APP_ALLOWLIST'], packaged.single);
      expect(environment['CORTEX_CUA_ALLOW_ACTIONS'], '1');
      expect(
        environment['CORTEX_CUA_DRIVER_SOCKET'],
        startsWith(r'\\.\pipe\cortex-cua-'),
      );
      expect(environment['CUA_DRIVER_RS_TELEMETRY_ENABLED'], 'false');
      expect(environment['CUA_TELEMETRY_ENABLED'], 'false');
      expect(environment['CUA_TELEMETRY'], '0');
      expect(environment['CUA_TELEMETRY_FORBID_NETWORK'], '1');
      expect(environment['DO_NOT_TRACK'], '1');
      final manifest = File(environment['CORTEX_CUA_MANIFEST_PATH']!);
      final frameHost =
          '${Platform.environment['SystemRoot'] ?? r'C:\Windows'}'
          '${Platform.pathSeparator}System32${Platform.pathSeparator}ApplicationFrameHost.exe';
      expect(
        jsonDecode(manifest.readAsStringSync()),
        buildCuaCapabilityManifest(
          executables: [files.app.path],
          packaged: [
            (
              kind: 'aumid',
              identity: 'Example.App!Main',
              executable: files.packaged.path,
            ),
          ],
          applicationFrameHost: File(frameHost).existsSync() ? frameHost : null,
        ),
        reason: '不能在托管移交时扩充原有应用、工具或生命周期权限',
      );
      expect(apps, [
        files.app.path,
        files.app.path,
        'relative.exe',
      ], reason: '不改调用方列表');
      expect(packaged, ['aumid|Example.App!Main|${files.packaged.path}']);
      expect(
        await runtime.prepareForAgent(
          allowlist: apps,
          packagedAllowlist: packaged,
        ),
        environment,
      );
      expect(
        await runtime.start(allowlist: apps),
        isEmpty,
        reason: '不能把已准备配置改成 legacy daemon',
      );
      expect(files.cuaCalls, isEmpty);

      final other = File('${manifest.parent.path}/other-instance.json')
        ..writeAsStringSync('保留');
      final rustPid = File('${manifest.parent.path}/rust-owned.pid')
        ..writeAsStringSync('保留');
      await Future.wait([runtime.stop(), runtime.stop()]);
      await runtime.stop();
      expect(manifest.existsSync(), isFalse);
      expect(other.existsSync(), isTrue, reason: '只清理本对象创建的配置');
      expect(rustPid.existsSync(), isTrue, reason: '不能删除 Rust 托管的状态文件');
      expect(runtime.isPrepared, isFalse);
      expect(runtime.isRunning, isFalse);
      expect(runtime.lastStopError, isNull);
      expect(files.cuaCalls, isEmpty, reason: 'prepared stop 也不能发驱动 stop');
    });

    test('SHA 不匹配不会准备可写配置，零预算与超时均不创建进程', () async {
      final invalid = files.runtime(expectedHash: 'not-the-driver-hash');
      expect(
        await invalid.prepareForAgent(allowlist: [files.app.path]),
        isEmpty,
      );
      expect(invalid.lastError, contains('SHA-256'));
      expect(files.manifests, isEmpty);
      final runtime = files.runtime();
      expect(
        await runtime.prepareForAgent(
          allowlist: [files.app.path],
          timeout: Duration.zero,
        ),
        isEmpty,
      );
      expect(runtime.isPrepared, isFalse);
      final bytes = Completer<List<int>>();
      final slow = files.runtime(readDriverBytes: (_) => bytes.future);
      final clock = Stopwatch()..start();
      expect(
        await slow.prepareForAgent(
          allowlist: [files.app.path],
          timeout: const Duration(milliseconds: 20),
        ),
        isEmpty,
      );
      expect(clock.elapsed, lessThan(const Duration(seconds: 2)));
      bytes.complete([1, 2, 3]);
      await slow.stop();
      expect(files.manifests, isEmpty);
      expect(files.cuaCalls, isEmpty);
    });

    test('准备超时后的迟到文件只清理本轮路径', () async {
      final release = Completer<void>();
      final writing = Completer<void>();
      final written = Completer<void>();
      var writes = 0;
      final runtime = files.runtime(
        readDriverBytes: (_) async => [1, 2, 3],
        writeManifest: (file, text) async {
          final delayed = writes++ == 0;
          if (delayed) {
            writing.complete();
            await release.future;
          }
          final created = await file.writeAsString(text, flush: true);
          if (delayed) written.complete();
          return created;
        },
      );
      final other = File('${files.root.path}/other-owned.json')
        ..writeAsStringSync('保留');
      final preparing = runtime.prepareForAgent(
        allowlist: [files.app.path],
        timeout: const Duration(milliseconds: 100),
      );
      await writing.future.timeout(const Duration(seconds: 2));
      expect(await preparing, isEmpty);
      await runtime.stop();
      final next = await runtime.prepareForAgent(allowlist: [files.app.path]);
      expect(next, isNotEmpty, reason: runtime.lastError);
      final nextManifest = File(next['CORTEX_CUA_MANIFEST_PATH']!);
      release.complete();
      await written.future;
      await _until(() => files.manifests.length == 1);
      expect(nextManifest.existsSync(), isTrue, reason: '上一轮迟到 I/O 不能删除新配置');
      expect(other.existsSync(), isTrue);
      expect(files.cuaCalls, isEmpty);
      expect(runtime.isPrepared, isTrue);
      expect(runtime.isRunning, isFalse);
      await runtime.stop();
    });

    test('准备期间 stop 会取消迟到结果，重启使用新的 manifest 与 socket', () async {
      final bytes = Completer<List<int>>();
      final reading = Completer<void>();
      final runtime = files.runtime(
        readDriverBytes: (_) {
          if (!reading.isCompleted) reading.complete();
          return bytes.future;
        },
      );
      final preparing = runtime.prepareForAgent(allowlist: [files.app.path]);
      await reading.future;
      await runtime.stop();
      bytes.complete([1, 2, 3]);
      expect(await preparing, isEmpty);
      expect(files.manifests, isEmpty);
      final first = await runtime.prepareForAgent(allowlist: [files.app.path]);
      await runtime.stop();
      final second = await runtime.prepareForAgent(allowlist: [files.app.path]);
      expect(
        first['CORTEX_CUA_MANIFEST_PATH'],
        isNot(second['CORTEX_CUA_MANIFEST_PATH']),
      );
      expect(
        first['CORTEX_CUA_DRIVER_SOCKET'],
        isNot(second['CORTEX_CUA_DRIVER_SOCKET']),
      );
      expect(runtime.isRunning, isFalse);
      await runtime.stop();
      expect(files.cuaCalls, isEmpty);
    });

    test('生产 agent 只消费 prepared env，退出前保留 manifest，重复 stop 共用等待', () async {
      final runtime = files.runtime();
      final process = _AgentProcess()..exitOnKill = false;
      late Map<String, String> agentEnvironment;
      late List<String> arguments;
      var spawns = 0;
      final agent = LocalAgent.forTesting(
        executable: files.agent.path,
        stateDir: files.root.path,
        computerUseFactory: ({required stateDir}) => runtime,
        startProcess:
            (executable, args, {environment, workingDirectory}) async {
              spawns++;
              expect(executable, files.agent.path);
              arguments = List.of(args);
              agentEnvironment = Map.of(environment!);
              File(
                args[args.indexOf('--addr-file') + 1],
              ).writeAsStringSync('127.0.0.1:48201');
              return process;
            },
      );
      final origin = await agent.start(
        remote: 'https://native-runtime.test/api',
        token: 'native-handoff-control-capability',
        computerUseAllowlist: [files.app.path],
        packagedComputerAllowlist: [
          'aumid|Example.App!Main|${files.packaged.path}',
        ],
        browserSiteAllowlist: ['example.com'],
        allowRemoteAttach: true,
        llmRoute: 'direct',
        extraEnv: {'CUSTOM_ENV': 'unchanged'},
      );
      expect(origin, 'http://127.0.0.1:48201');
      expect(arguments.take(6), [
        '--bind',
        '127.0.0.1:0',
        '--remote',
        'https://native-runtime.test/api',
        '--parent-pid',
        '$pid',
      ]);
      expect(arguments, contains('--allow-remote-attach'));
      expect(agentEnvironment['CORTEX_CUA_RUNTIME_OWNER'], 'agent');
      expect(agentEnvironment['CORTEX_LOCAL_LLM'], 'direct');
      expect(agentEnvironment['CORTEX_BROWSER_SITE_ALLOWLIST'], 'example.com');
      expect(agentEnvironment['CUSTOM_ENV'], 'unchanged');
      final row = _logs(
        agent,
      ).singleWhere((row) => row['ev'] == 'computer-use');
      expect(row['outcome'], 'prepared');
      expect(row['runtime_owner'], 'agent');
      expect(row['ms'], isA<int>());
      expect(jsonEncode(row), isNot(contains(files.app.path)));
      expect(runtime.isRunning, isFalse);
      final manifest = File(agentEnvironment['CORTEX_CUA_MANIFEST_PATH']!);
      process.onKill = () =>
          expect(manifest.existsSync(), isTrue, reason: 'kill 之前不能删配置');
      final firstStop = agent.stop();
      await _until(() => process.kills == 1);
      final secondStop = agent.stop();
      expect(identical(firstStop, secondStop), isTrue);
      expect(
        manifest.existsSync(),
        isTrue,
        reason: '尚未收到 exitCode，必须保留 manifest',
      );
      expect(runtime.isPrepared, isTrue);
      process.exit();
      await Future.wait([firstStop, secondStop]);
      expect(manifest.existsSync(), isFalse);
      expect(agent.isRunning, isFalse);
      await agent.stop();
      expect(process.kills, 1);
      expect(spawns, 1);
      expect(files.cuaCalls, isEmpty);
    });

    test('agent 退出未确认时不清 manifest，不启动新实例，重试 stop 后才清理', () async {
      final runtime = files.runtime();
      final process = _AgentProcess()..failKill = true;
      final agent = LocalAgent.forTesting(
        executable: files.agent.path,
        stateDir: files.root.path,
        computerUseFactory: ({required stateDir}) => runtime,
        startProcess: (_, args, {environment, workingDirectory}) async {
          File(
            args[args.indexOf('--addr-file') + 1],
          ).writeAsStringSync('127.0.0.1:48201');
          return process;
        },
      );
      await agent.start(
        remote: 'https://native-runtime.test',
        token: 'native-handoff-control-capability',
        computerUseAllowlist: [files.app.path],
      );
      final manifest = files.manifests.single;
      await agent.stop();
      expect(agent.lastCleanupError, contains('未确认'));
      expect(agent.isRunning, isTrue, reason: '句柄未退出不能假装停止成功');
      expect(manifest.existsSync(), isTrue);
      await expectLater(
        agent.start(
          remote: 'https://native-runtime.test',
          token: 'native-handoff-control-capability',
        ),
        throwsA(isA<LocalAgentException>()),
      );
      process.failKill = false;
      await agent.stop();
      expect(manifest.existsSync(), isFalse);
      expect(files.cuaCalls, isEmpty);
    });

    test('agent 意外退出后才清准备配置，准备失败不妨碍普通 agent 启动', () async {
      for (final valid in [true, false]) {
        final runtime = files.runtime(expectedHash: valid ? null : 'mismatch');
        final process = _AgentProcess();
        var exits = 0;
        final agent = LocalAgent.forTesting(
          executable: files.agent.path,
          stateDir: files.root.path,
          computerUseFactory: ({required stateDir}) => runtime,
          startProcess: (_, args, {environment, workingDirectory}) async {
            expect(environment!.containsKey('CORTEX_CUA_RUNTIME_OWNER'), valid);
            File(
              args[args.indexOf('--addr-file') + 1],
            ).writeAsStringSync('127.0.0.1:48201');
            return process;
          },
        );
        await agent.start(
          remote: 'https://native-runtime.test',
          token: 'native-handoff-control-capability',
          computerUseAllowlist: [files.app.path],
          onExit: (_, _) => exits++,
        );
        expect(agent.isRunning, isTrue);
        process.exit(17);
        await _until(() => exits == 1 && files.manifests.isEmpty);
        expect(agent.isRunning, isFalse);
        await agent.stop();
      }
      expect(files.cuaCalls, isEmpty);
    });

    test('spawn 前总预算耗尽时不创建 agent', () async {
      final runtime = files.runtime();
      final startupClock = _StartupClock();
      const budget = Duration(milliseconds: 150);
      var spawns = 0;
      final agent = LocalAgent.forTesting(
        executable: files.agent.path,
        stateDir: files.root.path,
        startupClock: () => startupClock,
        computerUseFactory: ({required stateDir}) {
          startupClock.elapsed = budget;
          return runtime;
        },
        startProcess: (_, _, {environment, workingDirectory}) {
          spawns++;
          throw StateError('预算耗尽后不能调用 OS 创建进程');
        },
      );
      for (final timeout in [Duration.zero, budget]) {
        final clock = Stopwatch()..start();
        await expectLater(
          agent.start(
            remote: 'https://native-runtime.test',
            token: 'native-handoff-control-capability',
            timeout: timeout,
          ),
          throwsA(isA<LocalAgentException>()),
        );
        expect(clock.elapsed, lessThan(const Duration(seconds: 2)));
        expect(spawns, 0);
        expect(files.manifests, isEmpty);
      }
      await agent.stop();
      expect(files.cuaCalls, isEmpty);
    });

    test('pending spawn 总预算耗尽时，迟到的进程退出前仍保留 manifest', () async {
      final runtime = files.runtime();
      // 先完成真实 SHA 与 manifest I/O；start 复用 prepared 配置，
      // 只在假 OS 创建调用已经进入后耗尽预算，确保覆盖迟到句柄分支。
      final prepared = await runtime.prepareForAgent(
        allowlist: [files.app.path],
      );
      expect(prepared, isNotEmpty, reason: runtime.lastError);
      final manifest = File(prepared['CORTEX_CUA_MANIFEST_PATH']!);
      final startupClock = _StartupClock();
      const budget = Duration(milliseconds: 150);
      var spawns = 0;
      final spawned = Completer<Process>();
      final agent = LocalAgent.forTesting(
        executable: files.agent.path,
        stateDir: files.root.path,
        startupClock: () => startupClock,
        computerUseFactory: ({required stateDir}) => runtime,
        startProcess: (_, _, {environment, workingDirectory}) {
          spawns++;
          expect(environment!['CORTEX_CUA_MANIFEST_PATH'], manifest.path);
          expect(manifest.existsSync(), isTrue);
          startupClock.elapsed = budget;
          return spawned.future;
        },
      );
      final clock = Stopwatch()..start();
      await expectLater(
        agent.start(
          remote: 'https://native-runtime.test',
          token: 'native-handoff-control-capability',
          computerUseAllowlist: [files.app.path],
          timeout: budget,
        ),
        throwsA(
          isA<LocalAgentException>().having(
            (error) => error.message,
            '超时阶段',
            '本地 agent 进程创建超过总启动预算',
          ),
        ),
      );
      expect(clock.elapsed, lessThan(const Duration(seconds: 2)));
      expect(spawns, 1);
      expect(files.manifests.single.path, manifest.path);
      await agent.stop();
      expect(manifest.existsSync(), isTrue, reason: 'OS 创建调用仍可能返回迟到句柄');
      final late = _AgentProcess()
        ..exitOnKill = false
        ..onKill = () =>
            expect(manifest.existsSync(), isTrue, reason: '迟到句柄 kill 前不能删配置');
      spawned.complete(late);
      await _until(() => late.kills == 1);
      expect(late.done.isCompleted, isFalse);
      expect(manifest.existsSync(), isTrue, reason: '迟到进程退出尚未确认，必须保留配置');
      late.exit();
      await _until(() => !manifest.existsSync());
      await agent.stop();
      expect(agent.isRunning, isFalse);
      expect(files.cuaCalls, isEmpty);
    });
  }, skip: !Platform.isWindows ? '验证 Windows allowlist 与专用命名管道，仅用假进程' : false);
}
