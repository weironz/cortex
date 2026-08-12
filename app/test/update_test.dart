/// 自我更新：版本比较、feed 解析、校验和、以及「什么时候干脆不做」。
///
/// # 这一条链上唯一测得到的部分
///
/// 真正的下载与安装要碰磁盘和进程，只有真机能证明。但**决定要不要动手**的
/// 那几步全是纯逻辑，而它们恰好是出错了也不报错的那几步：版本比错了就是
/// 「永远没有更新」，校验和取错行就是「拿一个来路不明的 exe 去装」。
library;

import 'dart:convert';

import 'package:cortex_app/core/update_feed.dart';
import 'package:cortex_app/core/update_version.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('版本比较', () {
    test('0.1.10 比 0.1.9 新 —— 字符串比较会在这里翻车', () {
      expect(
        isNewer(current: '0.1.9', latest: '0.1.10'),
        isTrue,
        reason:
            "'0.1.9'.compareTo('0.1.10') 是正数（字典序里 '9' > '1'），"
            '照那个比法，版本号进位那天所有人都收不到更新，而且不报任何错',
      );
      expect(
        isNewer(current: '0.1.10', latest: '0.1.9'),
        isFalse,
        reason: '反过来更糟：会把用户往回降级',
      );
    });

    test('相同版本不算新', () {
      expect(isNewer(current: '0.1.7', latest: '0.1.7'), isFalse);
      expect(
        isNewer(current: '0.1.7', latest: 'v0.1.7'),
        isFalse,
        reason: 'GitHub 的 tag 带 v 前缀，剥不掉的话每次都认为有新版本 —— '
            '一个装完还提示更新的死循环',
      );
    });

    test('build number 与预发布标记不参与比较', () {
      expect(
        isNewer(current: '0.1.7+8', latest: '0.1.7+9'),
        isFalse,
        reason: '+N 是 Flutter 的 build number，同一版重打一次包也会变。'
            '让它参与比较等于「重新打个包就通知全体用户升级」',
      );
      expect(isNewer(current: '0.1.7-rc.1', latest: '0.1.7'), isFalse);
    });

    test('段数不同也能比：0.2 与 0.2.0 是同一版', () {
      expect(isNewer(current: '0.2', latest: '0.2.0'), isFalse);
      expect(isNewer(current: '0.2', latest: '0.2.1'), isTrue);
    });

    test('大版本压过小版本', () {
      expect(isNewer(current: '0.9.9', latest: '1.0.0'), isTrue);
      expect(isNewer(current: '1.0.0', latest: '0.9.9'), isFalse);
    });

    test('解析不出来一律当成「没有更新」，而不是崩或者乱升', () {
      expect(isNewer(current: '', latest: '0.1.8'), isFalse,
          reason: '空的当前版本 = 这份构建没传 CORTEX_APP_VERSION。'
              '此时提示更新，一点就把开发版覆盖成正式版');
      expect(isNewer(current: '0.1.7', latest: 'nightly'), isFalse);
      expect(isNewer(current: '0.1.7', latest: ''), isFalse);
      expect(parseVersion('0.1.7.5'), isNull, reason: '四段不是我们的版本形状');
      expect(parseVersion('abc'), isNull);
    });
  });

  group('feed 解析', () {
    Map<String, dynamic> release({
      String tag = 'v0.1.8',
      List<Map<String, String>>? assets,
    }) => {
      'tag_name': tag,
      'html_url': 'https://github.com/weironz/cortex/releases/tag/$tag',
      'assets':
          assets ??
          [
            {
              'name':
                  'cortex-desktop-v0.1.8-x86_64-pc-windows-msvc-setup.exe',
              'browser_download_url': 'https://example.invalid/setup.exe',
            },
            {
              'name': 'cortex-v0.1.8-x86_64-pc-windows-msvc.zip',
              'browser_download_url': 'https://example.invalid/cli.zip',
            },
            {
              'name': 'SHA256SUMS',
              'browser_download_url': 'https://example.invalid/SHA256SUMS',
            },
          ],
    };

    test('挑出安装程序与校验和，剥掉 v 前缀', () {
      final r = parseLatestRelease(release())!;
      expect(r.version, '0.1.8');
      expect(r.setupUrl, 'https://example.invalid/setup.exe');
      expect(
        r.setupName,
        'cortex-desktop-v0.1.8-x86_64-pc-windows-msvc-setup.exe',
        reason: '要拿这个名字去 SHA256SUMS 里找对应那一行，取错了就校验错东西',
      );
      expect(r.sumsUrl, 'https://example.invalid/SHA256SUMS');
    });

    test('没有 SHA256SUMS 就当作「没有可装的东西」', () {
      final r = parseLatestRelease(
        release(
          assets: [
            {
              'name': 'cortex-desktop-v0.1.8-x86_64-pc-windows-msvc-setup.exe',
              'browser_download_url': 'https://example.invalid/setup.exe',
            },
          ],
        ),
      );
      expect(
        r,
        isNull,
        reason: '安装包没有代码签名，校验和是唯一能确认它没被换过的东西。'
            '拿不到就不该装，而不是「那就不校验了」',
      );
    });

    test('只有 Linux 产物的 release 不提示更新', () {
      final r = parseLatestRelease(
        release(
          assets: [
            {
              'name': 'cortex-v0.1.8-x86_64-unknown-linux-gnu.tar.gz',
              'browser_download_url': 'https://example.invalid/linux.tar.gz',
            },
            {
              'name': 'SHA256SUMS',
              'browser_download_url': 'https://example.invalid/SHA256SUMS',
            },
          ],
        ),
      );
      expect(
        r,
        isNull,
        reason: '提示一个下不到的版本正是 roadmap 里写的「比没有更糟」',
      );
    });

    test('畸形 JSON 不崩', () {
      expect(parseLatestRelease(null), isNull);
      expect(parseLatestRelease('not an object'), isNull);
      expect(parseLatestRelease(<String, dynamic>{}), isNull);
      expect(parseLatestRelease(release(tag: '')), isNull);
    });
  });

  group('SHA256SUMS', () {
    const sums = '''
2b1c0f9a6e5d4c3b2a19087766554433221100ffeeddccbbaa99887766554433  cortex-v0.1.8-x86_64-pc-windows-msvc.zip
aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899  cortex-desktop-v0.1.8-x86_64-pc-windows-msvc-setup.exe
''';

    test('按文件名取对应那一行', () {
      expect(
        sha256For(sums, 'cortex-desktop-v0.1.8-x86_64-pc-windows-msvc-setup.exe'),
        'aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899',
      );
    });

    test('取的是安装程序那一行，不是文件里的第一行', () {
      final got = sha256For(
        sums,
        'cortex-desktop-v0.1.8-x86_64-pc-windows-msvc-setup.exe',
      );
      expect(
        got,
        isNot(startsWith('2b1c')),
        reason: '拿第一行去校验安装包，结果是「每次都校验失败」——'
            '而排查方向会全错，因为下载明明是好的',
      );
    });

    test('文件名不在里面时返回 null（= 校验失败，不是「跳过校验」）', () {
      expect(sha256For(sums, 'something-else.exe'), isNull);
    });

    test('二进制模式的 * 前缀也认', () {
      const withStar =
          '11112222333344445555666677778888999900001111222233334444555566aa *setup.exe';
      expect(sha256For(withStar, 'setup.exe'), isNotNull);
    });

    test('空文件与噪声行不崩', () {
      expect(sha256For('', 'x.exe'), isNull);
      expect(sha256For('# comment\n\n   \n', 'x.exe'), isNull);
    });
  });

  group('feed 抓取', () {
    test('JSON 走通 —— 与真实 GitHub 响应同形状', () {
      // 用真实字段名而不是随手编的，避免「我们自己的解析器与自己的夹具
      // 互相自洽，但与 GitHub 对不上」
      final body = jsonEncode({
        'tag_name': 'v0.1.9',
        'html_url': 'https://github.com/weironz/cortex/releases/tag/v0.1.9',
        'assets': [
          {
            'name': 'cortex-desktop-v0.1.9-x86_64-pc-windows-msvc-setup.exe',
            'browser_download_url': 'https://example.invalid/s.exe',
          },
          {
            'name': 'SHA256SUMS',
            'browser_download_url': 'https://example.invalid/S',
          },
        ],
      });
      final r = parseLatestRelease(jsonDecode(body));
      expect(r?.version, '0.1.9');
      expect(r?.notesUrl, contains('releases/tag/v0.1.9'));
    });
  });
}
