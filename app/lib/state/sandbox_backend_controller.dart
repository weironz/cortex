/// Windows 沙箱后端的选择 —— 桌面端界面这一侧。
///
/// # 两档是两种取舍，不是强弱
///
/// * **AppContainer**（默认）：读默认拒绝（强边界），`cargo build` / `git`
///   都能跑；`dir` / `vol` 与含 `cl.exe` 的 C/C++ 构建跑不了。
/// * **受限令牌**：完整工具链（cargo 拉依赖、git、curl 全通），但**不挡读** ——
///   读身份就是用户本人。写仍只限工作区。
///
/// 完整档案见 `docs/windows-sandbox.md`。
///
/// # 为什么是「启动期」而不是运行时开关
///
/// 与「远程接入」不同：那个有运行时路由（`/local/attach`），拨一下当场生效。
/// 后端不行 —— agent 进程在**启动时**读 `CORTEX_WIN_BACKEND`，而
/// `sandbox::capability()` 每进程只探测一次并缓存（那是有意的：「我到底
/// 在哪一档」在一次运行里必须有唯一答案）。所以换档 = 存设置 + **重启本机
/// agent**。拨动之后 UI 该如实说「已在重启，用新后端起来」。
///
/// # 只在 Windows 桌面端有意义
///
/// 别的平台没有这两档（Linux landlock / macOS seatbelt 各只有一种）。
/// Web 端根本没有本机 agent。所以这一节在非 Windows、非桌面上**整个不画** ——
/// 与「电脑操作」同一条纪律：做不到就别摆出来。
library;

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'app_providers.dart';

/// 设置里那个键。值是 `'restricted'` 或 `''`/缺席（= 默认 AppContainer）。
const String kSandboxBackendSetting = 'win_sandbox_backend';

/// 传给 agent 的环境变量名 —— 必须与 `cortex_agent::sandbox` 读的那个一致。
const String kSandboxBackendEnvVar = 'CORTEX_WIN_BACKEND';

/// 「受限令牌」这个取值 —— 必须与 Rust 侧 `windows_backend_is_restricted`
/// 比较的字面量一致（空串不算数，那是老 bug「空串顶掉默认」的形状）。
const String kRestrictedBackend = 'restricted';

/// 读存下来的后端偏好，转成给 `extraEnv` 的一对（或空 map）。
///
/// 只有选了受限令牌才注入；默认档不设变量 —— 让 agent 走它自己的默认，
/// 而不是塞一个空串（空串会被 Rust 侧的 `.trim().is_empty()` 挡掉，但
/// 「设了个空的」本身就是噪音，不如不设）。
Future<Map<String, String>> savedSandboxBackendEnv(Ref ref) =>
    sandboxBackendEnvFrom(ref.read(settingsReaderProvider));

/// 纯函数版：直接吃「读设置」那个闭包，不碰 Riverpod —— 好测。
Future<Map<String, String>> sandboxBackendEnvFrom(
  Future<Map<String, String>> Function() readSettings,
) async {
  try {
    final saved = await readSettings();
    if (saved[kSandboxBackendSetting] == kRestrictedBackend) {
      return {kSandboxBackendEnvVar: kRestrictedBackend};
    }
  } on Object catch (_) {
    // 读不出就当默认档 —— 比抛异常挡住 agent 启动安全得多
  }
  return const {};
}

/// 界面用：当前选的是不是受限令牌档。
final sandboxBackendProvider = NotifierProvider<SandboxBackendController, bool>(
  SandboxBackendController.new,
);

class SandboxBackendController extends Notifier<bool> {
  @override
  bool build() {
    // 先给个默认，再异步读回存下来的。中间那一瞬显示「默认档」不会误导 ——
    // 真值一到就刷新
    Future.microtask(() async {
      try {
        final saved = await ref.read(settingsReaderProvider)();
        state = saved[kSandboxBackendSetting] == kRestrictedBackend;
      } on Object catch (_) {
        state = false;
      }
    });
    return false;
  }

  /// 换档。存设置并**重启本机 agent** —— 后端是启动期读的（见库文档）。
  ///
  /// 存的是用户点的那个值（不像 attach 那样有服务端裁决）：这里没有第二个
  /// 权威，设置就是权威，agent 下次启动照它走。
  Future<void> setRestricted(bool restricted) async {
    await ref.read(settingsPatcherProvider)(
      kSandboxBackendSetting,
      restricted ? kRestrictedBackend : '',
    );
    state = restricted;
    // 重启 agent 让新后端生效。invalidate 那个 origin provider 会拉起
    // 新进程（见 `app_providers.dart` 的 `restart()`）—— 它 spawn 时会
    // 重新读 `savedSandboxBackendEnv`
    if (!kIsWeb) {
      ref.invalidate(localAgentOriginProvider);
    }
  }
}
