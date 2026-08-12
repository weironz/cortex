import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:http/http.dart' as http;

import '../core/app_config.dart';
import '../core/update_feed.dart';
import '../core/update_version.dart';
import '../core/updater.dart';
import 'app_providers.dart';
import 'chat_controller.dart';

/// 更新走到哪一步了。
enum UpdatePhase {
  /// 没有可装的新版本 —— 包括「这份构建根本不谈更新」和「查过了，已是最新」。
  idle,

  /// 查到了新版本，等用户点。
  available,

  /// 正在下载。
  downloading,

  /// 下好、校验过了，只差把应用关掉。
  ready,

  /// 这一轮失败了。[UpdateState.error] 说清失败在哪一步。
  failed,
}

class UpdateState {
  const UpdateState({
    this.phase = UpdatePhase.idle,
    this.release,
    this.progress,
    this.error,
    this.installerPath,
    this.waitingForTurn = false,
  });

  final UpdatePhase phase;
  final UpdateRelease? release;

  /// 0..1；服务端没给长度时为 null（界面转不定进度的圈）。
  final double? progress;
  final String? error;
  final String? installerPath;

  /// 下好了，但正在流式回答，安装推迟到这一轮结束。
  final bool waitingForTurn;

  bool get hasUpdate =>
      phase == UpdatePhase.available ||
      phase == UpdatePhase.downloading ||
      phase == UpdatePhase.ready;

  UpdateState copyWith({
    UpdatePhase? phase,
    UpdateRelease? release,
    double? progress,
    String? error,
    String? installerPath,
    bool? waitingForTurn,
    bool clearProgress = false,
    bool clearError = false,
  }) => UpdateState(
    phase: phase ?? this.phase,
    release: release ?? this.release,
    progress: clearProgress ? null : (progress ?? this.progress),
    error: clearError ? null : (error ?? this.error),
    installerPath: installerPath ?? this.installerPath,
    waitingForTurn: waitingForTurn ?? this.waitingForTurn,
  );
}

/// 查更新、下更新、装更新。
///
/// # 什么都不做的两种情况，都必须是**彻底**不做
///
/// 1. 这份构建没有版本号（`--dart-define` 没传）—— 见 [AppConfig.appVersion]
/// 2. 这个平台装不了（Web、非 Windows 桌面）—— 见 `kUpdaterSupported`
///
/// 两种情况下 [UpdateState.phase] 恒为 idle，图标压根不出现。roadmap 里
/// 「每次开机提示一次，比没有更糟」说的就是这里松一寸的后果。
class UpdateController extends Notifier<UpdateState> {
  /// 上次查更新的时间戳（毫秒）。GitHub 未鉴权 API 有速率限制，而且
  /// 「有没有新版本」这件事一天知道一次足够了。
  static const String _kLastCheck = 'update_last_check';
  static const Duration _checkEvery = Duration(hours: 24);

  http.Client? _client;

  @override
  UpdateState build() {
    ref.onDispose(() => _client?.close());
    if (enabled) Future.microtask(_checkIfDue);
    return const UpdateState();
  }

  /// 这份构建 + 这个平台，谈不谈更新。
  bool get enabled => AppConfig.updatesSupported && kUpdaterSupported;

  http.Client get _http => _client ??= http.Client();

  /// 运行期的环境变量优先于编译期默认值。见 `updater_io.dart::feedOverride`。
  String get feedUrl => feedOverride() ?? AppConfig.updateFeed;

  Future<void> _checkIfDue() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final last = int.tryParse(saved[_kLastCheck] ?? '') ?? 0;
    final due = DateTime.now().millisecondsSinceEpoch - last;
    if (due < _checkEvery.inMilliseconds) return;
    await check();
  }

  /// 查一次。手动点「检查更新」也是这条。
  ///
  /// 失败**不进 failed 态**：一次后台查询连不上，不该在界面上留一个红图标。
  /// 只有用户主动发起的动作（下载、安装）失败了才值得显示。
  Future<void> check() async {
    if (!enabled) return;
    unawaited(
      ref.read(settingsPatcherProvider)(
        _kLastCheck,
        '${DateTime.now().millisecondsSinceEpoch}',
      ),
    );
    try {
      final latest = await fetchLatestRelease(_http, feedUrl);
      if (!ref.mounted || latest == null) return;
      if (!isNewer(current: AppConfig.appVersion, latest: latest.version)) {
        return;
      }
      state = state.copyWith(
        phase: UpdatePhase.available,
        release: latest,
        clearError: true,
      );
    } on UpdateException {
      // 见上：后台查询失败是安静的
    }
  }

  /// 下载 + 校验 + 装 + 退出。用户点那个图标走的就是这条。
  ///
  /// 从头到尾一步到位是用户明确要的（「点击即自动下载并安装更新重启」），
  /// 但**关掉应用**这一下会等：正在流式回答时先把那一轮放完。
  /// 中途重启会让那一轮丢在半路上，而下载本来就是这里最慢的一段，
  /// 等一等不影响「点一下就完事」的体验。
  Future<void> install() async {
    if (!enabled) return;
    final release = state.release;
    if (release == null) return;
    if (state.phase == UpdatePhase.downloading) return;

    state = state.copyWith(
      phase: UpdatePhase.downloading,
      clearProgress: true,
      clearError: true,
    );

    final String path;
    try {
      path = await downloadAndVerify(
        _http,
        release,
        onProgress: (f) {
          if (ref.mounted && state.phase == UpdatePhase.downloading) {
            state = state.copyWith(progress: f);
          }
        },
      );
    } on UpdateException catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(
        phase: UpdatePhase.failed,
        error: e.message,
        clearProgress: true,
      );
      return;
    }
    if (!ref.mounted) return;
    state = state.copyWith(
      phase: UpdatePhase.ready,
      installerPath: path,
      clearProgress: true,
    );
    await _installWhenIdle();
  }

  /// 等这一轮回答落地，再关掉应用。
  Future<void> _installWhenIdle() async {
    while (ref.mounted && ref.read(chatControllerProvider).streaming != null) {
      if (!state.waitingForTurn) {
        state = state.copyWith(waitingForTurn: true);
      }
      await Future<void>.delayed(const Duration(milliseconds: 400));
    }
    if (!ref.mounted) return;
    state = state.copyWith(waitingForTurn: false);

    final path = state.installerPath;
    if (path == null) return;
    try {
      // 先停本地 agent，**再**拉安装程序。安装包会同时替换
      // `cortex-local.exe`，而如果让 Restart Manager 看见它在跑，
      // `/RESTARTAPPLICATIONS` 可能把它当独立程序重新拉起来 —— 那时它没有
      // `--remote` / `--addr-file` / `--parent-pid`，起来就是个连不上远端、
      // 也没人管生死的孤儿 agent
      await ref.read(localAgentHandleProvider).stop();
      await launchInstaller(path);
    } on UpdateException catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(phase: UpdatePhase.failed, error: e.message);
      return;
    }
    quitForUpdate();
  }

  /// 失败之后再来一次。
  Future<void> retry() async {
    if (state.release == null) {
      await check();
      return;
    }
    state = state.copyWith(phase: UpdatePhase.available, clearError: true);
    await install();
  }
}

final updateControllerProvider =
    NotifierProvider<UpdateController, UpdateState>(UpdateController.new);
