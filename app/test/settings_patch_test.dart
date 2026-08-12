/// 存一个设置项不许把别的抹掉。
///
/// # 这是 0.1.7 刚修好的那个 bug 从另一扇门回来
///
/// `writeSettings` 是**整表覆盖**，而每个调用方都只写自己那一个键：
/// 存地址写 `{base_url: …}`，存权限档写 `{permission_mode: …}`。
/// 后写的那个把先写的抹掉。
///
/// 用户看到的是：改一下权限档，重启，服务器地址回到编译期默认值 ——
/// 而 0.1.7 的 CHANGELOG 里刚写着「修复：重启之后要重新登录、重填地址」。
/// 每加一个设置项，这种覆盖就多一对。
library;

import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  late Map<String, String> disk;

  ProviderContainer boot() {
    disk = {};
    return ProviderContainer(
      overrides: [
        settingsReaderProvider.overrideWithValue(() async => Map.of(disk)),
        settingsWriterProvider.overrideWithValue((values) async {
          // 与真 `writeSettings` 一样：**整表覆盖**，不合并
          disk = Map.of(values);
        }),
      ],
    );
  }

  Future<void> settle() async {
    for (var i = 0; i < 8; i++) {
      await Future<void>.delayed(Duration.zero);
    }
  }

  test('改权限档不许把 cortexd 地址抹掉', () async {
    final container = boot();
    addTearDown(container.dispose);

    container.read(appConfigProvider.notifier).setBaseUrl('http://box:9000');
    await settle();
    expect(disk['base_url'], 'http://box:9000', reason: '前置条件：地址存下来了');

    container.read(permissionModeProvider.notifier).set(PermissionMode.bypass);
    await settle();

    expect(
      disk['base_url'],
      'http://box:9000',
      reason:
          '把地址抹掉的话，用户重启后回到编译期默认地址 —— '
          '正是 0.1.7 刚修完的那个 bug，只是这次由「改权限档」触发',
    );
    expect(disk['permission_mode'], PermissionMode.bypass.wire);
  });

  test('反过来也一样：改地址不许把权限档抹掉', () async {
    final container = boot();
    addTearDown(container.dispose);

    container.read(permissionModeProvider.notifier).set(PermissionMode.bypass);
    await settle();
    container.read(appConfigProvider.notifier).setBaseUrl('http://box:9000');
    await settle();

    expect(
      disk['permission_mode'],
      PermissionMode.bypass.wire,
      reason:
          '权限档被悄悄退回「逐条确认」不会有人注意到 —— '
          '直到某天发现每条命令又开始问了',
    );
  });

  test('连着写好几个键，一个都不能丢', () async {
    final container = boot();
    addTearDown(container.dispose);
    final patch = container.read(settingsPatcherProvider);

    // 不 await，三个并发下去 —— 读—改—写之间那个窗口就是在这里暴露的
    final all = Future.wait([
      patch('a', '1'),
      patch('b', '2'),
      patch('c', '3'),
    ]);
    await all;
    await settle();

    expect(disk, {
      'a': '1',
      'b': '2',
      'c': '3',
    }, reason: '没有串行化的话，三个都读到空表，最后写的那个赢，前两个丢失');
  });
}
