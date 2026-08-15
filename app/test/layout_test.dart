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

    /// 记忆那一格随记忆界面一起去了 Cormex，右栏现在只剩文件。
    ///
    /// **默认改成收起**，而不是默认开着文件面板：一个刚打开应用、还没绑工作区
    /// 的人，那一栏里什么都没有 —— 默认展开一个空面板只是白占三分之一屏。
    test('默认：左栏展开、右栏收起', () async {
      final c = boot();
      addTearDown(c.dispose);
      c.read(layoutProvider);
      await settle();

      final s = c.read(layoutProvider);
      expect(s.leftCollapsed, isFalse);
      expect(s.rightPanel, isNull);
    });

    test('存下来的状态在启动时被读回来', () async {
      final c = boot({'left_pane_collapsed': 'true', 'right_panel': 'files'});
      addTearDown(c.dispose);
      // **先读一次把 provider 造出来**，再 settle。provider 是懒的：
      // 不碰它，`build()` 就没跑，那个排 `_restore` 的微任务也就不存在，
      // settle 等的是一个还没被创建的东西
      c.read(layoutProvider);
      await settle();

      expect(c.read(layoutProvider).leftCollapsed, isTrue);
      expect(c.read(layoutProvider).rightPanel, RightPanel.files);
    });

    /// 存过 `memory` 的老用户 —— 那一格已经不存在了。
    ///
    /// **落到「收起」，而不是崩、也不是硬塞文件面板给他**：他上次选的是记忆，
    /// 而记忆现在在 Cormex 的 Web 端。给他一个空的右栏，要看文件他自己点。
    ///
    /// 这条比看起来重要：`_readRight` 里少写一个 case，`switch` 在一个已经
    /// 不存在的枚举值上会走 default —— 而 default 里随便返回什么都能编译。
    test('存过 memory 的老用户落到收起，不是崩也不是换成文件', () async {
      final c = boot({'right_panel': 'memory'});
      addTearDown(c.dispose);
      c.read(layoutProvider);
      await settle();
      expect(c.read(layoutProvider).rightPanel, isNull);
    });

    /// 更老的那把键（`memory_pane_visible`）同理不再参与判断：它表达的是
    /// 「记忆栏开没开」，而那个问题已经没有意义了。
    test('更老的 memory_pane_visible 也不再顶出一个面板', () async {
      final c = boot({'memory_pane_visible': 'true'});
      addTearDown(c.dispose);
      c.read(layoutProvider);
      await settle();
      expect(c.read(layoutProvider).rightPanel, isNull);
    });

    test('点已经开着的那个就收起', () async {
      final c = boot();
      addTearDown(c.dispose);
      c.read(layoutProvider);
      await settle();

      c.read(layoutProvider.notifier).selectRight(RightPanel.files);
      expect(c.read(layoutProvider).rightPanel, RightPanel.files);

      c.read(layoutProvider.notifier).selectRight(RightPanel.files);
      expect(
        c.read(layoutProvider).rightPanel,
        isNull,
        reason: '点已经开着的那个 = 收起 —— 用户不用去找第二个关闭入口',
      );
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
      c.read(layoutProvider.notifier).selectRight(RightPanel.files);
      await settle();

      expect(disk['permission_mode'], PermissionMode.bypass.wire);
      expect(disk['right_panel'], 'files');
    });
  });
}
