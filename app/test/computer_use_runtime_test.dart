import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:cortex_app/core/computer_use_runtime_io.dart';
import 'package:cortex_app/core/local_agent.dart';
import 'package:flutter_test/flutter_test.dart';

final String? _testCuaDriver = Platform.environment['CORTEX_CUA_DRIVER_PATH'];
final File _testLocalAgent = File(
  '${Directory.current.parent.path}${Platform.pathSeparator}'
  'target${Platform.pathSeparator}debug${Platform.pathSeparator}'
  'cortex-local.exe',
);

bool get _canRunRealCua =>
    Platform.isWindows && _testCuaDriver != null && _testCuaDriver!.isNotEmpty;

bool get _canRunLocalAgent => _canRunRealCua && _testLocalAgent.existsSync();

Future<void> _waitFor(
  bool Function() condition, {
  required String reason,
  Duration timeout = const Duration(seconds: 3),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail(reason);
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }
}

void main() {
  test('packaged manifest adds the frame host as a transport resource', () {
    const host = r'C:\Windows\System32\ApplicationFrameHost.exe';
    const executable = r'C:\Apps\CalculatorApp.exe';
    final manifest = buildCuaCapabilityManifest(
      executables: const [executable],
      packaged: const [
        (
          kind: 'aumid',
          identity: 'Microsoft.WindowsCalculator_8wekyb3d8bbwe!App',
          executable: executable,
        ),
      ],
      applicationFrameHost: host,
    );
    final resources = manifest['resources'] as Map<String, Object>;
    final apps = (resources['apps'] as List).cast<Map<String, Object>>();
    final allow = manifest['allow'] as Map<String, Object>;
    final tools = (allow['tools'] as List).cast<String>();

    expect(
      apps.where((entry) => entry['executable'] == host),
      hasLength(1),
      reason: 'UWP visible HWNDs belong to ApplicationFrameHost',
    );
    expect(
      apps.where(
        (entry) =>
            entry['bundle_id'] ==
            'Microsoft.WindowsCalculator_8wekyb3d8bbwe!App',
      ),
      hasLength(1),
      reason: 'the actual authorization must remain the packaged identity',
    );
    expect(
      tools,
      isNot(contains('list_windows')),
      reason:
          'Cortex enumerates exact PIDs with Win32 instead of granting Cua '
          'desktop-wide UIA enumeration',
    );
  });

  test('manifest does not grant the frame host without packaged apps', () {
    final manifest = buildCuaCapabilityManifest(
      executables: const [r'C:\Apps\Plain.exe'],
      packaged: const [],
      applicationFrameHost: r'C:\Windows\System32\ApplicationFrameHost.exe',
    );
    final resources = manifest['resources'] as Map<String, Object>;
    final apps = (resources['apps'] as List).cast<Map<String, Object>>();
    expect(
      apps.any(
        (entry) =>
            entry['executable'] ==
            r'C:\Windows\System32\ApplicationFrameHost.exe',
      ),
      isFalse,
      reason: 'the system host must not be granted as a general app resource',
    );
  });

  test(
    'bounded Cua sidecar starts from the bundled layout and stops cleanly',
    () async {
      final notepad =
          '${Platform.environment['WINDIR']}\\System32\\notepad.exe';
      final state = await Directory.systemTemp.createTemp('cortex-cua-test-');
      final runtime = ComputerUseRuntime(stateDir: state.path);
      addTearDown(() async {
        await runtime.stop();
        await state.delete(recursive: true);
      });

      final startedAt = DateTime.now();
      final env = await runtime.start(allowlist: [notepad]);
      final startupElapsed = DateTime.now().difference(startedAt);
      expect(
        env,
        isNotEmpty,
        reason:
            'pinned Cua Driver 应能启动 bounded daemon，而不是只存在文件。'
            '${runtime.lastError ?? ''}\n${runtime.diagnosticLog}',
      );
      expect(env['CORTEX_CUA_DRIVER_SHA256'], kCuaDriverSha256);
      expect(env['CORTEX_CUA_ALLOW_ACTIONS'], '1');
      expect(env['CORTEX_CUA_EXECUTABLE_ALLOWLIST'], notepad);
      expect(File(env['CORTEX_CUA_MANIFEST_PATH']!).existsSync(), isTrue);
      expect(runtime.isRunning, isTrue);
      expect(
        startupElapsed,
        lessThan(const Duration(seconds: 8)),
        reason:
            'sidecar 只负责 Computer Use；它必须有界地启动并降级，'
            '不能把桌面会话启动拖到状态查询的累计超时里。',
      );

      await runtime.stop();
      expect(runtime.isRunning, isFalse);
    },
    skip: !_canRunRealCua,
  );

  test('bounded Cua sidecar can restart after a clean stop', () async {
    final notepad = '${Platform.environment['WINDIR']}\\System32\\notepad.exe';
    final state = await Directory.systemTemp.createTemp('cortex-cua-restart-');
    final runtime = ComputerUseRuntime(stateDir: state.path);
    addTearDown(() async {
      await runtime.stop();
      await state.delete(recursive: true);
    });

    for (var cycle = 0; cycle < 2; cycle++) {
      final startedAt = DateTime.now();
      final env = await runtime.start(allowlist: [notepad]);
      expect(
        env,
        isNotEmpty,
        reason:
            '第 ${cycle + 1} 次启动失败：'
            '${runtime.lastError ?? ''}\n${runtime.diagnosticLog}',
      );
      expect(runtime.isRunning, isTrue);
      expect(
        DateTime.now().difference(startedAt),
        lessThan(const Duration(seconds: 8)),
      );
      await runtime.stop();
      expect(runtime.isRunning, isFalse);
    }
  }, skip: !_canRunRealCua);

  test('同一 runtime 的并发或重复启动不会创建第二套 daemon', () async {
    final notepad = '${Platform.environment['WINDIR']}\\System32\\notepad.exe';
    final state = await Directory.systemTemp.createTemp('cortex-cua-single-');
    final runtime = ComputerUseRuntime(stateDir: state.path);
    addTearDown(() async {
      await runtime.stop();
      await state.delete(recursive: true);
    });

    final first = runtime.start(allowlist: [notepad]);
    final concurrent = await runtime.start(allowlist: [notepad]);
    expect(concurrent, isEmpty, reason: 'starting 状态必须拒绝第二套 sidecar');
    final ready = await first;
    expect(ready, isNotEmpty, reason: runtime.lastError);
    final pid = runtime.processId;
    expect(pid, isNotNull);

    final repeated = await runtime.start(allowlist: [notepad]);
    expect(repeated, ready, reason: 'running 时重复调用应复用同一套环境');
    expect(runtime.processId, pid, reason: '重复调用不能替换进程句柄');
  }, skip: !_canRunRealCua);

  test('daemon 启动后意外退出会清除运行状态和 manifest', () async {
    final notepad = '${Platform.environment['WINDIR']}\\System32\\notepad.exe';
    final state = await Directory.systemTemp.createTemp('cortex-cua-crash-');
    final runtime = ComputerUseRuntime(stateDir: state.path);
    addTearDown(() async {
      await runtime.stop();
      await state.delete(recursive: true);
    });

    final env = await runtime.start(allowlist: [notepad]);
    expect(env, isNotEmpty, reason: runtime.lastError);
    final pid = runtime.processId;
    expect(pid, isNotNull);
    final manifest = File(env['CORTEX_CUA_MANIFEST_PATH']!);
    expect(manifest.existsSync(), isTrue);

    expect(Process.killPid(pid!), isTrue, reason: '测试必须能终止刚启动的 daemon');
    await _waitFor(
      () => !runtime.isRunning,
      reason: 'daemon 进程退出后 isRunning 仍为 true',
    );
    expect(runtime.lastError, contains('意外退出'));
    await _waitFor(
      () => !manifest.existsSync(),
      reason: 'daemon 意外退出后 manifest 没有清理',
    );
  }, skip: !_canRunRealCua);

  test(
    'local agent reports Computer Use after the desktop starts the bounded sidecar',
    () async {
      final notepad =
          '${Platform.environment['WINDIR']}\\System32\\notepad.exe';
      final state = await Directory.systemTemp.createTemp('cortex-cua-agent-');
      final runtime = ComputerUseRuntime(stateDir: state.path);
      final addrFile = File('${state.path}${Platform.pathSeparator}agent.addr');
      Process? agent;
      final logs = <String>[];
      final subscriptions = <StreamSubscription<void>>[];
      addTearDown(() async {
        agent?.kill();
        await agent?.exitCode.timeout(
          const Duration(seconds: 3),
          onTimeout: () => -1,
        );
        await runtime.stop();
        for (final subscription in subscriptions) {
          await subscription.cancel();
        }
        await state.delete(recursive: true);
      });

      final runtimeEnv = await runtime.start(allowlist: [notepad]);
      expect(runtimeEnv, isNotEmpty, reason: runtime.lastError);
      final env = <String, String>{
        ...runtimeEnv,
        'CORTEX_TOKEN': 'computer-use-integration-token',
        'CORTEX_BROWSER_ENABLE': '1',
        'CORTEX_BROWSER_SITE_ALLOWLIST': 'example.com',
      };
      agent = await Process.start(_testLocalAgent.path, [
        '--bind',
        '127.0.0.1:0',
        '--remote',
        'http://127.0.0.1:9',
        '--parent-pid',
        '$pid',
        '--addr-file',
        addrFile.path,
      ], environment: env);
      subscriptions.addAll([
        agent.stdout
            .transform(const Utf8Decoder(allowMalformed: true))
            .transform(const LineSplitter())
            .listen(logs.add),
        agent.stderr
            .transform(const Utf8Decoder(allowMalformed: true))
            .transform(const LineSplitter())
            .listen(logs.add),
      ]);

      String? address;
      for (var attempt = 0; attempt < 200; attempt++) {
        if (addrFile.existsSync()) {
          address = addrFile.readAsStringSync().trim();
          if (address.isNotEmpty) break;
        }
        if (await agent.exitCode.timeout(
              const Duration(milliseconds: 1),
              onTimeout: () => -1,
            ) !=
            -1) {
          fail('local agent exited before writing its address');
        }
        await Future<void>.delayed(const Duration(milliseconds: 100));
      }
      expect(
        address,
        isNotNull,
        reason:
            'local agent 没有写出地址。'
            'exit=${await agent.exitCode.timeout(const Duration(milliseconds: 1), onTimeout: () => -1)}\n'
            '${logs.join('\n')}',
      );

      final client = HttpClient();
      addTearDown(client.close);
      final request = await client.getUrl(Uri.parse('http://$address/health'));
      final response = await request.close();
      final body = await utf8.decoder.bind(response).join();
      final health = jsonDecode(body) as Map<String, dynamic>;

      expect(response.statusCode, 200, reason: body);
      expect(health['role'], 'local-agent');
      expect(
        health['computer_use'],
        isTrue,
        reason: 'app→agent 环境接线后 /health 必须看到 Computer Use：$body',
      );
      final capabilities =
          health['computer_capabilities'] as Map<String, dynamic>;
      expect(capabilities['observe'], isTrue);
      expect(capabilities['act'], isTrue);
      expect(capabilities['browser'], isTrue);
    },
    skip: !_canRunLocalAgent,
  );

  test('LocalAgent 记录脱敏的 Computer Use 配置准备事件', () async {
    final notepad = '${Platform.environment['WINDIR']}\\System32\\notepad.exe';
    final state = await Directory.systemTemp.createTemp(
      'cortex-cua-launch-log-',
    );
    final agent = LocalAgent(
      executable: _testLocalAgent.path,
      stateDir: state.path,
    );
    final emptyMcp = File('${state.path}${Platform.pathSeparator}mcp.json')
      ..writeAsStringSync('{"mcpServers":{}}');
    addTearDown(() async {
      await agent.stop();
      await state.delete(recursive: true);
    });

    final startedAt = DateTime.now();
    await agent.start(
      remote: 'http://127.0.0.1:1',
      token: 'computer-use-launch-log-token',
      computerUseAllowlist: [notepad],
      extraEnv: {'CORTEX_MCP_CONFIG': emptyMcp.path},
    );
    final startupElapsed = DateTime.now().difference(startedAt);
    expect(
      startupElapsed,
      lessThan(const Duration(seconds: 15)),
      reason:
          '驱动配置准备与 local agent 冷启动必须低于桌面端 20 秒上限；'
          '本次耗时 $startupElapsed',
    );

    final rows = File(agent.launchLogPath)
        .readAsLinesSync()
        .where((line) => line.trim().isNotEmpty)
        .map((line) => jsonDecode(line) as Map<String, dynamic>)
        .where((row) => row['ev'] == 'computer-use')
        .toList();
    expect(rows, hasLength(1), reason: '每次拉起 agent 都要有一条配置准备结果');
    final row = rows.single;
    expect(row['outcome'], 'prepared');
    expect(row['runtime_owner'], 'agent');
    expect(row['ms'], isA<int>());
    expect(row['ms'] as int, greaterThanOrEqualTo(0));
    expect(
      row.containsKey('error'),
      isFalse,
      reason: '配置准备成功只报告 prepared，不需要错误字段',
    );
    expect(
      jsonEncode(row),
      isNot(contains(notepad)),
      reason: '启动日志不能带出 allowlist 路径',
    );
  }, skip: !_canRunLocalAgent);

  test(
    'LocalAgent rejects an exhausted total startup budget before spawning',
    () async {
      final notepad =
          '${Platform.environment['WINDIR']}\\System32\\notepad.exe';
      final state = await Directory.systemTemp.createTemp('cortex-cua-budget-');
      final agent = LocalAgent(
        executable: _testLocalAgent.path,
        stateDir: state.path,
      );
      addTearDown(() async {
        await agent.stop();
        await state.delete(recursive: true);
      });

      final startedAt = DateTime.now();
      final error = await agent
          .start(
            remote: 'http://127.0.0.1:1',
            token: 'computer-use-budget-token',
            computerUseAllowlist: [notepad],
            timeout: const Duration(milliseconds: 1),
          )
          .then<Object?>((_) => null, onError: (Object error) => error);
      final elapsed = DateTime.now().difference(startedAt);

      expect(error, isA<LocalAgentException>());
      expect(error.toString(), contains('总预算'));
      expect(agent.isRunning, isFalse);
      final events = File(agent.launchLogPath)
          .readAsLinesSync()
          .where((line) => line.trim().isNotEmpty)
          .map((line) => jsonDecode(line) as Map<String, dynamic>)
          .map((row) => row['ev'])
          .toList();
      expect(
        events,
        isNot(contains('spawn')),
        reason: '预算在本地 agent 进程启动前已经耗尽，不能先拉起再补杀',
      );
      expect(
        elapsed,
        lessThan(const Duration(seconds: 3)),
        reason: '预算耗尽后的清理必须有界，不能重新变成无限的会话启动等待',
      );
    },
    skip: !_canRunLocalAgent,
  );
}
