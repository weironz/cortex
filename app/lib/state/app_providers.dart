/// ## Why Riverpod
///
/// Three things decided it over Provider and Bloc:
///
/// 1. **The mock/live swap is the app's central seam.** With Riverpod the
///    entire data source is one `Provider` whose body reads config; every
///    consumer is invalidated automatically when it flips. With `Bloc` the same
///    swap means re-wiring repositories through constructors of a dozen blocs.
/// 2. **`select` gives bubble-level rebuild isolation for free.** Streaming
///    text updates ~60×/s; the conversation list must not rebuild at that rate.
///    `ref.watch(p.select((s) => s.streamingText))` confines the rebuild to the
///    one widget that reads it — the same thing with `ChangeNotifierProvider`
///    needs hand-rolled `Selector`s, and with Bloc needs `buildWhen` on every
///    `BlocBuilder`.
/// 3. **No `BuildContext` for reads.** Controllers can react to each other
///    (session switch → cancel in-flight stream) without a widget in scope.
///
/// Cost accepted: `NotifierProvider` boilerplate is wordier than
/// `ChangeNotifier`, and Riverpod 3's generics show up in error messages. Both
/// are one-time costs; the rebuild isolation is a per-frame benefit.
library;

import '../core/permission_mode.dart';
import 'dart:async';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/cortex_api.dart';
import '../api/http_cortex_api.dart';
import '../api/mock_cortex_api.dart';
import '../auth/local_llm_store.dart';
import '../core/app_config.dart';
import '../core/local_llm.dart';
import '../core/settings_store.dart';
import '../core/local_agent.dart';
import '../models/account.dart';
import '../models/health_status.dart';
import 'auth_controller.dart';

/// Mutable runtime config. Seeded from `--dart-define`, editable in settings.
/// 读取跨重启的非密设置。做成 provider 是为了能在测试里换掉 ——
/// 真实现要碰磁盘 / localStorage。
final settingsReaderProvider = Provider<Future<Map<String, String>> Function()>(
  (ref) => readSettings,
);

/// 同上，写入侧。**整表覆盖** —— 想改一个键请用 [settingsPatcherProvider]。
final settingsWriterProvider =
    Provider<Future<void> Function(Map<String, String>)>(
      (ref) => writeSettings,
    );

/// 只改一个键，其余原样留着。
///
/// # 为什么必须有这个东西
///
/// [settingsWriterProvider] 是整表覆盖，而调用方各存各的一个键：
/// `writeSettings({'base_url': …})` 与 `writeSettings({'permission_mode': …})`
/// **互相抹掉**。症状是「改了权限档之后，重启回到默认服务器地址」——
/// 而 0.1.7 刚刚才修完「重启要重填地址」，等于从另一扇门又放了回来。
///
/// 每加一个设置项，这种覆盖就多一对。所以合并这件事必须在一个地方做完，
/// 而不是指望每个调用方都记得先读再写。
///
/// # 串行化
///
/// 读—改—写之间有个窗口：两个并发的 patch 会各自读到旧表，后写的赢，
/// 先写的那个键丢失。用户手速达不到，但「切后端时批量恢复设置」这类代码
/// 一次发好几个是很自然的。用一条 future 链排队，代价是零。
final settingsPatcherProvider = Provider<Future<void> Function(String, String)>(
  (ref) {
    var queue = Future<void>.value();
    return (key, value) {
      queue = queue.then((_) async {
        final current = await ref.read(settingsReaderProvider)();
        await ref.read(settingsWriterProvider)({...current, key: value});
      });
      return queue;
    };
  },
);

class AppConfigNotifier extends Notifier<AppConfig> {
  /// 上一次用的地址在**磁盘**上，读它要异步；而 `build` 是同步的。
  ///
  /// 所以先给编译期默认值，再排一个微任务把存下来的读回来。
  /// 中间那一瞬间用默认地址不会造成可见的错误：认证控制器的探测
  /// 也排在微任务里，且 baseUrl 变化会触发它重来。
  ///
  /// **在此之前这个类完全活在内存里** —— 重启之后地址回到编译期默认值，
  /// 而一个把 cortexd 部署在别处的人每次打开都要重填一遍。
  @override
  AppConfig build() {
    Future.microtask(_restore);
    return AppConfig.initial;
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final url = saved[_kBaseUrl]?.trim();
    if (url == null || url.isEmpty || url == state.baseUrl) return;
    state = state.copyWith(baseUrl: url);
  }

  static const String _kBaseUrl = 'base_url';

  /// 落盘。失败只是「下次要重填」，不打断任何事（见 `writeSettings`）。
  void _persist() {
    unawaited(ref.read(settingsPatcherProvider)(_kBaseUrl, state.baseUrl));
  }

  void setUseMock(bool value) {
    if (state.useMock == value) return;
    state = state.copyWith(useMock: value);
  }

  void setBaseUrl(String value) {
    final trimmed = value.trim();
    if (trimmed.isEmpty || state.baseUrl == trimmed) return;
    state = state.copyWith(baseUrl: trimmed);
    _persist();
  }

  /// 进／出离线模式。
  ///
  /// 离开时把 mock 也一并关掉：两者都是「不连真后端」的形态，而同时开着
  /// 会让人分不清眼前这条对话到底是真是假。
  void setOffline(bool value) {
    if (state.offline == value) return;
    state = state.copyWith(offline: value, useMock: false);
  }
}

/// 这台设备上选的权限档位。
///
/// # 为什么按设备持久化，而不是按会话
///
/// 它是**一个人的工作习惯**（「我信得过它，别老打断我」），不是某一次对话的
/// 属性。每开一个会话都要重选一遍，人的反应是永远停在默认档 —— 那等于这个
/// 开关不存在。Claude Code 的菜单里那个 "Default ✓" 就是同一个判断。
///
/// # 为什么默认档不持久化也没关系
///
/// 读不出来就是 [PermissionMode.ask]，也就是最谨慎的一档。这个方向刻意选死：
/// 一个读坏的配置文件不该静默把 agent 变成无人值守的。
/// 当前登录的是谁。null = 没有名字可显示（见 [CortexApi.whoAmI]）。
///
/// 跟着 `cortexApiProvider` 走：换后端 / 换账号时自动重来。失败也回 null ——
/// 账号栏不该因为一次查询失败就变成一条报错，它还挂着设置与退出登录。
final accountProvider = FutureProvider<Account?>((ref) async {
  final api = ref.watch(cortexApiProvider);
  try {
    return await api.whoAmI();
  } on Object {
    return null;
  }
});

/// 两侧面板收起没有。
class LayoutState {
  const LayoutState({this.leftCollapsed = false, this.memoryVisible = true});

  /// 会话 + 工作区那一栏。收起时账号栏也跟着不见 —— 与 Codex / Claude
  /// 桌面端一致：收起就是整条侧栏都没了，要用就展开。
  final bool leftCollapsed;

  /// 记忆栏。默认**开着**：记忆是这个产品的主张，藏起来等于把它变成
  /// 一个可选功能。
  final bool memoryVisible;

  LayoutState copyWith({bool? leftCollapsed, bool? memoryVisible}) =>
      LayoutState(
        leftCollapsed: leftCollapsed ?? this.leftCollapsed,
        memoryVisible: memoryVisible ?? this.memoryVisible,
      );
}

/// 两侧折叠状态，跨重启记住。
///
/// # 为什么是 provider 而不是 `_AppShellState` 的字段
///
/// 读它的地方分散在三处：伸缩按钮在中间栏的 header 里、账号栏在左栏底部、
/// 真正的布局在 `AppShell`。用回调层层往下传，`AppShell` 会退化成一个
/// 纯粹的参数中转站，而每加一个要读布局的组件就多穿一层。
///
/// # 为什么要持久化
///
/// 收起侧栏的人是**为了腾地方**，而不是为了这一次。每次打开都弹回来，
/// 等于这个开关只在当前这个窗口有效 —— 两家参考产品都记住它。
class LayoutNotifier extends Notifier<LayoutState> {
  static const String _kLeft = 'left_pane_collapsed';
  static const String _kMemory = 'memory_pane_visible';

  @override
  LayoutState build() {
    Future.microtask(_restore);
    return const LayoutState();
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    state = LayoutState(
      leftCollapsed: saved[_kLeft] == 'true',
      // 缺省是**开着**，所以只有显式的 'false' 才关。写成
      // `saved[_kMemory] == 'true'` 的话，第一次启动（没有这个键）
      // 记忆栏就是关的 —— 一个由「还没存过」造成的默认值反转
      memoryVisible: saved[_kMemory] != 'false',
    );
  }

  void toggleLeft() {
    state = state.copyWith(leftCollapsed: !state.leftCollapsed);
    unawaited(
      ref.read(settingsPatcherProvider)(_kLeft, '${state.leftCollapsed}'),
    );
  }

  void toggleMemory() {
    state = state.copyWith(memoryVisible: !state.memoryVisible);
    unawaited(
      ref.read(settingsPatcherProvider)(_kMemory, '${state.memoryVisible}'),
    );
  }
}

final layoutProvider = NotifierProvider<LayoutNotifier, LayoutState>(
  LayoutNotifier.new,
);

class PermissionModeNotifier extends Notifier<PermissionMode> {
  static const String _key = 'permission_mode';

  @override
  PermissionMode build() {
    Future.microtask(_restore);
    return PermissionMode.ask;
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final mode = PermissionMode.fromWire(saved[_key]);
    if (mode != state) state = mode;
  }

  void set(PermissionMode mode) {
    if (state == mode) return;
    state = mode;
    unawaited(ref.read(settingsPatcherProvider)(_key, mode.wire));
  }
}

final permissionModeProvider =
    NotifierProvider<PermissionModeNotifier, PermissionMode>(
      PermissionModeNotifier.new,
    );

/// 这一轮要不要在**云端沙箱**里跑。
///
/// # 为什么它是一个开关而不是自动的
///
/// 起一个沙箱容器要占几百 MB 内存。绝大多数对话不需要文件与命令 ——
/// 「帮我想一下这段话怎么写」不该顺手拉起一个容器。
///
/// # 为什么只在 Web 端有意义
///
/// 桌面端的 agent 跑在**用户自己的机器上**（`cortex-local` 直连），压根不经
/// cortexd 的 `/chat`。给桌面端也放一个开关，等于给一个不存在的东西做界面。
/// 见 [kLocalAgentSupported] 与 `docs/sandbox.md`。
///
/// **不持久化**：与权限档不同，这一条的代价（一个容器）是即时可感的，
/// 而「上次开着这次也开着」会让人在完全不需要文件的对话里白拉一个容器。
class SandboxNotifier extends Notifier<bool> {
  @override
  bool build() => false;

  void set(bool on) {
    if (state != on) state = on;
  }

  void toggle() => set(!state);
}

final sandboxProvider = NotifierProvider<SandboxNotifier, bool>(
  SandboxNotifier.new,
);

final appConfigProvider = NotifierProvider<AppConfigNotifier, AppConfig>(
  AppConfigNotifier.new,
);

/// 本机 LLM 配置（离线模式用）。见 [`LocalLlmConfig`]。
///
/// 异步加载：读系统凭据库要跨进程。加载完成前是 `empty` —— 那期间
/// 本地 agent 起不来是**对的**，不该拿一个残缺的配置去启动它然后失败。
class LocalLlmNotifier extends AsyncNotifier<LocalLlmConfig> {
  @override
  Future<LocalLlmConfig> build() => readLocalLlm();

  /// 存进凭据库并让本地 agent 用上新配置。
  ///
  /// # Errors
  /// 存不进去时抛给调用方 —— 用户刚点了保存，静默失败会让他以为配好了
  Future<void> save(LocalLlmConfig config) async {
    await writeLocalLlm(config);
    state = AsyncData(config);
  }

  Future<void> clear() async {
    await clearLocalLlm();
    state = const AsyncData(LocalLlmConfig.empty);
  }
}

final localLlmProvider =
    AsyncNotifierProvider<LocalLlmNotifier, LocalLlmConfig>(
      LocalLlmNotifier.new,
    );

/// The bundled agent's origin, or null when this build runs without one.
///
/// ## Why it starts only after sign-in
///
/// The agent needs the remote URL *and* the token — it proxies to `cortexd` on
/// the app's behalf. Starting it earlier would mean holding a credential the
/// user has not yet proven is valid.
///
/// The token check itself must hit the **remote**, which is why
/// [AuthController.signIn] probes `config.baseUrl` rather than the agent: the
/// agent validates inbound requests against the very token the app handed it,
/// so asking it "is this token good?" proves nothing.
///
/// ## null is a normal outcome, not an error
///
/// No binary beside the app (a `flutter run` build), Web, or an agent that
/// refuses to start — all fall back to talking to the remote directly. That
/// costs local tools, not the app. Failing sign-in over it would turn a
/// degraded feature into a total outage.
///
/// ## Disposal kills it
///
/// Signing out or switching backends disposes this provider, which stops the
/// agent. That matters: it holds the token and can execute commands, so it must
/// not outlive the session that authorised it.
/// How many consecutive deaths before we stop trying.
///
/// Some failures never get better by retrying — a protocol mismatch with the
/// remote, a corrupt binary, a port the OS refuses. Restarting those forever
/// burns CPU and buries the real error under a wall of identical log lines.
/// Give up, stay on the remote, and leave the reason in the log.
const _maxConsecutiveRestarts = 5;

/// Live this long and the run counts as healthy — the failure budget resets.
///
/// Without this, an agent that dies once a day would eventually exhaust a
/// lifetime budget and stop being restarted, which is the opposite of what a
/// supervisor is for. What we are actually trying to detect is a *crash loop*,
/// and a crash loop is defined by dying **quickly**, not by dying often.
const _healthyRunThreshold = Duration(seconds: 30);

/// Survives [localAgentOriginProvider] being invalidated, which is exactly why
/// it is separate: the restart counter has to outlive the thing it counts.
///
/// It does **not** survive signing in again or pointing at a different server.
/// Both are the user actively changing the situation, and both are the actual
/// fix for the failures that exhaust this budget — a protocol mismatch is
/// resolved by aiming at a different daemon, and a stale token by signing in.
/// Keeping a spent budget across either would mean the fix appears not to work.
final _agentRestartBudgetProvider = Provider<_RestartBudget>((ref) {
  ref.watch(authControllerProvider.select((s) => s.token));
  ref.watch(appConfigProvider.select((c) => c.baseUrl));
  return _RestartBudget();
});

class _RestartBudget {
  int consecutive = 0;

  bool get exhausted => consecutive >= _maxConsecutiveRestarts;

  /// 1s, 2s, 4s, 8s, 16s. Backing off matters more than it looks: the common
  /// cause of an instant re-death is something transient holding a resource,
  /// and hammering it is how a transient failure becomes a permanent one.
  Duration get delay => Duration(seconds: 1 << consecutive.clamp(0, 4));
}

/// 指向此刻活着的那个本地 agent —— **只为了「装更新之前先把它停掉」**。
///
/// [localAgentOriginProvider] 把 agent 关在自己的闭包里，外面只有
/// `ref.onDispose` 能碰到它，而那条路是「发出去就不管了」的：
/// `unawaited(agent.stop())`。
///
/// 更新需要的是**能 await 的**停止。安装程序下一秒就要替换
/// `cortex-local.exe`，而 Restart Manager 如果还看得见它在跑，
/// `/RESTARTAPPLICATIONS` 可能把它当成一个独立程序重新拉起来 ——
/// 那时它没有 `--remote` / `--addr-file` / `--parent-pid`，
/// 起来就是个连不上远端、也没人管生死的孤儿。
///
/// 靠 `invalidate` 触发 onDispose 换不来这个保证：那只是排了一个停止动作，
/// 我们不知道它什么时候真的停了。
final localAgentHandleProvider = Provider<LocalAgentHandle>(
  (ref) => LocalAgentHandle(),
);

class LocalAgentHandle {
  LocalAgent? _current;

  // ignore: use_setters_to_change_properties
  void adopt(LocalAgent agent) => _current = agent;

  void forget(LocalAgent agent) {
    if (identical(_current, agent)) _current = null;
  }

  /// 停掉并**等它真的停了**。没有 agent 时是 no-op。
  Future<void> stop() async {
    final agent = _current;
    _current = null;
    if (agent != null) await agent.stop();
  }
}

/// 离线模式下桌面端与本地 agent 之间的一次性凭据。
///
/// 进程内生成一次、只活到退出。它保护的是「同机其他进程别来指挥这个
/// 能执行命令的 agent」——够不着的人猜不到，够得着的人（同一个用户）
/// 本来就能读这个进程的内存。
final String _sessionSecret = () {
  final rng = Random.secure();
  return List.generate(
    32,
    (_) => rng.nextInt(256).toRadixString(16).padLeft(2, '0'),
  ).join();
}();

final localAgentOriginProvider = FutureProvider<String?>((ref) async {
  final config = ref.watch(appConfigProvider);
  final token = ref.watch(authControllerProvider.select((s) => s.token));
  // **watch 而不是 read**：改了本机模型配置要让 agent 带着新环境重启。
  // read 的话用户会保存、看到成功、然后发现模型还是老的 —— 而那时
  // 界面上没有任何东西提示他需要重启
  final localLlm = ref.watch(localLlmProvider).value;
  // 关掉认证的部署（`CORTEX_AUTH=disabled`，自托管跑在 127.0.0.1 上的
  // 常见形态）根本没有 token 可拿。
  //
  // **原先这里只看 token 是否为 null**，于是那种部署会：跳过登录界面
  // （对的）、然后**静默地不启动本地 agent** —— 用户看到的是「装了桌面端，
  // 但它读不到我本机的文件」，而界面上没有任何一处说明为什么。
  // 两件事本来就没有因果关系：本地 agent 要的是「能连上 cortexd」，
  // 不是「有一把 token」。
  final needsToken = ref.watch(
    authControllerProvider.select((s) => s.health?.requiresToken ?? true),
  );
  if (config.useMock || !kLocalAgentSupported) return null;
  // 离线模式：**没有 cortexd**，本地 agent 就是全部 —— 必须起，
  // 而且没有 token 可给（也没人会来校验它）。不放行的话这个模式
  // 什么也不是：没有模型、没有工具、只有一个空界面
  if (!config.offline && needsToken && token == null) return null;

  final agent = discoverLocalAgent();
  if (agent == null) return null;

  final handle = ref.read(localAgentHandleProvider)..adopt(agent);

  var disposed = false;
  Timer? restartTimer;
  ref.onDispose(() {
    disposed = true;
    restartTimer?.cancel();
    handle.forget(agent);
    unawaited(agent.stop());
  });

  final budget = ref.read(_agentRestartBudgetProvider);
  if (budget.exhausted) {
    debugPrint(
      '本地 agent 连续 $_maxConsecutiveRestarts 次启动即退出，不再重试；'
      '已回落到直连远端。重新登录可重置',
    );
    return null;
  }

  final startedAt = DateTime.now();

  /// One policy for both ways this can fail.
  ///
  /// "It died after running" and "it never came up" look different but call for
  /// the same response: back off, try again, and stop after enough tries. Two
  /// separate policies would drift — and the one that got forgotten would be
  /// the silent one.
  void scheduleRestart(String why) {
    if (disposed) return;

    // A run that lasted counts as proof the thing works; whatever killed it was
    // not a startup failure. Reset before incrementing so a long-lived agent
    // that dies once starts over at a 1-second backoff.
    if (DateTime.now().difference(startedAt) >= _healthyRunThreshold) {
      budget.consecutive = 0;
    }
    budget.consecutive++;
    debugPrint('本地 agent（第 ${budget.consecutive} 次）：$why');

    if (budget.exhausted) {
      debugPrint('不再重试本地 agent —— 上面那段是它最后说的话。已回落到直连远端');
      // Still invalidate: the provider must re-run to hand the rest of the app
      // a null origin, otherwise every request keeps going to a port nobody is
      // listening on.
      ref.invalidateSelf();
      return;
    }
    restartTimer = Timer(budget.delay, () {
      if (!disposed) ref.invalidateSelf();
    });
  }

  try {
    final origin = await agent.start(
      remote: config.baseUrl,
      // 关掉认证的部署没有 token。空串而不是抛错：本地 agent 那侧
      // 也只是把它塞进 Authorization 头，而一个不认证的 cortexd 根本不看
      // 离线模式没有 cortexd 的 token，但**不能传空串**：本地 agent 能执行
      // 命令，而同机任意进程都够得着 127.0.0.1。现生成一把随机的、只活到
      // 本次进程结束的凭据 —— 桌面端与它自己拉起的 agent 之间对上即可，
      // 别人猜不到。
      //
      // （空串曾经是这里的写法，结果是 agent 拿到 `Some("")`、以为自己
      // 有认证、把桌面端 401 挡在外面，而「不做认证」那条警告一次都不打）
      token: token ?? _sessionSecret,
      // 离线模式必须本地直连模型：代理那条路要经 cortexd，而它不存在。
      // 传 null 表示「不干预」，让 agent 自己按环境变量决定
      llmRoute: config.offline ? 'direct' : null,
      // 只在离线模式下注入：连着服务器时模型是 cortexd 的事，
      // 把本机那份塞进去只会让两处配置打架
      extraEnv: config.offline ? localLlm?.toEnvironment() : null,
      // Unexpected death only — a deliberate `stop()` never lands here, so
      // signing out cannot be mistaken for a crash.
      //
      // The tail is the only record of *why*: `cortex-local` refusing to start
      // on a protocol mismatch prints one line, and without it the user sees
      // nothing but "tools stopped working".
      onExit: (code, logTail) => scheduleRestart(
        '退出，code=$code${logTail.isEmpty ? '' : '\n$logTail'}',
      ),
    );
    debugPrint('本地 agent 已就绪：$origin（工具在本机执行）');
    return origin;
  } on LocalAgentException catch (e) {
    // Loud in the log, silent in the UI: the app still works, it just runs
    // tools on the server. Blocking here would be worse than degrading.
    //
    // Retried on the same budget rather than given up on: the usual cause of a
    // one-off start failure is something transient holding the executable (a
    // virus scanner sweeping a freshly-installed .exe is the common one), and
    // that clears on its own within seconds. A permanent cause — a protocol
    // mismatch, a missing binary — burns the budget instead and then stops.
    scheduleRestart('$e');
    return null;
  }
});

/// The single place that knows whether we are on mock or live data.
///
/// Rebuilt whenever [appConfigProvider] changes, which cascades an invalidation
/// through every dependent provider — sessions, memory search, health — so the
/// UI fully re-hydrates against the new backend without manual refresh calls.
///
/// ## Why the credential is watched here too
///
/// The token is baked into the client at construction rather than read per
/// request, so it cannot drift mid-flight. Signing in or out is
/// therefore the same kind of event as switching backends — a different
/// identity is a different data source — and it re-hydrates by exactly the same
/// mechanism instead of needing its own refresh path.
///
/// Only the *token* is watched, not the whole [AuthState]: `select` keeps a
/// countdown tick or a busy flag from tearing down every in-flight request.
final cortexApiProvider = Provider<CortexApi>((ref) {
  final config = ref.watch(appConfigProvider);
  if (config.useMock) {
    final api = MockCortexApi();
    ref.onDispose(api.dispose);
    return api;
  }

  final token = ref.watch(authControllerProvider.select((s) => s.token));

  // 本地 agent 起好之后指向它，否则指向远端 —— 两侧说同一套协议
  // （agent 把自己不处理的原样反代），所以下游分辨不出区别，
  // 换过去只是又一次后端变更。
  //
  // `config.baseUrl` 在**显示**的地方始终是远端：那是用户配的、
  // 也是他关心的；给他看 `127.0.0.1:51234` 是真话但没有用。
  //
  // # 为什么 loading 期间也用远端，而不是等
  //
  // 这个 provider 是同步的，等不了。而「等」也不对：这台机器可能根本
  // 没有 agent（Web、或者 flutter run 的构建），那时 loading 会一直挂着。
  //
  // 代价是冷启动那一两秒里发出的请求会打到远端，然后在 agent 就绪、
  // 这个 provider 重建时被 `dispose`（`_client.close()`）**当场掐断** ——
  // 于是「记忆检索」在每次冷启动都报一次
  // 「Connection closed before full header was received」。
  //
  // 治法在消费侧而不是这里：换后端时把在飞的请求**作废**并自动重来
  // （见 `MemoryController.build` 里那段）。在这里改成「等」会把一个
  // 只影响头两秒的问题，换成一个在无 agent 平台上永远转圈的问题。
  final agentOrigin = ref.watch(localAgentOriginProvider);
  final origin = agentOrigin.value ?? config.baseUrl;

  final api = HttpCortexApi(
    baseUrl: origin,
    token: token,
    // `read`, not `watch`: this is an outbound edge. Watching the notifier
    // would make every auth state change rebuild the client, including the one
    // this very callback triggers.
    onUnauthorized: () =>
        ref.read(authControllerProvider.notifier).onUnauthorized(),
  );
  ref.onDispose(api.dispose);
  return api;
});

/// `GET /health`, polled lazily (on demand + on manual refresh).
final healthProvider = FutureProvider<HealthStatus>((ref) async {
  final api = ref.watch(cortexApiProvider);
  return api.health();
});

/// System-following theme mode with a manual override.
class ThemeModeNotifier extends Notifier<ThemeMode> {
  @override
  ThemeMode build() => ThemeMode.system;

  void cycle() {
    state = switch (state) {
      ThemeMode.system => ThemeMode.light,
      ThemeMode.light => ThemeMode.dark,
      ThemeMode.dark => ThemeMode.system,
    };
  }
}

final themeModeProvider = NotifierProvider<ThemeModeNotifier, ThemeMode>(
  ThemeModeNotifier.new,
);
