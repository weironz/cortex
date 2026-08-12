@TestOn('vm')
/// 下载与校验走**真的** I/O：真 HTTP 服务、真落盘、真算哈希。
///
/// # 为什么这一条不能用替身
///
/// 这段代码的价值全在「它拒绝装错东西」上，而拒绝的依据是一个流式算出来的
/// 哈希与一份文本文件里某一行的比对。用假的 client、假的文件系统去测，
/// 测的是我自己写的那份夹具，而**真正会出错的地方**（流没读完就算哈希、
/// 半截文件被当成下好了、失败时把坏文件留在磁盘上）一个都碰不到。
///
/// 真正只有真机能证明的只剩最后一步：拉起安装程序、替换正在运行的 exe。
library;

import 'dart:io';

import 'package:cortex_app/core/update_feed.dart';
import 'package:cortex_app/core/updater.dart';
import 'package:crypto/crypto.dart';
import 'package:http/http.dart' as http;
import 'package:flutter_test/flutter_test.dart';

void main() {
  late HttpServer server;
  late String origin;
  late List<int> payload;
  late String goodSum;

  /// 服务端要不要谎报校验和。
  var serveCorruptSum = false;

  /// 要不要在下载途中掐断。
  var truncate = false;

  setUp(() async {
    // 16 KiB 的确定性内容 —— 大到会分好几个 chunk，小到测试不慢
    payload = List<int>.generate(16 * 1024, (i) => (i * 31 + 7) % 256);
    goodSum = sha256.convert(payload).toString();
    serveCorruptSum = false;
    truncate = false;

    server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    origin = 'http://127.0.0.1:${server.port}';
    server.listen((req) async {
      switch (req.uri.path) {
        case '/setup.exe':
          req.response.headers.contentType = ContentType.binary;
          if (truncate) {
            // 报一个比实际发得多的长度，然后中途关掉 —— 复现「网断了」
            req.response.headers.contentLength = payload.length;
            req.response.add(payload.sublist(0, 1024));
            await req.response.flush();
            await req.response.close().catchError((_) {});
            return;
          }
          req.response.headers.contentLength = payload.length;
          req.response.add(payload);
        case '/SHA256SUMS':
          final sum = serveCorruptSum
              ? goodSum.replaceRange(0, 1, goodSum[0] == 'a' ? 'b' : 'a')
              : goodSum;
          req.response.write(
            '$sum  setup.exe\n'
            'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff  other.zip\n',
          );
        default:
          req.response.statusCode = 404;
      }
      await req.response.close();
    });
  });

  tearDown(() async => server.close(force: true));

  UpdateRelease release() => UpdateRelease(
    version: '9.9.9',
    setupUrl: '$origin/setup.exe',
    setupName: 'setup.exe',
    sumsUrl: '$origin/SHA256SUMS',
  );

  File tempFile() => File(
    '${Directory.systemTemp.path}${Platform.pathSeparator}'
    'cortex-update${Platform.pathSeparator}setup.exe',
  );

  test('下得下来，校验得过，落在磁盘上', () async {
    final client = http.Client();
    addTearDown(client.close);

    final seen = <double>[];
    final path = await downloadAndVerify(
      client,
      release(),
      onProgress: seen.add,
    );

    final f = File(path);
    expect(f.existsSync(), isTrue);
    expect(
      f.lengthSync(),
      payload.length,
      reason:
          '长度对不上说明流没读完就收工了 —— 而哈希紧接着也会对不上，'
          '排查方向会指向「服务端给错了」',
    );
    expect(seen, isNotEmpty, reason: '有 Content-Length 就该报进度');
    expect(seen.last, closeTo(1.0, 0.001));
    await f.delete();
  });

  test('校验和对不上：不装，而且把下载的文件删掉', () async {
    serveCorruptSum = true;
    final client = http.Client();
    addTearDown(client.close);

    await expectLater(
      downloadAndVerify(client, release()),
      throwsA(
        isA<UpdateException>().having(
          (e) => e.message,
          'message',
          contains('对不上'),
        ),
      ),
    );

    expect(
      tempFile().existsSync(),
      isFalse,
      reason:
          '留一个校验失败的 exe 在 %TEMP% 里，等于给下一次'
          '「反正已经下好了」留了个坑。安装包没有代码签名，'
          '这份校验是唯一能确认它没被换过的东西',
    );
  });

  test('下到一半断了：报错，且不留半截文件', () async {
    truncate = true;
    final client = http.Client();
    addTearDown(client.close);

    await expectLater(
      downloadAndVerify(client, release()),
      throwsA(isA<UpdateException>()),
    );
    expect(
      tempFile().existsSync(),
      isFalse,
      reason:
          '半截文件与下完的长得一模一样，只是短一点 —— '
          '留着它，下一次就可能拿它去装',
    );
  });

  test('SHA256SUMS 里没有这个文件名：一个字节都不下', () async {
    final client = http.Client();
    addTearDown(client.close);

    final r = UpdateRelease(
      version: '9.9.9',
      setupUrl: '$origin/setup.exe',
      setupName: 'not-listed.exe',
      sumsUrl: '$origin/SHA256SUMS',
    );
    await expectLater(
      downloadAndVerify(client, r),
      throwsA(
        isA<UpdateException>().having(
          (e) => e.message,
          'message',
          contains('无法确认'),
        ),
      ),
    );
  });

  test('安装开关一条都不能少 —— 每条都是真机上撞出来的', () {
    // 真机复现过的两次失败：
    //
    // 1. 没有 /SUPPRESSMSGBOXES：agent 还占着文件时，安装程序弹出
    //    「Setup was unable to automatically close all applications」
    //    三选一模态框**并停在那里等人点** —— 而这时 GUI 已经被关掉了，
    //    用户看到的是「程序没了，屏幕上有个看不懂的框」
    // 2. 没有 /FORCECLOSEAPPLICATIONS：RM 关掉了 GUI，却**没有**关掉
    //    控制台形态的 cortex-local.exe，于是它一直占着自己那个文件
    //
    // 两次的共同点是「装到一半停住，而应用已经关了」，也就是 roadmap 里
    // 那句「会下载但装不上」。删掉任何一条都不会有测试变红，除了这一条。
    expect(
      installerArgs,
      containsAll(<String>[
        '/VERYSILENT',
        '/SUPPRESSMSGBOXES',
        '/CLOSEAPPLICATIONS',
        '/FORCECLOSEAPPLICATIONS',
        '/RESTARTAPPLICATIONS',
        '/NORESTART',
      ]),
    );
  });

  test('拉起一个不存在的安装程序：明说，而不是静默什么都没发生', () async {
    await expectLater(
      launchInstaller('${Directory.systemTemp.path}/definitely-not-here.exe'),
      throwsA(isA<UpdateException>()),
    );
  });
}
