@TestOn('vm')
library;

import 'dart:convert';
import 'dart:io';

import 'package:cortex_app/core/local_agent.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;

/// Spawning the bundled agent — the one seam neither side's tests can cover
/// alone.
///
/// The Rust tests prove `cortex-local` binds and serves. The Dart tests prove
/// the provider wiring. **Neither proves the handshake between them**: the
/// argument names, the address file, the environment variable carrying the
/// token. Every one of those fails silently — the app just quietly keeps
/// talking to the remote and runs your tools on the server.
///
/// So this test starts the real binary.
void main() {
  final exe = _findAgent();

  group('LocalAgent', () {
    test(
      'starts the real binary and reports the port it bound',
      skip: exe == null
          ? '找不到 cortex-local 二进制，先 cargo build -p cortex-local'
          : null,
      () async {
        final stateDir = Directory.systemTemp.createTempSync('cortex-agent-');
        addTearDown(() => stateDir.deleteSync(recursive: true));

        final agent = LocalAgent(
          executable: exe!,
          stateDir: stateDir.path,
        );
        addTearDown(agent.stop);

        // A remote that will never answer. The agent must still come up —
        // its whole offline story depends on that, and it means this test
        // needs no cortexd.
        final origin = await agent.start(
          remote: 'http://127.0.0.1:1',
          token: 'test-token-not-a-real-credential',
        );

        expect(
          origin,
          startsWith('http://127.0.0.1:'),
          reason: '必须绑在 loopback 上 —— 它能执行命令，绑到别处等于把同网段放进来',
        );
        expect(
          origin,
          isNot(endsWith(':0')),
          reason: '报回来的必须是内核实际挑的端口，不是请求里那个 0',
        );

        // It is actually serving, and it is the agent (not something else that
        // happened to be on that port).
        final res = await http.get(Uri.parse('$origin/health'));
        expect(res.statusCode, 200);
        final body = jsonDecode(res.body) as Map<String, dynamic>;
        expect(body['role'], 'local-agent');
        expect(
          (body['memory'] as Map)['reachable'],
          false,
          reason: '远端是个死地址，agent 必须如实报告而不是假装连上了',
        );
      },
      timeout: const Timeout(Duration(seconds: 60)),
    );

    test(
      'the token travels in the environment, never on the command line',
      skip: exe == null ? '找不到 cortex-local 二进制' : null,
      () async {
        // 命令行对同机任意进程可见（tasklist /v、ps aux），还会被崩溃报告
        // 采集走。这条测试读**真实的**命令行，而不是信任源码里的写法
        final stateDir = Directory.systemTemp.createTempSync('cortex-agent-');
        addTearDown(() => stateDir.deleteSync(recursive: true));

        const secret = 'zzz-secret-must-not-appear-in-argv';
        final agent = LocalAgent(executable: exe!, stateDir: stateDir.path);
        addTearDown(agent.stop);
        await agent.start(remote: 'http://127.0.0.1:1', token: secret);

        final cmdlines = await _agentCommandLines();
        expect(
          cmdlines,
          isNotEmpty,
          reason: '查不到 cortex-local 的命令行，这条断言就没有意义了',
        );
        for (final line in cmdlines) {
          expect(
            line,
            isNot(contains(secret)),
            reason: 'token 出现在了命令行上 —— 同机任意进程都读得到',
          );
        }
      },
      timeout: const Timeout(Duration(seconds: 60)),
    );

    test('a missing binary fails loudly rather than hanging', () async {
      final stateDir = Directory.systemTemp.createTempSync('cortex-agent-');
      addTearDown(() => stateDir.deleteSync(recursive: true));

      final agent = LocalAgent(
        executable: '${stateDir.path}${Platform.pathSeparator}nope.exe',
        stateDir: stateDir.path,
      );
      expect(
        () => agent.start(remote: 'http://127.0.0.1:1', token: 't'),
        throwsA(isA<LocalAgentException>()),
        reason: '二进制不在时必须当场报错 —— 卡在那儿等超时会让登录挂住 20 秒',
      );
    });
  });
}

/// The debug build, which is what a developer has locally. Release too, in case
/// this runs right after packaging.
String? _findAgent() {
  final name = Platform.isWindows ? 'cortex-local.exe' : 'cortex-local';
  for (final profile in ['debug', 'release']) {
    // Tests run with CWD = app/, so the workspace target/ is one level up.
    final p = '..${Platform.pathSeparator}target'
        '${Platform.pathSeparator}$profile${Platform.pathSeparator}$name';
    if (File(p).existsSync()) return File(p).absolute.path;
  }
  return null;
}

/// Command lines of every running `cortex-local`.
Future<List<String>> _agentCommandLines() async {
  if (Platform.isWindows) {
    final r = await Process.run('powershell', [
      '-NoProfile',
      '-Command',
      "Get-CimInstance Win32_Process -Filter \"Name='cortex-local.exe'\" "
          '| ForEach-Object { \$_.CommandLine }',
    ]);
    return const LineSplitter()
        .convert(r.stdout.toString())
        .where((l) => l.trim().isNotEmpty)
        .toList();
  }
  final r = await Process.run('ps', ['-eo', 'args']);
  return const LineSplitter()
      .convert(r.stdout.toString())
      .where((l) => l.contains('cortex-local'))
      .toList();
}
