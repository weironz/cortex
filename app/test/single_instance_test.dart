/// 同一个状态目录，同一时刻只许一份桌面端。
///
/// # 这道门防的是什么
///
/// 装机版与 dev 构建同时跑（窗口标题和图标一模一样，**用户根本看不出
/// 自己开了两个**），共用 `%LOCALAPPDATA%\cortex`：
///
/// - 两份进程各拿同一把 refresh token 去轮换，服务端一次性作废旧值 ——
///   **后轮换的把先轮换的作废**，输家此后每个请求 401；
/// - `settings.json` 是读-改-写，互相覆盖对方存的地址（「地址串台」）；
/// - `workspaces.json` / `outbox.mark` 只有进程内 Mutex，没有文件锁；
/// - 握手文件的清扫会删掉对方正在等的那个（已单独收窄，见
///   `local_agent_io.dart` 的 `_sweepStaleHandshakes`）。
///
/// 这些都不是「用户不该那么用」—— 是程序自己该守的门。
@TestOn('vm')
library;

import 'dart:io';

import 'package:cortex_app/core/single_instance_io.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  late Directory tmp;
  late String? savedLocalAppData;

  setUp(() {
    tmp = Directory.systemTemp.createTempSync('cortex-single');
    savedLocalAppData = Platform.environment['LOCALAPPDATA'];
  });
  tearDown(() {
    releaseSingleInstanceLockForTest();
    try {
      tmp.deleteSync(recursive: true);
    } on FileSystemException {
      // Windows 上句柄偶尔还没放干净，测试目录留着无害
    }
  });

  test('第一份拿得到名额', () {
    expect(
      ensureSingleInstance(),
      isTrue,
      reason: '这条红了说明**没有人**能启动 —— 防护把所有人都挡在了门外',
    );
  });

  test('放掉之后能再拿 —— 锁不是一次性的', () {
    expect(ensureSingleInstance(), isTrue);
    releaseSingleInstanceLockForTest();
    expect(
      ensureSingleInstance(),
      isTrue,
      reason:
          '上一份退出（进程死、锁被内核释放）之后，下一份必须能正常启动。'
          '拿不到的话，用户关掉应用就再也打不开了 —— 比原来的病更糟',
    );
  });

  test('锁文件落在状态目录里，与被保护的数据同处', () {
    ensureSingleInstance();
    final base = savedLocalAppData;
    if (base == null || base.isEmpty) return; // 非 Windows / 环境缺失，跳过
    expect(
      File(
        '$base${Platform.pathSeparator}cortex'
        '${Platform.pathSeparator}app.lock',
      ).existsSync(),
      isTrue,
      reason:
          '锁必须落在**它保护的那个目录**里。另起一处的话，两个构建可能'
          '各锁各的目录，而共享的数据仍然没人守 —— 等于没锁',
    );
  });
}
