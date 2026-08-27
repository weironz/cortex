@TestOn('vm')
library;

import 'dart:async';
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
    /// **一台连上就装死的 MCP server 不许挡着 agent 起来。**
    ///
    /// 2026-08-28 真机撞到：`McpHub::connect` 当时在启动路径上 `.await`，
    /// 而端口是在它之后才绑的。于是 agent 的就绪被「连上每一台第三方
    /// MCP server」挡着 —— 每台上限 60 秒且串行。拉起它的桌面端只等 20 秒，
    /// 超时就把「本地 agent 启动失败」给用户看，**而 agent 好好的**。
    ///
    /// 那次是靠开发机上真实的 MCP server 偶然撞出来的，红不红取决于机器
    /// 状态。这条把它变成确定的：一个「接了连接就一个字节都不回」的
    /// 监听器扮演那台 server —— 那正是最坏的情况，连得上，然后永远沉默。
    test(
      '一台装死的 MCP server 不挡启动',
      skip: exe == null
          ? '找不到 cortex-local 二进制，先 cargo build -p cortex-local'
          : null,
      () async {
        final stateDir = Directory.systemTemp.createTempSync(
          'cortex-mcp-hang-',
        );
        addTearDown(() => stateDir.deleteSync(recursive: true));

        // 接了就攥着不放，一个字节都不回
        final mute = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
        final held = <Socket>[];
        mute.listen(held.add);
        addTearDown(() async {
          for (final s in held) {
            s.destroy();
          }
          await mute.close();
        });

        final cfg = File('${stateDir.path}${Platform.pathSeparator}mcp.json')
          ..writeAsStringSync(
            '{"mcpServers":{"mute":'
            '{"type":"http","url":"http://127.0.0.1:${mute.port}/mcp"}}}',
          );

        final agent = LocalAgent(executable: exe!, stateDir: stateDir.path);
        addTearDown(agent.stop);

        final started = DateTime.now();
        final origin = await agent.start(
          remote: 'http://127.0.0.1:1',
          token: 'test-token-not-a-real-credential',
          extraEnv: {'CORTEX_MCP_CONFIG': cfg.path},
        );
        final waited = DateTime.now().difference(started);

        expect(origin, startsWith('http://127.0.0.1:'));
        // MCP 的连接上限是 60 秒（`CORTEX_MCP_TIMEOUT_SECS`）。就绪必须
        // 远早于它 —— 否则说明启动又被挂回那条路上了
        expect(
          waited.inSeconds,
          lessThan(15),
          reason:
              '起来花了 $waited —— MCP 连接又挡在启动路径上了。'
              '桌面端只等 20 秒，超时会把「本地 agent 启动失败」给用户看，'
              '而 agent 其实好好的，只是在等一台别人的进程',
        );

        // 而且它真的在服务，不是「早早报了个地址然后自己还没好」
        final res = await http.get(Uri.parse('$origin/health'));
        expect(res.statusCode, 200);
      },
    );

    test(
      'starts the real binary and reports the port it bound',
      skip: exe == null
          ? '找不到 cortex-local 二进制，先 cargo build -p cortex-local'
          : null,
      () async {
        final stateDir = Directory.systemTemp.createTempSync('cortex-agent-');
        addTearDown(() => stateDir.deleteSync(recursive: true));

        final agent = LocalAgent(executable: exe!, stateDir: stateDir.path);
        addTearDown(agent.stop);

        // ⚠️ **把 MCP 指到一份空配置上。**
        //
        // 不指的话，这个 agent 会去读**开发机上那份真实的 `mcp.json`** ——
        // 于是这条测试的耗时取决于「这台机器上装了哪些 MCP server、
        // 它们今天快不快」。2026-08-28 它就这么红过一次：一台 server 卡住，
        // agent 20 秒没报出地址，而代码一个字都没错。
        //
        // 一条会因为**机器状态**变红的测试，比没有这条测试更糟：
        // 它红的时候没人信，绿的时候也证明不了什么。
        final emptyMcp = File(
          '${stateDir.path}${Platform.pathSeparator}mcp.json',
        )..writeAsStringSync('{"mcpServers":{}}');

        // A remote that will never answer. The agent must still come up —
        // its whole offline story depends on that, and it means this test
        // needs no cortexd.
        final origin = await agent.start(
          remote: 'http://127.0.0.1:1',
          token: 'test-token-not-a-real-credential',
          extraEnv: {'CORTEX_MCP_CONFIG': emptyMcp.path},
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
          (body['server'] as Map)['reachable'],
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

    test(
      'a daemon that refuses to talk to us says so, in words',
      skip: exe == null ? '找不到 cortex-local 二进制' : null,
      () async {
        // 装了桌面端之后，本地 agent 与远端 cortexd 就各自独立升级了。
        // 两侧协议对不上时 agent **拒绝启动**（降级运行的表现是「某个功能
        // 悄悄不对」，比起不起来难查一个量级）。
        //
        // 但拒绝启动只完成了一半：拒绝的**理由**必须一路传到调用方。
        // 子进程的管道曾经是直接 drain 到空的，于是用户能看到的只有
        // 「本地 agent 没起来」，而真正那句「请升级本机这一侧」被扔掉了。
        final fake = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
        addTearDown(fake.close);
        unawaited(() async {
          await for (final req in fake) {
            req.response
              ..headers.contentType = ContentType.json
              // 一个「比我们新、且已经不再支持这么老的客户端」的 cortexd
              ..write('{"status":"ok","protocol":999,"min_peer_protocol":999}');
            await req.response.close();
          }
        }());

        final stateDir = Directory.systemTemp.createTempSync('cortex-agent-');
        addTearDown(() => stateDir.deleteSync(recursive: true));
        final agent = LocalAgent(executable: exe!, stateDir: stateDir.path);
        addTearDown(agent.stop);

        final err = await agent
            .start(
              remote: 'http://127.0.0.1:${fake.port}',
              token: 't',
              timeout: const Duration(seconds: 15),
            )
            .then<Object?>((_) => null, onError: (Object e) => e);

        expect(
          err,
          isA<LocalAgentException>(),
          reason: '协议不兼容时必须启动失败，而不是带着一个对不上的契约跑起来',
        );
        final msg = err.toString();
        expect(
          msg,
          contains('协议'),
          reason: '错误里必须点明是协议问题。只说「没起来」等于让人去猜：\n$msg',
        );
        expect(
          msg,
          contains('本机'),
          reason:
              '这个方向该让用户升**本机**这一侧，而这句话来自子进程的输出 ——'
              '它证明管道确实被读了，没有被 drain 掉：\n$msg',
        );
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
    final p =
        '..${Platform.pathSeparator}target'
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
