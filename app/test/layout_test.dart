/// 侧栏伸缩与账号栏。
///
/// # 为什么这些值得测
///
/// 账号栏那一行**没有名字时的四种情况长得一模一样**（都是「没有用户名」），
/// 而它们的含义完全不同：mock 是假数据、离线是没记忆、未启用认证是这个部署
/// 谁都能连、老服务端只是答不出。压成一句话，用户就没法从界面上分辨自己
/// 处在哪一种。
///
/// 伸缩那两个开关则是另一类：默认值错了不报错，只是「记忆栏怎么每次打开
/// 都是关的」。
library;

import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/features/shell/widgets/account_bar.dart';
import 'package:cortex_app/models/account.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _me = Account(userId: 'u1', username: 'willai', schemaName: 'cortex_u1');

void main() {
  group('账号栏显示什么', () {
    test('登录了就显示用户名', () {
      expect(
        accountLabel(
          account: _me,
          useMock: false,
          offline: false,
          authDisabled: false,
        ),
        'willai',
      );
    });

    test('mock 与离线由客户端自己知道，压过服务端答的任何东西', () {
      // 这两条排在前面是有意的：它们是**本地事实**，比一次可能过期的
      // /auth/me 更确定。反过来的话，切到 mock 之后账号栏还挂着上一个
      // 真账号的名字 —— 而屏幕上的数据全是假的
      expect(
        accountLabel(
          account: _me,
          useMock: true,
          offline: false,
          authDisabled: false,
        ),
        'Mock 数据源',
      );
      expect(
        accountLabel(
          account: _me,
          useMock: false,
          offline: true,
          authDisabled: false,
        ),
        '离线 · 无记忆',
      );
    });

    test('关掉认证的部署明说，而不是留空', () {
      expect(
        accountLabel(
          account: null,
          useMock: false,
          offline: false,
          authDisabled: true,
        ),
        '未启用认证',
      );
    });

    test('老服务端答不出 /auth/me 时说「已连接」，不是报错也不是空白', () {
      expect(
        accountLabel(
          account: null,
          useMock: false,
          offline: false,
          authDisabled: false,
        ),
        '已连接',
        reason:
            '连是连上了，只是没有名字。这一条不该让账号栏消失 —— '
            '它还挂着设置与退出登录',
      );
    });

    test('用户名是空白串等于没有名字', () {
      expect(
        accountLabel(
          account: const Account(userId: 'u', username: '   ', schemaName: 's'),
          useMock: false,
          offline: false,
          authDisabled: false,
        ),
        '已连接',
        reason:
            '空串顶掉默认值是这个仓库数了六次的形状。'
            '一个只有空格的用户名会让头像变成空白圆圈',
      );
    });

    test('头像取第一个字符，中文也要有', () {
      expect(avatarLetter('willai'), 'W');
      expect(avatarLetter('王小明'), '王', reason: '中文没有「首字母」这回事');
      expect(avatarLetter(''), '?');
    });
  });

  group('两侧折叠', () {
    late Map<String, String> disk;

    ProviderContainer boot([Map<String, String>? seed]) {
      disk = {...?seed};
      return ProviderContainer(
        overrides: [
          settingsReaderProvider.overrideWithValue(() async => Map.of(disk)),
          settingsWriterProvider.overrideWithValue(
            (v) async => disk = Map.of(v),
          ),
        ],
      );
    }

    Future<void> settle() async {
      for (var i = 0; i < 8; i++) {
        await Future<void>.delayed(Duration.zero);
      }
    }

    test('默认：左栏展开、记忆栏开着', () async {
      final c = boot();
      addTearDown(c.dispose);
      c.read(layoutProvider);
      await settle();

      final s = c.read(layoutProvider);
      expect(s.leftCollapsed, isFalse);
      expect(s.memoryVisible, isTrue, reason: '记忆是这个产品的主张。默认藏起来等于把它降级成一个可选功能');
    });

    test('没存过的时候记忆栏必须是开的 —— 别被「读不到」翻转成关', () async {
      // `saved['memory_pane_visible'] == 'true'` 写法的下场：第一次启动
      // （表里没有这个键）记忆栏就是关的，而没有人动过那个开关
      final c = boot();
      addTearDown(c.dispose);
      c.read(layoutProvider);
      await settle();
      expect(c.read(layoutProvider).memoryVisible, isTrue);
    });

    test('存下来的状态在启动时被读回来', () async {
      final c = boot({
        'left_pane_collapsed': 'true',
        'memory_pane_visible': 'false',
      });
      addTearDown(c.dispose);
      // **先读一次把 provider 造出来**，再 settle。provider 是懒的：
      // 不碰它，`build()` 就没跑，那个排 `_restore` 的微任务也就不存在，
      // settle 等的是一个还没被创建的东西
      c.read(layoutProvider);
      await settle();

      expect(c.read(layoutProvider).leftCollapsed, isTrue);
      expect(c.read(layoutProvider).memoryVisible, isFalse);
    });

    test('切一下就落盘，且不抹掉别的设置', () async {
      final c = boot({'base_url': 'http://box:9000'});
      addTearDown(c.dispose);
      c.read(layoutProvider);
      await settle();

      c.read(layoutProvider.notifier).toggleLeft();
      await settle();

      expect(disk['left_pane_collapsed'], 'true');
      expect(
        disk['base_url'],
        'http://box:9000',
        reason: '整表覆盖式写入会把地址抹掉 —— 那正是刚修完的那个 bug',
      );
    });

    test('与权限档互不干扰（两个都用 patcher）', () async {
      final c = boot();
      addTearDown(c.dispose);
      await settle();

      c.read(permissionModeProvider.notifier).set(PermissionMode.bypass);
      await settle();
      c.read(layoutProvider.notifier).toggleMemory();
      await settle();

      expect(disk['permission_mode'], PermissionMode.bypass.wire);
      expect(disk['memory_pane_visible'], 'false');
    });
  });
}
