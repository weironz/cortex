/// Windows 沙箱后端选择：存的设置怎么变成 agent 的启动环境。
///
/// 这一层薄，但两个点错了都不报错：
/// 1. **只有选了受限令牌才注入变量** —— 默认档必须是「不设」而不是「设空串」，
///    否则撞上 Rust 侧那条老 bug 的形状（空串顶掉默认，见
///    `windows_backend_is_restricted`）。
/// 2. 变量名与取值必须与 Rust 侧对得上 —— 对不上就是「开关拨了，agent 没听见」。
library;

import 'package:cortex_app/state/sandbox_backend_controller.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('选了受限令牌 → 注入 CORTEX_WIN_BACKEND=restricted', () async {
    final env = await sandboxBackendEnvFrom(
      () async => {kSandboxBackendSetting: kRestrictedBackend},
    );
    expect(env, {
      'CORTEX_WIN_BACKEND': 'restricted',
    }, reason: '变量名/取值任一与 Rust 侧对不上，就是开关拨了 agent 没听见');
  });

  test('默认档 → 一个变量都不设（不是设空串）', () async {
    for (final saved in [
      const <String, String>{}, // 从没选过
      {kSandboxBackendSetting: ''}, // 从受限档切回来
    ]) {
      final env = await sandboxBackendEnvFrom(() async => saved);
      expect(
        env,
        isEmpty,
        reason:
            '默认档必须「不设」而非「设空串」—— 空串顶掉默认是这个仓库记过账的 bug 形状。'
            '实际：$env（来自 $saved）',
      );
    }
  });

  test('读设置抛异常时安全退到默认档，不挡住 agent 启动', () async {
    final env = await sandboxBackendEnvFrom(
      () async => throw StateError('钥匙串炸了'),
    );
    expect(env, isEmpty, reason: '诊断/配置读失败绝不能让本机 agent 起不来 —— 退默认档是安全的那一侧');
  });
}
