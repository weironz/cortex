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
import 'dart:convert';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:http/http.dart' as http;

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../api/http_cortex_api.dart';
import '../api/mock_cortex_api.dart';
import '../auth/local_llm_store.dart';
import '../models/assistant.dart';
import '../models/attachment.dart';
import '../models/chat_event.dart';
import '../models/image_prefs.dart';
import '../models/import_plan.dart';
import '../models/skill.dart';
import '../models/sync_event.dart';
import '../core/app_config.dart';
import '../core/local_llm.dart';
import '../core/motion.dart';
import '../core/ui_scale.dart';
import '../core/settings_store.dart';
import '../core/local_agent.dart';
import '../models/account.dart';
import '../models/health_status.dart';
import '../models/mcp.dart';
import '../models/sandbox_health.dart';
import '../models/workspace.dart';
import 'auth_controller.dart';
// 循环 import（chat_controller 也 import 这里）—— Dart 在库这一级允许，
// 而把 MainView 那套挪去 chat_controller 会让「我在哪个地方」跟对话状态
// 绑在一起，那正是它当初没塞进 LayoutState 的同一个理由
import 'chat_state.dart';
import 'chat_controller.dart';
import 'remote_attach_controller.dart';
import 'sandbox_backend_controller.dart';

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
        // 排在队里的那次写**可能等到容器已经销毁**（换后端、或者测试收尾）。
        // 那时 `ref.read` 直接抛「Cannot use the Ref after it has been
        // disposed」，而它抛在一条没人 await 的 future 上 —— 症状是控制台
        // 里一段没有上下文的异常，以及这一次设置没落盘。
        if (!ref.mounted) return;
        final current = await ref.read(settingsReaderProvider)();
        if (!ref.mounted) return;
        await ref.read(settingsWriterProvider)({...current, key: value});
      });
      return queue;
    };
  },
);

/// 上次拨到「开」了吗 —— **只用来决定这次启动传不传 flag**。
///
/// 权威是运行时那个开关（`remoteAttachProvider`），见它的库文档。
/// 读不到 / 读不懂一律当**关**：这个方向的默认值不对称，
/// 「该开的没开」用户点一下就好了，「该关的开着」他不会发现。
Future<bool> _savedRemoteAttach(Ref ref) async {
  try {
    final saved = await ref.read(settingsReaderProvider)();
    return saved[kRemoteAttachSetting] == 'true';
  } on Object catch (_) {
    return false;
  }
}

class AppConfigNotifier extends Notifier<AppConfig> {
  /// 上一次用的地址在**磁盘**上，读它要异步；而 `build` 是同步的。
  ///
  /// 所以先给编译期默认值，再排一个微任务把存下来的读回来。
  /// 中间那一瞬间用默认地址不会造成可见的错误：认证控制器的探测
  /// 也排在微任务里，且 baseUrl 变化会触发它重来。
  ///
  /// **在此之前这个类完全活在内存里** —— 重启之后地址回到编译期默认值，
  /// 而一个把 cortexd 部署在别处的人每次打开都要重填一遍。
  /// 「地址已经落定」。见 [restored]。
  ///
  /// `null` = 压根没有待办的恢复。**测试里的替身会走到这一支**：
  /// 它们覆写 `build()` 且不调 `super`，于是这个字段一直是 null ——
  /// 那时 `restored` 必须立刻完成，否则等它的人会永远挂着。
  Completer<void>? _pending;

  /// 等「上次那个地址」从磁盘读回来。
  ///
  /// # 谁需要它
  ///
  /// [AuthController._bootstrap]。它启动时要拿存下来的 refresh token 去续
  /// 会话，而**读凭据库比读设置慢**（跨进程问钥匙串）。地址一落地就会触发
  /// `_reset()`，于是那次续期回来时发现代次变了，把自己判成「已被取代」
  /// 直接返回 —— 续期请求一次都没发出去。
  ///
  /// 症状：**存过自定义地址的用户（也就是所有自建部署的用户）每次启动都
  /// 回到登录页**，而界面上写着「登录状态会记住 30 天」。服务端那侧的
  /// 证据是 refresh token 的 `rotated_at` 恒为空 —— 一次轮换都没发生过。
  Future<void> get restored => _pending?.future ?? Future<void>.value();

  @override
  AppConfig build() {
    _pending = Completer<void>();
    Future.microtask(_restore);
    return AppConfig.initial;
  }

  Future<void> _restore() async {
    try {
      final saved = await ref.read(settingsReaderProvider)();
      if (!ref.mounted) return;
      final known = _decodeKnown(saved[_kKnownBaseUrls]);
      final url = saved[_kBaseUrl]?.trim();
      if (url == null || url.isEmpty) {
        if (known.isNotEmpty) state = state.copyWith(knownBaseUrls: known);
        return;
      }
      state = state.copyWith(
        baseUrl: url,
        // 存过地址就说明这个地址是**选出来的**，哪怕它还没进过清单
        // （这个功能之前的老配置就是这样）—— 不补上的话，升级之后
        // 第一次打开，清单里连自己正在用的那个都没有
        knownBaseUrls: AppConfig.remember(known, url),
      );
    } finally {
      // **放 finally 里**：读设置失败（文件损坏、权限）时同样要放行，
      // 否则启动路径上等它的那一步会永远挂着，而界面是一片空白
      if (_pending?.isCompleted == false) _pending!.complete();
    }
  }

  static const String _kBaseUrl = 'base_url';
  static const String _kKnownBaseUrls = 'known_base_urls';

  /// 设置表的值只能是字符串，所以清单编码成 JSON 数组。
  ///
  /// 读不懂就当**空清单**，不抛 —— 这份数据的全部作用是「少打一次字」，
  /// 为它把启动路径弄挂是完全不成比例的。
  static List<String> _decodeKnown(String? raw) {
    if (raw == null || raw.trim().isEmpty) return const [];
    try {
      final decoded = jsonDecode(raw);
      if (decoded is! List) return const [];
      return [
        for (final e in decoded)
          if (e is String && e.trim().isNotEmpty) e.trim(),
      ];
    } on FormatException {
      return const [];
    }
  }

  /// 落盘。失败只是「下次要重填」，不打断任何事（见 `writeSettings`）。
  void _persist() {
    final patch = ref.read(settingsPatcherProvider);
    unawaited(patch(_kBaseUrl, state.baseUrl));
    unawaited(patch(_kKnownBaseUrls, jsonEncode(state.knownBaseUrls)));
  }

  void setUseMock(bool value) {
    if (state.useMock == value) return;
    state = state.copyWith(useMock: value);
  }

  /// 换后端地址。**这个动作同时解除离线模式。**
  ///
  /// # 为什么在这里解除
  ///
  /// 离线**不是用户选的**：开机探测失败时 `AuthController._unreachable`
  /// 自动兜上它。而它一旦为真，`probe()` 就在离线那条短路上直接返回，
  /// **再也不碰网络**。
  ///
  /// 于是 2026-08-30 之前，「去设置里把地址改回官方服务器」是一个空转的
  /// 动作：地址真的换了、listener 也真的重探了，探测却停在短路上 ——
  /// 界面纹丝不动，而用户手上没有任何线索说明为什么。
  ///
  /// 解除离线原本全仓库只有一个入口（聊天页横幅上的「去连接」），而那
  /// 恰恰不是一个「我连错环境了」的人会去点的东西 —— 他会去改地址。
  /// **一个状态只有一条退出路径时，要确认那条路径是用户真的会走的那条。**
  ///
  /// # 解除是安全的
  ///
  /// 这一次重探不带 `fallBackToOffline`，所以新地址要是也连不上，落进的是
  /// [AuthPhase.unreachable] —— 失败原因摆在他正在改的那个输入框上面，
  /// 而不是再一次静默变回离线。
  void setBaseUrl(String value) {
    final trimmed = value.trim();
    if (trimmed.isEmpty) return;
    // 地址一样、而且本来就在线 —— 这一次是真的没事可做。
    // ⚠️ 不能只判地址：**人在离线时，重新给一次同样的地址也必须放行**。
    // 开机那一刻官方服务器恰好抽风的人，地址本来就是对的，他能做的
    // 「重新连一下」正是原样再给一次 —— 按地址短路等于把他锁死在离线里。
    final sameUrl = state.baseUrl == trimmed;
    if (sameUrl && !state.offline) return;
    state = state.copyWith(
      baseUrl: trimmed,
      offline: false,
      knownBaseUrls: AppConfig.remember(state.knownBaseUrls, trimmed),
    );
    // 地址没变就没有新东西要落盘
    if (!sameUrl) _persist();
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

/// 右侧那一列此刻显示谁。
///
/// # 为什么是一个枚举而不是两个 bool
///
/// 右侧只有一列，所以「记忆」与「文件」天然互斥。用两个 bool 的话，
/// 「两个都 true」是一个**类型上合法但界面上没有定义**的状态 —— 而它迟早
/// 会出现（某次只改了一处的开关），表现是右侧画了谁全看代码里哪个 if 在前。
///
/// 一个可空枚举把互斥变成**类型上不可能违反**，与这个仓库在别处的立场一致：
/// 能靠结构保证的，就不要靠每个读点各写一遍判断。
/// # 现在只剩一格，为什么还留着枚举
///
/// 记忆那一格随记忆界面一起去了 Cormex。剩一个变体时它读起来像个多余的
/// `bool`，但**下一格已经排上了**（MCP 服务器面板，见 roadmap 的 H 节）——
/// 现在退回 bool，那天要把三处调用点再改回来。
enum RightPanel {
  /// 文件。云端是那个项目的工作区，桌面端是会话绑定的目录。
  files,

  /// 终端。
  ///
  /// # 为什么它从右栏的页签里搬了出来
  ///
  /// 它从前是 `RailTab.terminal`，与「文件 / 本轮改动」共用一个入口图标。
  /// 那套心智（「打开文件栏，然后在里面翻」）对**看**的东西成立，对终端
  /// 不成立：终端是**开着干活**的地方，它要自己的标签页、自己的开关、
  /// 自己的快捷键（Ctrl+`），还要能铺满整块内容区。塞在别人的页签条里，
  /// 这四样一样都摆不下。
  ///
  /// 但它仍然住在**同一列**（右侧只有一列，见 `AppShell` 的文档）——
  /// 它与文件栏轮流占那一列，而不是再开第四列把对话挤掉。
  terminal,
}

/// 右栏里的页签。
///
/// # 为什么页签不进 [RightPanel]
///
/// [RightPanel] 管的是「右栏开没开、开着谁」—— 顶栏一个图标一格。
/// 这两页共用同一个入口图标，进了枚举就得有两个图标，而用户的心智是
/// 「打开文件栏，然后在里面翻」：它们回答的是同一类问题（这个会话
/// 动了哪些文件）。
///
/// ⚠️ 终端 2026-08-26 从这里搬去了 [RightPanel.terminal]，理由见那里 ——
/// 它不是「看」的东西，是「干活」的地方。
enum RailTab {
  files('文件'),
  changes('本轮改动');

  const RailTab(this.label);

  final String label;
}

/// 右栏当前停在哪个页签。
///
/// 独立 provider 而不是 `RightRail` 的局部 state：顶栏「本会话改动」
/// 与气泡里 diff 卡的「在右栏打开」都要**从外面**把它切到某一页 ——
/// 局部 state 够不着。刻意不持久化：页签是「这次想看什么」，
/// 不是布局偏好。
class RailTabNotifier extends Notifier<RailTab> {
  @override
  RailTab build() => RailTab.files;

  void go(RailTab tab) => state = tab;
}

final railTabProvider = NotifierProvider<RailTabNotifier, RailTab>(
  RailTabNotifier.new,
);

/// 两侧面板的显示状态。
class LayoutState {
  const LayoutState({
    this.leftCollapsed = false,
    this.rightPanel,
    this.leftWidth = kLeftPaneDefault,
    this.rightWidth = kRightPaneDefault,
  });

  /// 会话那一栏。收起时账号栏也跟着不见 —— 与 Codex / Claude
  /// 桌面端一致：收起就是整条侧栏都没了，要用就展开。
  final bool leftCollapsed;

  /// 右侧显示谁，`null` = 右侧整个收起。
  final RightPanel? rightPanel;

  /// 左栏宽度（逻辑像素）。见 [kLeftPaneMin]。
  final double leftWidth;

  /// 右栏宽度（逻辑像素）。上限由窗口宽度决定，见 `AppShell`。
  final double rightWidth;

  /// `rightPanel` 要能被显式置 `null`，所以它**不能**用
  /// `rightPanel ?? this.rightPanel` 那套 —— 那样「收起」永远写不进去。
  /// 于是这个 copyWith 只管左栏，右侧由调用方直接造新的。
  LayoutState collapseLeft(bool collapsed) => LayoutState(
    leftCollapsed: collapsed,
    rightPanel: rightPanel,
    leftWidth: leftWidth,
    rightWidth: rightWidth,
  );

  /// 只换右侧那一格，别的原样带过去。
  ///
  /// ⚠️ 三个改右侧的方法从前各自 `LayoutState(leftCollapsed: …, rightPanel: …)`
  /// 手写一遍。加宽度字段的那一刻，那种写法会把两个宽度**静默重置成默认值**
  /// —— 拖过宽度的人一点右栏图标，两侧齐刷刷弹回原宽，而没有任何报错。
  LayoutState withRight(RightPanel? panel) => LayoutState(
    leftCollapsed: leftCollapsed,
    rightPanel: panel,
    leftWidth: leftWidth,
    rightWidth: rightWidth,
  );
}

/// 左栏宽度的三个数。
///
/// # 为什么左右两条的可调幅度差这么多
///
/// 左栏装的是**导航**：一列会话标题。再宽也只是让标题少截几个字，而中间
/// 那栏是真正在读的东西 —— 把导航拉到半屏是纯损失。所以给一个够用的窄区间。
///
/// 右栏装的是**内容**：文件树、diff、终端输出。它们与对话是对照着读的，
/// 「这次我要仔细看 diff」是个正当诉求，那时对话退成一条缝完全合理。
/// 所以它的上限由窗口宽度定（见 `AppShell`），不在这儿封。
const double kLeftPaneMin = 200;
const double kLeftPaneMax = 380;
const double kLeftPaneDefault = 240;

const double kRightPaneMin = 280;
const double kRightPaneDefault = 348;

/// 拖到最宽时留给对话的那一条。
///
/// 不留 0：一个 0 宽的中间栏会让它内部一串固定宽度的东西（顶栏按钮、
/// 输入框内边距）报溢出，而那种红黄条纹一旦出现，用户以为整个应用崩了。
/// 留一条还看得见「对话在这儿」，也还抓得住那根分隔条把它拉回来。
const double kChatPaneMin = 200;

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
  static const String _kRight = 'right_panel';
  static const String _kLeftWidth = 'left_pane_width';
  static const String _kRightWidth = 'right_pane_width';

  /// 上一版的键，只读不写。
  ///
  /// 老键 `memory_pane_visible` **刻意不再读**。它表达的是「记忆栏开没开」，
  /// 而记忆界面已经去了 Cormex —— 那个问题没有答案了。读它的唯一后果是把
  /// 一个存过 `true` 的老用户顶到文件面板上，而他从来没要过文件面板。

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
      rightPanel: _readRight(saved),
      // 存坏了（手改过设置、或者某一版写了别的东西）就用默认值，
      // 而不是让一个 NaN 或者 3 像素的宽度传到布局里去
      leftWidth: _readWidth(
        saved[_kLeftWidth],
        kLeftPaneDefault,
        kLeftPaneMin,
        kLeftPaneMax,
      ),
      // 上限在这儿放宽到一个不可能的大数：右栏的真上限跟着**窗口宽度**走，
      // 而恢复的这一刻还不知道窗口多宽。真正的封顶在 `AppShell` 里，
      // 那儿 `clamp` 一次就够 —— 存的时候封会让人换到小屏再换回来时
      // 宽度被永久截掉
      rightWidth: _readWidth(
        saved[_kRightWidth],
        kRightPaneDefault,
        kRightPaneMin,
        double.infinity,
      ),
    );
  }

  /// 新键优先；没有新键就按老键推。
  static RightPanel? _readRight(Map<String, String> saved) {
    switch (saved[_kRight]) {
      case 'files':
        return RightPanel.files;
      // ★ 存过 `memory` 的老用户落到这里 —— 那一格已经不存在了。
      //   **收起**，而不是崩，也不是硬塞给他文件面板：他上次选的是记忆，
      //   而记忆现在在 Cormex 的 Web 端。给他一个空的右栏，
      //   要看文件他自己点那个图标。
      //
      //   老的 `_kMemoryLegacy` 同理不再参与判断：它表达的是「记忆栏开没开」，
      //   而那个问题已经没有意义了。
      default:
        return null;
    }
  }

  static double _readWidth(String? raw, double fallback, double lo, double hi) {
    final v = double.tryParse(raw ?? '');
    if (v == null || !v.isFinite) return fallback;
    return v.clamp(lo, hi);
  }

  /// 指针横向挪了 `dx` —— **在当下的宽度上累加**。
  ///
  /// # ⚠️ 为什么不能让调用方算好宽度传进来
  ///
  /// `onHorizontalDragUpdate` 一帧可能来好几次（高回报率鼠标、Debug 构建
  /// 掉帧时更明显），而界面**一帧只重建一次**。调用方写成
  /// `setLeftWidth(当前宽度 + dx)` 的话，那个「当前宽度」是 build 时捕获
  /// 进闭包的旧值 —— 同一帧里的几次更新全基于同一个起点算，只有最后一次
  /// 生效，其余的位移**凭空丢掉**。
  ///
  /// 症状不是报错，是**手感**：鼠标挪了 10 像素，栏只跟了 3 像素，
  /// 而且越卡越慢。所以累加这一步必须发生在这里 —— `state` 在这儿永远
  /// 是最新的。
  void nudgeLeftWidth(double dx) => setLeftWidth(state.leftWidth + dx);

  /// 同上。`max` 由调用方给：右栏的上限跟着**窗口宽度**走，
  /// 而这一层不知道窗口多宽。
  void nudgeRightWidth(double dx, double max) {
    // 先把存着的值收进当下这个窗口允许的区间，再累加：窗口被拖窄过之后
    // state 里可能留着一个比 max 大的值，直接减 dx 会让第一下跳一大截
    final now = state.rightWidth.clamp(kRightPaneMin, max);
    setRightWidth((now - dx).clamp(kRightPaneMin, max));
  }

  /// 拖完了才存 —— 拖动过程中每一帧都写一次的话，一次拖拽就是几十次
  /// 磁盘写入，而中间那些值没有一个是用户想要的
  void setLeftWidth(double width) {
    final w = width.clamp(kLeftPaneMin, kLeftPaneMax);
    if (w == state.leftWidth) return;
    state = LayoutState(
      leftCollapsed: state.leftCollapsed,
      rightPanel: state.rightPanel,
      leftWidth: w,
      rightWidth: state.rightWidth,
    );
  }

  void setRightWidth(double width) {
    if (width == state.rightWidth) return;
    state = LayoutState(
      leftCollapsed: state.leftCollapsed,
      rightPanel: state.rightPanel,
      leftWidth: state.leftWidth,
      rightWidth: width,
    );
  }

  /// 松手。**存的是最终值**，见 [setLeftWidth]。
  void persistWidths() {
    final patch = ref.read(settingsPatcherProvider);
    unawaited(patch(_kLeftWidth, '${state.leftWidth}'));
    unawaited(patch(_kRightWidth, '${state.rightWidth}'));
  }

  void toggleLeft() {
    state = state.collapseLeft(!state.leftCollapsed);
    unawaited(
      ref.read(settingsPatcherProvider)(_kLeft, '${state.leftCollapsed}'),
    );
  }

  /// 点顶栏某个面板的图标。
  ///
  /// **点已经开着的那个 = 收起**，与两家参考产品一致：同一个按钮既是
  /// 「给我看这个」也是「不看了」，用户不用去找第二个关闭入口
  /// （面板自己那个 × 仍然在，两条路都通）。
  void selectRight(RightPanel panel) {
    final next = state.rightPanel == panel ? null : panel;
    state = state.withRight(next);
    unawaited(ref.read(settingsPatcherProvider)(_kRight, next?.name ?? 'none'));
  }

  /// 只选中，**不切换**。窄屏那条路用它。
  ///
  /// 那儿右栏是个抽屉，开合由 `Scaffold` 管，不由 `rightPanel` 管。用
  /// [selectRight] 的话，连点两次「文件」的第二次会把 `rightPanel` 置空 ——
  /// 而抽屉照样打开，里面画的是记忆。用户点的是文件。
  void showRight(RightPanel panel) {
    if (state.rightPanel == panel) return;
    state = state.withRight(panel);
    unawaited(ref.read(settingsPatcherProvider)(_kRight, panel.name));
  }

  /// 面板自己那个关闭按钮。
  void closeRight() {
    state = state.withRight(null);
    unawaited(ref.read(settingsPatcherProvider)(_kRight, 'none'));
  }
}

final layoutProvider = NotifierProvider<LayoutNotifier, LayoutState>(
  LayoutNotifier.new,
);

/// 中间那一大栏现在是哪个「地方」。
///
/// # 为什么不塞进 [LayoutState]
///
/// 那个类回答的是**两侧怎么排**（收起没有、右边画谁），而这个回答的是
/// **我在哪儿**。合并的代价很具体：`LayoutState` 有四处构造点，每一处
/// 都得顺手把当前视图带上，而「收起左栏」与「我在画廊里」之间没有任何关系 ——
/// 漏带一处的表现是收个侧栏就被弹回聊天。
///
/// # 为什么是枚举而不是 `bool isImages`
///
/// 与 [RightPanel] 同一条理由：下一个「地方」进来时（roadmap 里排着的
/// 资料库），bool 那条路要把每个调用点改一遍。
enum MainView {
  chat,
  images,
  projects,
  assistants,
  library;

  static MainView fromWire(String? s) =>
      MainView.values.where((v) => v.name == s).firstOrNull ?? MainView.chat;
}

/// 在哪个地方，跨重启记住。
///
/// 持久化的理由与侧栏折叠同源：切到画廊的人多半要在那儿待一会儿，
/// 每次开窗都弹回聊天，等于这个入口只在当前这个窗口有效。
class MainViewNotifier extends Notifier<MainView> {
  static const String _key = 'main_view';
  static const String _kImageSession = 'image_session';

  /// 离开聊天页时它正开着哪条会话 —— 回来时接着看那条。
  ///
  /// 只活在内存里：跨重启由 `activeSessionId` 自己那套恢复负责，
  /// 这里再存一份就是同一件事两个来源。
  String? _chatSession;

  /// 图片页那条会话。跨重启记住 —— 不然每次开窗都多一条空会话。
  String? _imageSession;

  @override
  MainView build() {
    // ⚠️ **「在图片页时，活动会话必须是图片那条」是一个不变式，
    // 不是一次性的动作。**
    //
    // `ChatController` 在 `cortexApiProvider` 变化时会 `_reload()` **重置
    // 整个状态**（认证续上、地址从磁盘恢复，启动时都会触发）。冷启动直接
    // 恢复到图片页时，`_restore` 建好的那条会话正好被随后那次 reload 冲掉，
    // 于是图片页上显示的是聊天那条对话 —— 实测撞到过。
    //
    // 所以每次会话列表加载完都重新落一次这个不变式，而不是只在进入时做。
    ref.listen(chatControllerProvider.select((s) => s.sessionsLoading), (
      was,
      now,
    ) {
      if (was == true && now == false && state == MainView.images) {
        _enterImages();
      }
    });
    Future.microtask(_restore);
    return MainView.chat;
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    _imageSession = saved[_kImageSession];
    final v = MainView.fromWire(saved[_key]);
    if (v != state) {
      state = v;
      // 恢复到图片页时也要把那条会话选回来 —— 否则显示的是上次聊天
      // 那一条，而它跟画图毫无关系。**上面那个监听是它的保险**：
      // 这一下可能被随后的 reload 冲掉
      if (v == MainView.images) _enterImages();
    }
  }

  /// 去某个地方。
  ///
  /// # 为什么切「地方」要跟着切会话
  ///
  /// `ConversationView` 画的永远是**当前活动会话**（十几处
  /// `activeSessionId` 选择器）。把它参数化要动整个对话内核，而收益只是
  /// 让两个地方各记一条会话 —— 那件事换个方式也能做到：**进哪个地方，
  /// 就把那个地方的会话设成活动的**。
  ///
  /// 这不是将就。「每个地方有自己当下那条对话」本来就是它该有的语义，
  /// 而 `selectSession` 明写着切换**不会打断在飞的那一轮**
  ///（"keep it running and just look away"）—— 所以来回走不丢东西。
  void go(MainView view) {
    if (state == view) return;
    final chat = ref.read(chatControllerProvider);
    // ⚠️ **离开聊天时就记下它那条会话，不论去哪儿。**
    //
    // 从前是「去图片时记一下」。加了项目页之后那个写法会错：
    // 聊天 → 项目 → 聊天 这条路上没人记过，于是回来时恢复的是**上一次
    // 去图片页之前**那条会话 —— 用户看到的是「回来之后换了个对话」。
    if (state == MainView.chat) _chatSession = chat.activeSessionId;
    switch (view) {
      case MainView.images:
        _enterImages();
      case MainView.chat:
        final back = _chatSession;
        // ⚠️ **刚点了「新对话」时不要恢复。**
        //
        // 「回聊天页就恢复上次那条会话」与「点新对话开一张白纸」是直接冲突的，
        // 而它们会在同一次点击里先后发生：`NavBlock` 的「新对话」按钮先
        // `startNewChat()`，再 `go(MainView.chat)` —— 于是白纸刚立起来就被
        // 这里顶掉，用户被扔回上一条会话。
        //
        // 从图片页或项目页点「新对话」就是这条路（在聊天页上点时 `_chatSession`
        // 恰好等于当前那条，看不出来）。2026-08-23 惰性化时实测撞到，
        // 症状是「点新对话没反应」，而三条 provider 各自都工作正常。
        //
        // `pendingNewChat` 就是那张白纸的存在证明 —— 它非空说明用户此刻
        // 要的是新的，不是回到旧的
        if (chat.pendingNewChat == null &&
            back != null &&
            chat.sessions.any((s) => s.id == back)) {
          // 那条会话可能已经被删了 —— 删了就停在原来那条上，
          // 而不是把用户扔进一个空白页
          ref.read(chatControllerProvider.notifier).selectSession(back);
        }
      // 项目页、智能体页、资料库**都不碰活动会话**：它们是卡片墙，
      // 点某张卡才会去某条会话。顺手切一条的话，从这几页返回聊天会
      // 发现对话被换掉了
      case MainView.projects || MainView.assistants || MainView.library:
        break;
    }
    state = view;
    unawaited(ref.read(settingsPatcherProvider)(_key, view.name));
  }

  /// 让图片页那条会话成为活动会话，没有就建一条。
  ///
  /// **懒建而不是每次都建**：每点一次「图片」多一条空会话的话，
  /// 侧栏很快就全是「新会话」。记住 id 之后重复进出复用同一条。
  void _enterImages() {
    final ctrl = ref.read(chatControllerProvider.notifier);
    final chat = ref.read(chatControllerProvider);
    final known = _imageSession;
    if (known != null) {
      // ⚠️ **会话列表还在路上时，「不在列表里」不等于「已经删了」。**
      //
      // 冷启动恢复到图片页时 `sessions` 还是空的，拿它当判据的话每次
      // 开窗都会多建一条空会话，而记住的那条永远用不上。
      final gone =
          !chat.sessionsLoading && !chat.sessions.any((s) => s.id == known);
      if (!gone) {
        // 已经是它了就什么都不做 —— `selectSession` 自己也早退，
        // 但这里显式写出来是为了下面「否则新建」那一支读起来没有歧义
        if (chat.activeSessionId != known) ctrl.selectSession(known);
        return;
      }
    }
    _imageSession = ctrl.createSession();
    unawaited(
      ref.read(settingsPatcherProvider)(_kImageSession, _imageSession!),
    );
  }

  /// 图片页上点「新对话」。
  ///
  /// # 已经在一张白纸上就什么都不做
  ///
  /// 2026-09-02 用户实报：「输入框没有输入内容，点击新对话，会一直创建空
  /// 对话」。这一支此前**无条件**建 —— 点五次多五条「新会话」。
  ///
  /// 上面 `_enterImages` 的注释早就写着同一件事（「懒建而不是每次都建：
  /// 每点一次多一条空会话的话，侧栏很快就全是新会话」），只是那条纪律没有
  /// 跟到按钮这一支上。这是「同一条判据在两处，只有一处照做」那个形状。
  ///
  /// 判据是**会话本身还是空的**，不是「有没有点过」：一条已经问过话的会话
  /// 当然要给新的。手上打了一半还没发的字不算 —— 那些不在 transcript 里，
  /// 而它们正是用户此刻要用的东西，把它们连同会话一起换掉才是真的丢东西。
  void newImageSession() {
    final ctrl = ref.read(chatControllerProvider.notifier);
    final chat = ref.read(chatControllerProvider);
    // ⚠️ 判据看的是**当前活动会话**，不是记着的那个 `_imageSession`。
    //
    // 第一版看后者，结果第一次点仍然会多建一条：`_imageSession` 只是这个
    // notifier 的一个字段，notifier 一重建它就没了（重建之后靠设置里那条
    // 恢复，而恢复是异步的、也可能根本没存过）。而「眼前这条是不是白纸」
    // 不依赖任何记忆 —— 它就是用户看着的那个东西。
    final active = chat.activeSessionId;
    if (active != null && _isBlank(chat, active)) {
      // 顺手认下它：下次进图片页直接复用，不必再建
      if (_imageSession != active) {
        _imageSession = active;
        unawaited(ref.read(settingsPatcherProvider)(_kImageSession, active));
      }
      return;
    }
    _imageSession = ctrl.createSession();
    unawaited(
      ref.read(settingsPatcherProvider)(_kImageSession, _imageSession!),
    );
  }

  /// 这条会话还是一张白纸吗。
  ///
  /// 三个条件缺一不可：
  ///
  /// * transcript 是空的 —— 一条问过话的会话不是白纸；
  /// * **没在跑** —— 刚发出去还没落进 transcript 的那一瞬间，消息列表也是
  ///   空的。只看消息数的话，那一瞬间点「新对话」会把正在生成的那条当成
  ///   白纸复用掉，而用户看到的是自己的问题被顶掉了；
  /// * transcript **已经取回来了** —— 没取回来时空列表的意思是「还不知道」，
  ///   拿它当「空的」会在冷启动恢复到图片页时复用一条其实有内容的会话。
  bool _isBlank(ChatState chat, String sessionId) {
    final t = chat.transcripts[sessionId];
    return t != null &&
        t.loadedFromServer &&
        t.messages.isEmpty &&
        !chat.isRunning(sessionId);
  }
}

final mainViewProvider = NotifierProvider<MainViewNotifier, MainView>(
  MainViewNotifier.new,
);

/// 这一轮如果画图，按什么规格。
///
/// # 为什么是 provider 而不是 `send()` 的一个参数
///
/// 与权限档、模型选择完全同一路数：`ChatController.send` 在**发出去那一刻**
/// 读它们（`chat_controller.dart` 里那三行）。做成参数的话，每一个调用
/// `send` 的地方都得记着传，而漏传的表现是「我设了 2 张，它只画了 1 张」。
///
/// 只有图片页会写它。聊天页那边保持默认（完全听模型的）——
/// 在一条聊天里顺口让它画张图，本来就不该被图片页上某个设置左右。
class ImagePrefsNotifier extends Notifier<ImagePrefs> {
  @override
  ImagePrefs build() => const ImagePrefs();

  void set(ImagePrefs prefs) => state = prefs;
}

final imagePrefsProvider = NotifierProvider<ImagePrefsNotifier, ImagePrefs>(
  ImagePrefsNotifier.new,
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

/// 准不准操作电脑（截屏 + 键鼠）。
///
/// # ⚠️ 默认关，而且这个默认值是安全属性的一部分
///
/// 别的工具都被围栏管着：文件工具受工作区限制、shell 在沙箱里、外来工具在
/// 别人的进程里。这一组**没有围栏** —— 它动的是用户整台机器上正在运行的
/// 一切：浏览器里登录着的账号、聊天窗口、密码管理器。
///
/// 与 `permissionModeProvider` 完全同构（逐轮带、存本地设置），只有默认值
/// 那一条刻意不同：权限档的默认是「每次询问」（一个安全的**中间**值），
/// 这里的默认是**没有这个能力**。
///
/// # 它只是「用户准了」
///
/// 够不够得着还要看跑在哪：容器里没有屏幕。`cortex-local` 那侧会再判一次，
/// 而界面这侧靠 `/health` 的 `computer_use` 决定摆不摆那个开关。
class ComputerUseNotifier extends Notifier<bool> {
  static const String _key = 'computer_use';

  @override
  bool build() {
    Future.microtask(_restore);
    return false;
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    // ⚠️ 只有**明确存过 `true`** 才打开。别用「不是 false 就当 true」那种
    // 写法：设置文件损坏、键名改过、旧版本没写过这一项，都会变成默认打开
    final on = saved[_key] == 'true';
    if (on != state) state = on;
  }

  void set({required bool on}) {
    if (state == on) return;
    state = on;
    unawaited(ref.read(settingsPatcherProvider)(_key, on ? 'true' : 'false'));
  }
}

final computerUseProvider = NotifierProvider<ComputerUseNotifier, bool>(
  ComputerUseNotifier.new,
);

/// Windows executables the local Computer Use driver may observe or control.
///
/// This is separate from the per-turn switch. The allowlist decides the hard
/// boundary; the switch decides whether a particular conversation may request
/// those tools at all. Entries are absolute executable paths because a display
/// name or basename is attacker-controlled.
class ComputerUseAllowlistNotifier extends Notifier<List<String>> {
  static const String _key = 'computer_use_allowlist';

  @override
  List<String> build() {
    Future.microtask(_restore);
    return defaultLocalComputerUseAllowlist();
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final raw = saved[_key];
    if (raw == null) {
      state = defaultLocalComputerUseAllowlist();
      return;
    }
    try {
      final decoded = jsonDecode(raw);
      if (decoded is! List) {
        state = const [];
        return;
      }
      state = _normalize(decoded.whereType<String>());
    } on FormatException {
      state = const [];
    }
  }

  Future<void> save(Iterable<String> entries) async {
    final next = _normalize(entries);
    state = next;
    await ref.read(settingsPatcherProvider)(_key, jsonEncode(next));
  }

  Future<void> add(String path) => save([...state, path]);

  Future<void> remove(String path) =>
      save(state.where((entry) => entry != path));

  Future<void> resetToDefaults() => save(defaultLocalComputerUseAllowlist());

  List<String> _normalize(Iterable<String> entries) {
    final seen = <String>{};
    return [
      for (final raw in entries)
        if (_looksLikeWindowsExecutable(raw) &&
            seen.add(raw.trim().toLowerCase()))
          raw.trim(),
    ];
  }

  bool _looksLikeWindowsExecutable(String raw) {
    final path = raw.trim();
    if (!path.toLowerCase().endsWith('.exe') ||
        path.contains('\u0000') ||
        path.contains('\n') ||
        path.contains('\r')) {
      return false;
    }
    return RegExp(r'^[A-Za-z]:[\\/]').hasMatch(path) || path.startsWith(r'\\');
  }
}

final computerUseAllowlistProvider =
    NotifierProvider<ComputerUseAllowlistNotifier, List<String>>(
      ComputerUseAllowlistNotifier.new,
    );

/// Packaged Windows apps need a package identity in addition to an exe path.
///
/// Stored wire format:
/// `aumid|<AUMID>|<absolute exe path>` or
/// `package-family|<PackageFamily>|<absolute exe path>`.
class ComputerPackagedAllowlistNotifier extends Notifier<List<String>> {
  static const String _key = 'computer_packaged_app_allowlist';

  @override
  List<String> build() {
    Future.microtask(_restore);
    return const [];
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final raw = saved[_key];
    if (raw == null) {
      state = const [];
      return;
    }
    try {
      final decoded = jsonDecode(raw);
      state = decoded is List
          ? _normalize(decoded.whereType<String>())
          : const [];
    } on FormatException {
      state = const [];
    }
  }

  Future<void> save(Iterable<String> entries) async {
    final next = _normalize(entries);
    state = next;
    await ref.read(settingsPatcherProvider)(_key, jsonEncode(next));
  }

  Future<void> add({
    required String kind,
    required String identity,
    required String executable,
  }) => save([
    ...state,
    '${kind == 'aumid' ? 'aumid' : 'package-family'}|'
        '${identity.trim()}|${executable.trim()}',
  ]);

  Future<void> remove(String entry) =>
      save(state.where((value) => value != entry));

  List<String> _normalize(Iterable<String> entries) {
    final seen = <String>{};
    return [
      for (final raw in entries)
        if (_normalizeOne(raw) case final entry?
            when seen.add(entry.toLowerCase()))
          entry,
    ];
  }

  String? _normalizeOne(String raw) {
    final parts = raw.split('|');
    if (parts.length < 3) return null;
    final kind = parts[0].trim().toLowerCase();
    final identity = parts[1].trim();
    final executable = parts.sublist(2).join('|').trim();
    if (kind != 'aumid' && kind != 'package-family') return null;
    if (identity.contains('|')) return null;
    if (executable.contains('|')) return null;
    if (identity.isEmpty ||
        identity.length > 512 ||
        identity.contains(RegExp(r'[\x00-\x1f\r\n]'))) {
      return null;
    }
    if (kind == 'aumid' && !identity.contains('!')) return null;
    if (kind == 'package-family' && identity.contains('!')) return null;
    if (!executable.toLowerCase().endsWith('.exe') ||
        executable.contains('\u0000') ||
        executable.contains('\n') ||
        executable.contains('\r') ||
        executable.contains('|') ||
        (!RegExp(r'^[A-Za-z]:[\\/]').hasMatch(executable) &&
            !executable.startsWith(r'\\'))) {
      return null;
    }
    return '$kind|$identity|$executable';
  }
}

final computerPackagedAllowlistProvider =
    NotifierProvider<ComputerPackagedAllowlistNotifier, List<String>>(
      ComputerPackagedAllowlistNotifier.new,
    );

/// Sites the isolated Chromium profile may reach.
///
/// The browser network proxy enforces this list on every connection. An empty
/// list disables the browser channel entirely; it never means “all sites”.
class ComputerBrowserAllowlistNotifier extends Notifier<List<String>> {
  static const String _key = 'computer_browser_allowlist';

  @override
  List<String> build() {
    Future.microtask(_restore);
    return const [];
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final raw = saved[_key];
    if (raw == null) {
      state = const [];
      return;
    }
    try {
      final decoded = jsonDecode(raw);
      state = decoded is List
          ? _normalize(decoded.whereType<String>())
          : const [];
    } on FormatException {
      state = const [];
    }
  }

  Future<void> save(Iterable<String> entries) async {
    final next = _normalize(entries);
    state = next;
    await ref.read(settingsPatcherProvider)(_key, jsonEncode(next));
  }

  Future<void> add(String site) => save([...state, site]);

  Future<void> remove(String site) =>
      save(state.where((entry) => entry != site));

  List<String> _normalize(Iterable<String> entries) {
    final seen = <String>{};
    return [
      for (final raw in entries)
        if (_validSite(raw) && seen.add(raw.trim().toLowerCase()))
          raw.trim().toLowerCase(),
    ];
  }

  bool _validSite(String raw) {
    var value = raw.trim().toLowerCase();
    if (value.isEmpty ||
        value.length > 253 ||
        value.contains(RegExp(r'[\s/?#@]'))) {
      return false;
    }
    value = value.startsWith('*.') ? value.substring(2) : value;
    final host = value.split(':').first;
    if (!host.contains('.') ||
        host == 'localhost' ||
        host.endsWith('.local') ||
        host.endsWith('.internal') ||
        RegExp(r'^[0-9.]+$').hasMatch(host) ||
        host.contains(':')) {
      return false;
    }
    final colon = value.lastIndexOf(':');
    if (colon != -1) {
      final port = int.tryParse(value.substring(colon + 1));
      if (port == null || port < 1 || port > 65535) return false;
    }
    return true;
  }
}

final computerBrowserAllowlistProvider =
    NotifierProvider<ComputerBrowserAllowlistNotifier, List<String>>(
      ComputerBrowserAllowlistNotifier.new,
    );

final permissionModeProvider =
    NotifierProvider<PermissionModeNotifier, PermissionMode>(
      PermissionModeNotifier.new,
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
  // 同账号 access token 轮换不是一次新身份，不能顺手清空崩溃预算；
  // 反过来，登出/重新登录确实换了失败上下文，应当重新计数。
  ref.watch(authControllerProvider.select((s) => s.phase == AuthPhase.ready));
  ref.watch(appConfigProvider.select((c) => c.baseUrl));
  return _RestartBudget();
});

class _RestartBudget {
  int consecutive = 0;

  bool get exhausted => consecutive >= _maxConsecutiveRestarts;

  /// 2s, 4s, 8s, 16s, 16s。Backing off matters more than it looks: the common
  /// cause of an instant re-death is something transient holding a resource,
  /// and hammering it is how a transient failure becomes a permanent one.
  ///
  /// ⚠️ 注释此前写的是「1s, 2s, 4s, 8s, 16s」，而**实现给不出 1s**：
  /// 调用方在读 `delay` 之前先 `consecutive++`，所以第一档取到的是
  /// `1 << 1 = 2s`。差一档不致命，但一份与实现对不上的注释会让排查的人
  /// 拿错误的节律去比对日志 —— 而「日志里的周期是 1.13 秒」正是
  /// 2026-08-21 那次判断根因的关键证据。
  Duration get delay => Duration(seconds: 1 << consecutive.clamp(0, 4));

  // ── 401 引发的重启另记一本账 ──────────────────────────────
  //
  // 上面那本记的是**崩溃**（进程死了才 +1）。401 重启走的是
  // `invalidate`，进程是被我们自己体面停掉的，永远不会进那本账 ——
  // 于是它此前**完全没有预算**。2026-08-21 的实测后果：凭据在服务端
  // 失效后，confirmations 的 1 秒轮询每次 401 都触发一次重启，agent
  // 被杀了 639+ 次，仍在继续。一个没有预算的自愈动作不是自愈，
  // 是把一次故障变成永动机。

  DateTime? _lastKick;
  int _kicks = 0;

  /// 这一次 401 重启还批不批。规则见 [allowRestartKick]。
  bool allowKick(DateTime now) {
    final r = allowRestartKick(now: now, lastKick: _lastKick, kicks: _kicks);
    _lastKick = now;
    _kicks = r.kicks;
    return r.allowed;
  }

  /// deployment/user 主体已经变化，旧主体的崩溃与 401 预算都失去意义。
  void resetForIdentityChange() {
    consecutive = 0;
    _lastKick = null;
    _kicks = 0;
  }
}

/// 401 重启的节流规则：**30 秒窗口内最多 2 次**。
///
/// # 为什么这是最后一道防线
///
/// 上游还有一道判据（「这个 401 是 agent 自己拒的吗」）。那道判据 2026-08-21
/// 之前是错的，后果是 agent 在 13.7 分钟里被杀了 730 次。判据可以再错，
/// 而这里保证**错的后果有上界**：坏也只坏成「重启两次没用」，不是永动机。
///
/// 一个没有预算的自愈动作不是自愈，是把一次故障变成永动机。
///
/// # 为什么是 2 次
///
/// 入站凭据错位是「agent 手上是旧的、客户端已经换新」这类**一次性**错位，
/// 重启一次就该好。给到 2 是容忍重启期间在飞的旧请求再触发一回。
/// 第 3 次还 401，说明根因不在错位 —— 再杀多少次进程也没用。
///
/// 拆成纯函数是为了**测得到**：这条规则只在故障时才走到，而故障现场
/// （持续 401）在测试里造出来代价很高。
@visibleForTesting
({bool allowed, int kicks}) allowRestartKick({
  required DateTime now,
  required DateTime? lastKick,
  required int kicks,
  Duration window = const Duration(seconds: 30),
  int limit = 2,
}) {
  // 窗口外就重新计数：真的隔了很久又错位一次，那是新的一次故障，
  // 不该被上一次的额度拖累
  final fresh = lastKick == null || now.difference(lastKick) > window;
  final next = (fresh ? 0 : kicks) + 1;
  return (allowed: next <= limit, kicks: next);
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
///
/// 测试与平台装配共用这一个发现口。生产仍只发现贴着应用发布的那一份；
/// 测试替换它，是为了实际覆盖 provider 的启动、换代与停止顺序，而不是只测
/// 一个与线上接线无关的纯函数。
@visibleForTesting
final localAgentDiscoveryProvider = Provider<LocalAgent? Function()>(
  (ref) => discoverLocalAgent,
);

/// `PUT /local/credential` 对本次 token 切换给出的身份结论。
enum LocalAgentCredentialPushResult {
  /// 服务端验明仍是同一个 `(deployment_id, user_id)`，新出站凭据已生效。
  accepted,

  /// 服务端返回 409：新凭据属于另一个账号，绝不能让旧进程继续服务它。
  identityChanged,

  /// 网络、鉴权或服务端错误。无法证明身份未变，按失败关闭处理。
  failed,
}

@visibleForTesting
LocalAgentCredentialPushResult agentCredentialPushResultForStatus(int status) {
  if (status >= 200 && status < 300) {
    return LocalAgentCredentialPushResult.accepted;
  }
  if (status == 409) {
    return LocalAgentCredentialPushResult.identityChanged;
  }
  return LocalAgentCredentialPushResult.failed;
}

typedef LocalAgentCredentialPush =
    Future<LocalAgentCredentialPushResult> Function({
      required String origin,
      required String inbound,
      required String? outbound,
    });

/// 热推凭据的测试缝。默认实现会读取 [httpClientFactoryProvider]；生产不
/// override，测试则无需真的在 loopback 上起 HTTP 服务。
@visibleForTesting
final localAgentCredentialPushProvider = Provider<LocalAgentCredentialPush>(
  (ref) =>
      ({
        required String origin,
        required String inbound,
        required String? outbound,
      }) => _pushAgentCredential(
        origin: origin,
        inbound: inbound,
        outbound: outbound,
        clientFactory: ref.read(httpClientFactoryProvider),
      ),
);

final localAgentHandleProvider = Provider<LocalAgentHandle>(
  (ref) => LocalAgentHandle(),
);

class LocalAgentHandle {
  LocalAgent? _current;
  bool _hasBoundCredential = false;
  String? _outboundCredential;
  Future<void> _stopTail = Future<void>.value();

  /// 这一轮 agent **启动时**钉住的那把本机凭据。
  ///
  /// # 为什么要钉，而不是每次读当前的用户 token
  ///
  /// agent 的入站认证比对的是它启动时拿到的那个值，而用户的 access token
  /// 每 15 分钟轮换一次。跟着轮换去发的后果是：客户端已经在用新的、
  /// agent 还只认旧的 —— 每次续期都稳定 401 一次，而那个 401 会被读成
  /// 「你的登录失效了」，把刚续过期的用户踢回登录页。
  ///
  /// 出站那把（agent 自己调部署入口用的）**照常跟着轮换**，走
  /// `PUT /local/credential` 热替换，不重启进程。
  String? pinnedCredential;

  // ignore: use_setters_to_change_properties
  void adopt(LocalAgent agent) => _current = agent;

  /// 服务端已确认这把出站凭据属于当前运行中的 agent。
  ///
  /// 只保存 token 值用于比较；它本来就已经在内存里，不新增持久化副本。
  void bindCredential({required String inbound, required String? outbound}) {
    pinnedCredential = inbound;
    _outboundCredential = outbound;
    _hasBoundCredential = true;
  }

  /// 同账号续期成功。此方法只更新围栏，不碰进程。
  void acceptOutboundCredential(String? outbound) {
    if (_current == null) return;
    _outboundCredential = outbound;
    _hasBoundCredential = true;
  }

  /// 当前桌面端 token 是否已被运行中的 agent 接住。
  ///
  /// token 一换、推送还在飞时它会立即变成 false；`cortexApiProvider` 据此
  /// 先用远端直连，绝不能在新凭据尚未验明时继续把请求送进旧账号的 agent。
  bool acceptsOutboundCredential(String? outbound) =>
      _current != null &&
      _hasBoundCredential &&
      _outboundCredential == outbound;

  /// 让客户端立刻停止把新 token 路由给旧进程。真正是否退出由 [stop] 决定。
  void invalidateCredential() {
    _hasBoundCredential = false;
    _outboundCredential = null;
  }

  void forget(LocalAgent agent) {
    if (identical(_current, agent)) {
      _current = null;
      pinnedCredential = null;
      invalidateCredential();
    }
  }

  /// 停掉并**等它真的停了**。没有 agent 时是 no-op。
  Future<void> stop() async {
    final agent = _current;
    _current = null;
    pinnedCredential = null;
    invalidateCredential();

    // provider 重建时 onDispose 与新 build 都会请求停止。把动作串成一条
    // future 链，后来者等待的是同一次真实退出，而不是在旧进程还没死时
    // 抢先拉起下一份。
    final stopped = _stopTail.then((_) async {
      if (agent != null) await agent.stop();
    });
    _stopTail = stopped.catchError((Object error, StackTrace stack) {
      debugPrint('等待本地 agent 停止失败：$error');
    });
    await stopped;
  }

  /// 只停仍由 [expected] 占据当前位置的进程。
  ///
  /// onDispose 是异步触发的；若新一轮已经 adopt 了 B，旧闭包晚到的 stop
  /// 不许误杀 B。
  Future<void> stopIfCurrent(LocalAgent expected) {
    if (!identical(_current, expected)) return Future<void>.value();
    return stop();
  }
}

class _LocalAgentCredentialRevision extends Notifier<int> {
  @override
  int build() => 0;

  void advance() => state++;
}

/// [LocalAgentHandle] 是可变对象，Riverpod 看不见它内部的围栏变化。
/// 凭据成功接住或失效时推进这个版本号，让主 API 在同一帧内改道远端或
/// 回到已验明的本地 agent。
final _localAgentCredentialRevisionProvider =
    NotifierProvider<_LocalAgentCredentialRevision, int>(
      _LocalAgentCredentialRevision.new,
    );

/// 远端不要凭据时，桌面端与本地 agent 之间的一次性凭据。
///
/// 进程内生成一次、只活到退出。它保护的是「同机其他进程别来指挥这个
/// 能执行命令的 agent」——够不着的人猜不到，够得着的人（同一个用户）
/// 本来就能读这个进程的内存。
///
/// 两种部署会走到它：离线模式（压根没有 cortexd），以及
/// `CORTEX_AUTH=disabled` 的自托管（有 cortexd，但它不认证）。
String _newLocalControlSecret() {
  final rng = Random.secure();
  return List.generate(
    32,
    (_) => rng.nextInt(256).toRadixString(16).padLeft(2, '0'),
  ).join();
}

final String _sessionSecret = _newLocalControlSecret();

/// 桌面端始终用独立随机凭据控制本地 agent，用户 token 只用于远端账号。
///
/// 启动、热更新与本机 HTTP 客户端必须使用同一把控制凭据。它不随账号刷新
/// 改变，也不会被当成远端 access token；真实启动还会为每个新进程单独签发。
@visibleForTesting
String localAgentToken(String? _) => _sessionSecret;

/// 客户端这一次要发的凭据。[onLocalAgent] 决定它打的是 agent 还是远端。
///
/// 只在指向 agent 时替换：把这把本机凭据发给一个真的要认证的 cortexd，
/// 换回来的是一个内容完全不同的 401 —— 而两种 401 在界面上长得一样。
/// [pinned] 是 agent 启动时钉住的那把（见 [`LocalAgentHandle.pinnedCredential`]）。
/// 它为 null 只出现在「agent 刚起、还没钉上」的一瞬，那时回落到当前值 ——
/// 与钉之前的行为一致。
@visibleForTesting
String? apiToken({
  required String? userToken,
  required bool onLocalAgent,
  String? pinned,
}) => onLocalAgent ? (pinned ?? localAgentToken(userToken)) : userToken;

/// 把轮换过的凭据推给**还在跑的**那个 agent。
///
/// 这里必须返回明确结论。204 表示服务端验明 deployment/user 未变，可以热换；
/// 409 表示已经切到另一账号；其余失败则无法证明边界仍成立。后两类由调用方
/// 立刻撤掉本地路由并关闭重起，不能只写一行日志后继续宣称 ready。
Future<LocalAgentCredentialPushResult> _pushAgentCredential({
  required String origin,
  required String inbound,
  required String? outbound,
  http.Client Function()? clientFactory,
}) async {
  final ownsClient = clientFactory == null;
  final client = clientFactory?.call() ?? http.Client();
  try {
    final resp = await client
        .put(
          Uri.parse('$origin/local/credential'),
          headers: {
            'authorization': 'Bearer $inbound',
            'content-type': 'application/json',
          },
          body: jsonEncode({'token': outbound}),
        )
        .timeout(const Duration(seconds: 5));
    return agentCredentialPushResultForStatus(resp.statusCode);
  } on Object catch (e) {
    debugPrint('新凭据没能推给本地 agent：$e');
    return LocalAgentCredentialPushResult.failed;
  } finally {
    if (ownsClient) client.close();
  }
}

final localAgentOriginProvider = FutureProvider<String?>((ref) async {
  // ── 每多 watch 一样东西，冷启动就多杀一个健康 agent ──
  //
  // 这个 provider 的每一次重建都会 `onDispose` → `agent.stop()` → 杀掉
  // 一个刚 ready 的进程再拉一个。而它 watch 的东西在冷启动时是**逐个
  // 异步落定**的（磁盘设置、凭据库、网络续期），于是「落定几样就杀几次」。
  //
  // 所以这里 watch 的每一位都必须是**真的会改变 agent 该怎么起**的：
  // 只 select 那三个字段（useMock / baseUrl / offline），而不是整个
  // AppConfig —— 后者的 `==` 含 `knownBaseUrls`，那是「你用过哪些地址」
  // 的清单，与 agent 怎么起毫无关系，而它恰好在冷启动时从磁盘晚到一次。
  final useMock = ref.watch(appConfigProvider.select((c) => c.useMock));
  final baseUrl = ref.watch(appConfigProvider.select((c) => c.baseUrl));
  final offline = ref.watch(appConfigProvider.select((c) => c.offline));
  // 生命周期看的是「认证门已经为谁打开了」，不是手里有没有一串 token。
  // 关认证的部署与离线模式都在 ready 且 token 为 null；同一账号续期则
  // ready 不变、token 值变。前者必须启动，后者不能重启。
  final authReady = ref.watch(
    authControllerProvider.select((s) => s.phase == AuthPhase.ready),
  );
  // **watch 而不是 read**：改了本机模型配置要让 agent 带着新环境重启。
  // read 的话用户会保存、看到成功、然后发现模型还是老的 —— 而那时
  // 界面上没有任何东西提示他需要重启。
  //
  // 但**只在离线模式下 watch**：这份配置只有离线时才注入进环境
  // （见下面的 extraEnv），在线时它变不变都与 agent 无关。而它每次冷启动
  // 必然变一次（AsyncLoading → AsyncData，跨进程读凭据库），于是无条件
  // watch 的那一版让**每一次冷启动**都白白杀掉一个刚 ready 的 agent。
  final localLlm = offline ? ref.watch(localLlmProvider).value : null;
  // The sidecar reads executable paths only at process launch. Watching this
  // provider intentionally restarts the local agent when the user changes the
  // hard app boundary; leaving it as `read` would show a successful save while
  // the running driver continued using the old allowlist.
  final computerUseAllowlist = ref.watch(computerUseAllowlistProvider);
  final packagedComputerAllowlist = ref.watch(
    computerPackagedAllowlistProvider,
  );
  final browserSiteAllowlist = ref.watch(computerBrowserAllowlistProvider);
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
  final handle = ref.read(localAgentHandleProvider);
  // 新身份的第一条规则：旧进程先停。onDispose 只能发出一个不等待的停止，
  // 所以这里再等同一个 handle 的串行链，确保它返回时旧进程已经咽气。
  await handle.stop();
  if (useMock || !kLocalAgentSupported) return null;
  if (!offline && !authReady) return null;
  final token = authReady ? ref.read(authControllerProvider).token : null;
  // 离线模式：**没有 cortexd**，本地 agent 就是全部 —— 必须起，
  // 而且没有 token 可给（也没人会来校验它）。不放行的话这个模式
  // 什么也不是：没有模型、没有工具、只有一个空界面
  if (!offline && needsToken && token == null) return null;

  final agent = ref.read(localAgentDiscoveryProvider)();
  if (agent == null) return null;

  handle.adopt(agent);
  // 切号、登出或重启都不能复用旧进程的控制 key；同账号续期只热换出站凭据。
  final inbound = _newLocalControlSecret();

  var disposed = false;
  Timer? restartTimer;
  ref.onDispose(() {
    disposed = true;
    restartTimer?.cancel();
    unawaited(handle.stopIfCurrent(agent));
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

  final pushCredential = ref.read(localAgentCredentialPushProvider);
  Future<void> credentialQueue = Future<void>.value();

  void advanceCredentialRevision() {
    if (ref.mounted) {
      ref.read(_localAgentCredentialRevisionProvider.notifier).advance();
    }
  }

  Future<void> applyCredential(String? outbound) async {
    if (disposed || !ref.mounted) return;

    LocalAgentCredentialPushResult result;
    try {
      result = await pushCredential(
        origin: agent.origin!,
        inbound: inbound,
        outbound: outbound,
      );
    } on Object catch (e) {
      debugPrint('新凭据推给本地 agent 时异常：$e');
      result = LocalAgentCredentialPushResult.failed;
    }
    if (disposed || !ref.mounted) return;

    if (result == LocalAgentCredentialPushResult.accepted) {
      // 推送期间可能又换了 token。只把服务端真正确认过的那把标成可用；
      // 更新那一把会由串行队列里的下一项验证。
      if (ref.read(authControllerProvider).token == outbound) {
        handle.acceptOutboundCredential(outbound);
        advanceCredentialRevision();
      }
      return;
    }

    // 409 与任何无法判定的失败都必须失败关闭。先撤掉路由，再等待进程
    // 退出，最后 invalidate；不是一个只写日志的旁路。
    handle.invalidateCredential();
    advanceCredentialRevision();
    if (result == LocalAgentCredentialPushResult.identityChanged) {
      budget.resetForIdentityChange();
      debugPrint('本地 agent 的 deployment/user 已变化 —— 停旧进程后按新账号重起');
    } else {
      debugPrint('本地 agent 没接住新凭据 —— 停旧进程后重试');
    }
    try {
      await handle.stopIfCurrent(agent);
    } on Object catch (e) {
      // 停止清理本身失败时，凭据路由已经在上文撤掉；仍要继续 invalidate，
      // 让下一次装配走重试路径，而不是让一个失败把凭据队列永久卡死。
      debugPrint('停止旧本地 agent 失败，保持远端路由并重试装配：$e');
    }
    if (disposed || !ref.mounted) return;
    ref.invalidateSelf();
  }

  void enqueueCredential(String? outbound) {
    credentialQueue = credentialQueue
        .catchError((Object error, StackTrace stack) {
          debugPrint('本地 agent 凭据队列上一项异常：$error');
        })
        .then((_) => applyCredential(outbound));
  }

  try {
    final origin = await agent.start(
      remote: baseUrl,
      // 没有用户 token 时（离线、或 `CORTEX_AUTH=disabled`）用那把一次性
      // 凭据。**不能传空串**：agent 会拿到 `Some("")`、以为自己有认证、
      // 把桌面端 401 挡在外面，而「不做认证」那条警告一次都不打。
      //
      // 走 `_localAgentToken` 而不是在这里写 `token ?? _sessionSecret`：
      // `cortexApiProvider` 那侧必须发同一个值，见那个函数的文档
      token: inbound,
      userToken: token,
      // 离线模式必须本地直连模型：代理那条路要经 cortexd，而它不存在。
      // 传 null 表示「不干预」，让 agent 自己按环境变量决定
      llmRoute: offline ? 'direct' : null,
      // 上次拨到「开」就照旧开着。**只是启动默认值** —— 权威是运行时那个
      // 开关（见 `remoteAttachProvider`）。存这一份是为了让「我开过了」
      // 活过一次重启，而不是让它成为第二个真相
      allowRemoteAttach: await _savedRemoteAttach(ref),
      computerUseAllowlist: computerUseAllowlist,
      packagedComputerAllowlist: packagedComputerAllowlist,
      browserSiteAllowlist: browserSiteAllowlist,
      // 注入的环境变量，两个来源合一：
      //   - 本机模型配置（仅离线模式；连着服务器时模型是 cortexd 的事）
      //   - Windows 沙箱后端选择（任何模式都可能选受限令牌档）
      extraEnv: {
        if (offline) ...?localLlm?.toEnvironment(),
        ...await savedSandboxBackendEnv(ref),
      },
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

    // provider 可能在 `start` 的 await 期间被 deployment 变化取代。此时
    // 不能把刚拉起来的孤儿 agent 登记成 ready。
    if (disposed || !ref.mounted) {
      await handle.stopIfCurrent(agent);
      return null;
    }

    final initialOutbound = ref.read(authControllerProvider).token;
    LocalAgentCredentialPushResult initialResult;
    try {
      initialResult = await pushCredential(
        origin: origin,
        inbound: inbound,
        outbound: initialOutbound,
      );
    } on Object catch (e) {
      debugPrint('启动后验证本地 agent 凭据时异常：$e');
      initialResult = LocalAgentCredentialPushResult.failed;
    }
    if (disposed || !ref.mounted) {
      await handle.stopIfCurrent(agent);
      return null;
    }
    if (initialResult != LocalAgentCredentialPushResult.accepted) {
      handle.invalidateCredential();
      advanceCredentialRevision();
      if (initialResult == LocalAgentCredentialPushResult.identityChanged) {
        budget.resetForIdentityChange();
      }
      await handle.stopIfCurrent(agent);
      scheduleRestart(
        initialResult == LocalAgentCredentialPushResult.identityChanged
            ? '凭据主体与启动身份不一致'
            : '启动后的凭据验证失败',
      );
      return null;
    }

    // 只有服务端验明 deployment/user 之后，客户端才把请求路由进这个
    // agent。入站凭据钉死在这一刻；出站凭据随同账号续期热替换。
    handle.bindCredential(inbound: inbound, outbound: initialOutbound);
    advanceCredentialRevision();
    debugPrint('本地 agent 已就绪：$origin（工具在本机执行）');

    // 出站那把跟着轮换。204 不重启；409 或失败会走上面的失败围栏。
    ref.listen(authControllerProvider.select((s) => s.token), (_, next) {
      enqueueCredential(next);
    });
    // start 与挂监听之间用户可能正好续期过一次；补推一次现值，
    // 差一次推送的后果是 agent 拿着已过期的凭据去写 episode
    final now = ref.read(authControllerProvider).token;
    if (now != initialOutbound) {
      enqueueCredential(now);
    }
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

/// [AuthController] 够得到的、关于本机 agent 的两个动作。
///
/// # 为什么要绕这一圈，而不是让它直接 read `localAgentOriginProvider`
///
/// 那个 provider **watch 着 `authControllerProvider`**（token 有无、
/// requiresToken 都是它的输入）。AuthController 反过来 read 它，就是
/// 依赖图上的一个环 —— riverpod 的循环依赖断言当场抛
/// `CircularDependencyError`（实测：白续裁决第一版就这么写，被既有的
/// futile 测试抓住）。这个网关自己**零依赖**：动作发生在调用时刻、
/// 用的是网关自己的 ref，不进 AuthController 的依赖图。
final localAgentGatewayProvider = Provider<LocalAgentGateway>(
  LocalAgentGateway.new,
);

class LocalAgentGateway {
  LocalAgentGateway(this._ref);

  final Ref _ref;

  /// 此刻活着的本机 agent 的地址；没有（Web / 还没就绪 / 起不来）是 null。
  String? get origin => _ref.read(localAgentOriginProvider).value;

  /// 重启它。invalidate 是懒生效的 —— 下一个读它的人拿到新一轮的构建。
  void restart() => _ref.invalidate(localAgentOriginProvider);
}

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

  // ── 没到 ready 就发一个「门是关的」桩，而不是一个真客户端。──
  //
  // 消费方（会话列表、项目、确认轮询、/auth/me……）是 App 一起动就 fire 的，
  // 而且以后还会添新的。在每个消费方里各开一道「登录了吗」的门是打地鼠 ——
  // 漏一个，登录页阶段就多一串 401。把门装在唯一的出口上，现在和将来的
  // 消费方就都拦住了。
  //
  // 三个非 ready 相位都该拦：needsToken（服务端要凭据而我们没有 ——
  // 发出去只能是 401）、probing（还不知道要不要凭据 —— 发出去是在赌）、
  // unreachable（用户填的地址没通 —— 发出去只是超时）。桌面端的离线模式
  // 与关认证的部署都落在 ready（见 `AuthPhase.ready` 的注释），
  // 这道门不会挡住它们；登录与续期走的是 `authProbeApiProvider`，
  // 也不经过这里。
  //
  // select 的是**布尔**而不是 phase 本身：probing→needsToken 这类
  // 非 ready 相位之间的跳变不该换桩 —— 换了，全部消费方就白白重建一轮。
  final ready = ref.watch(
    authControllerProvider.select((s) => s.phase == AuthPhase.ready),
  );
  if (!ready) {
    final api = GateClosedApi();
    ref.onDispose(api.dispose);
    return api;
  }

  final token = ref.watch(authControllerProvider.select((s) => s.token));
  // handle 本身可变，Riverpod 看不见凭据围栏内部的状态变化；这个版本号
  // 会在服务端接受新凭据、或推送失败撤下旧 agent 时推进，让主 API 在同一帧
  // 内改道远端或回到已验明的本地 agent。
  ref.watch(_localAgentCredentialRevisionProvider);

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
  final handle = ref.read(localAgentHandleProvider);
  final onLocalAgent =
      agentOrigin.value != null && handle.acceptsOutboundCredential(token);
  final origin = onLocalAgent ? agentOrigin.value! : config.baseUrl;

  final api = HttpCortexApi(
    baseUrl: origin,
    // 打的是本机 agent 时，把**用户配的那个地址**一并交给它 ——
    // 连不上时那句话要说得出「你配的是哪儿、这个 127.0.0.1 又是谁」
    frontsDeployment: onLocalAgent ? config.baseUrl : null,
    // 打到 agent 时用**它认的那把**，见 `apiToken`
    // 打到 agent 时用**钉住的那把**：它认的是自己启动时那个值，
    // 而 `token` 每 15 分钟就换一次
    token: apiToken(
      userToken: token,
      onLocalAgent: onLocalAgent,
      pinned: handle.pinnedCredential,
    ),
    // 生产恒 null（HttpCortexApi 自建）。这个口子只为测试存在：没有它，
    // 下面 onUnauthorized 闭包的接线（资格审查用哪边的 token）从测试里
    // 够不着 —— 上一轮它只有纯函数测试，接线错了照样全绿。
    client: ref.watch(httpClientFactoryProvider)?.call(),
    // `read`, not `watch`: this is an outbound edge. Watching the notifier
    // would make every auth state change rebuild the client, including the one
    // this very callback triggers.
    onUnauthorized: () {
      // ── 迟到的 401 没资格拉当前凭据下马。──
      //
      // 401 经 `scheduleMicrotask` 报上来，而 token 一换这个实例就被换掉
      // （见上面 watch token 那段）—— 于是「上一代实例发的请求」的 401
      // 完全可能在**新凭据已经生效之后**才落地。放它进去的实际后果
      // （生产复现过）：登录成功 → 登录前发出的无凭据请求的 401 迟到抵达
      // → 触发续期（成功）→ 3 秒内又一条迟到 401 → 熔断器判「刚续过还
      // 401」→ 把刚登录的人打回登录页，红字「登录已过期」。
      //
      // 判据是**这个实例被造出来时的 token 还是不是当前那把**：不是，
      // 说明它报的是一把已经退位的凭据，与现任无关，丢掉。
      if (!ref.mounted) return;
      // 离线模式没有「远端会话」这回事：一个主动选了离线的用户手里
      // 本来就没有凭据（token 恒 null，与现任 null 相等，光靠上面的
      // 判据拦不住），此时打向远端的任何 401 都不构成「你被登出了」。
      // 不拦的实际后果（审查复现过）：点「离线使用」→ ready →
      // 本地 agent 还没起、请求先落到远端 → 401 → 被打回登录页，
      // 且 offline 已是 true，「离线使用」按钮从此同值短路、点不动。
      if (ref.read(appConfigProvider).offline) return;
      // ⚠️ 这里**不再**按「agent 在跑」就重启 agent。
      //
      // 那条老路的前提是「401 = 本机入站凭据错位」，而 401 还有另一个
      // 来源：**远端经反代转回来的**（用户凭据在服务端已失效）。老路
      // 分不出两者，把远端 401 也当成错位去重启 —— 重启治不了远端，
      // 于是下一次轮询又 401、又重启：2026-08-21 实测 agent 被 1 秒一次
      // 地杀了 639+ 次。现在 agent 自己拒的 401 带
      // `x-cortex-denied-by: local-agent` 头，走 [onLocalAgentRejected]
      // 那条带预算的路；走到这里的 401 一律按「远端不认」处理。
      if (!shouldForwardUnauthorized(
        instanceToken: token,
        currentToken: ref.read(authControllerProvider).token,
      )) {
        return;
      }
      ref.read(authControllerProvider.notifier).onUnauthorized();
    },
    // agent **自己**拒的 401（带 x-cortex-denied-by 头）：入站凭据错位，
    // 让它带着对的凭据重来一次 —— 但**有预算**。没有预算的自愈动作
    // 不是自愈，是把一次故障变成永动机（见 _RestartBudget.allowKick）。
    onLocalAgentRejected: () {
      if (!ref.mounted) return;
      // 旧实例可能在凭据换代后才收到这条 401。只有当前 token 仍被这个
      // agent 接住时，才允许把“入站错位”作为重启理由。
      if (!handle.acceptsOutboundCredential(
        ref.read(authControllerProvider).token,
      )) {
        return;
      }
      if (!ref.read(_agentRestartBudgetProvider).allowKick(DateTime.now())) {
        debugPrint(
          '本机 agent 30 秒内第 3 次拒绝入站凭据 —— 不再重启。'
          '错位重启一次就该好，还在 401 说明根因不在错位，再杀进程也没用',
        );
        return;
      }
      debugPrint('本机 agent 拒绝了这次请求（401）—— 重起它，不动登录态');
      ref.invalidate(localAgentOriginProvider);
    },
  );
  ref.onDispose(api.dispose);
  return api;
});

/// 测试注入 HTTP 层的口子。生产不 override，恒 null。
@visibleForTesting
final httpClientFactoryProvider = Provider<http.Client Function()?>(
  (_) => null,
);

/// 一个 401 报告要不要转给 [AuthController.onUnauthorized]。
///
/// 拆成纯函数是为了可测：真正的调用点在 `cortexApiProvider` 的闭包里，
/// 从测试里够不着。
@visibleForTesting
bool shouldForwardUnauthorized({
  required String? instanceToken,
  required String? currentToken,
}) => instanceToken == currentToken;

/// 登录门没开时 `cortexApiProvider` 发出的桩：任何调用立刻抛，不碰网络。
///
/// # 为什么抛而不是静默空转
///
/// 空列表、空流这类「温和」的返回值会把「还没登录」伪装成「没有数据」——
/// 一个界面要是在门没开时读到了空会话列表，它会如实渲染「没有会话」，
/// 而那是假的。抛出去，消费方现有的错误路径（都有 try/catch）自然接住，
/// 且这些界面全在登录门之后，用户根本看不到。
///
/// # 为什么用 `noSuchMethod`
///
/// [CortexApi] 有几十个方法且还在长。逐个写 `throw` 意味着每加一个方法
/// 都要记得来这里补一刀 —— 忘了的那一个会在门关着时真的发请求。
/// `noSuchMethod` 让「新方法默认被拦」成为不需要人记得的事。
/// 这一份 api 是「登录门没开」的那个桩吗。
///
/// 导出一个判定而不是把 [GateClosedApi] 本身放开：调用方要判断的是**状态**
/// （此刻有没有可用的凭据），不是要拿到那个类去做别的事。
bool isGateClosed(CortexApi api) => api is GateClosedApi;

@visibleForTesting
class GateClosedApi implements CortexApi {
  static const _closed = CortexApiException(
    // ASCII 的「gate-closed」是给产物验证用的指纹：中文在 dart2js 产物里
    // 是 \uXXXX 转义，grep 不到。
    '还没登录，这个请求没有发出去（gate-closed）',
    statusCode: 401,
  );

  /// provider 换代时会被调用（`ref.onDispose`），必须真的存在且不抛。
  @override
  void dispose() {}

  // ── 四个 Stream 成员必须显式实现成 Stream.error，不能走 noSuchMethod。──
  //
  // 真实现全是 async*，**永远不会同步抛**，调用方据此把「先置 running
  // 状态、再 listen」写成了非 async 的直筒代码（ImportController.start）。
  // noSuchMethod 的同步抛会在 `.listen` 之前就穿出去：状态卡死在
  // running、订阅号还是 null、异常直冲 unhandled zone —— 会话过期时
  // 开着导入对话框点「开始导入」就能命中。Stream.error 保住时序契约，
  // 错误从调用方现成的 onError 路径走。`RunAttachUnsupported` 是同一
  // 判断的先例。
  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    Assistant? assistant,
    List<Skill> skills = const [],
    bool computerUse = false,
    ImagePrefs? imagePrefs,
  }) => Stream.error(_closed);

  @override
  Stream<ChatEvent> attachChat(String sessionId) => Stream.error(_closed);

  @override
  Stream<SyncEvent> watchSync() => Stream.error(_closed);

  @override
  Stream<ImportEvent> runImport(ImportTarget target, {int? maxConversations}) =>
      Stream.error(_closed);

  @override
  dynamic noSuchMethod(Invocation invocation) => throw _closed;
}

/// `GET /health`, polled lazily (on demand + on manual refresh).
final healthProvider = FutureProvider<HealthStatus>((ref) async {
  final api = ref.watch(cortexApiProvider);
  return api.health();
});

/// `GET /sandbox/health` —— 「这个地址跑不跑得了**云端对话**」。
///
/// # 为什么是第二个 provider，而不是并进 [healthProvider]
///
/// 生产上这两条路由由边缘分给了**两个不同的进程**（`/health` 归记忆服务，
/// `/sandbox/health` 才是 agent 编排服务），一次请求问不完。
///
/// 更要紧的是它们**失败的含义不一样**：`/health` 不通是「连不上」，
/// 而这一条不通往往只是「这个部署没有沙箱」——自托管与纯本机形态的常态。
/// 合成一个 provider 之后，后者会把前者也拖成错误态，于是界面上一个
/// 完全健康的本机部署会显示成连接故障。
final sandboxHealthProvider = FutureProvider<SandboxHealth>((ref) async {
  final api = ref.watch(cortexApiProvider);
  return api.sandboxHealth();
});

/// 默认工作空间根目录 + 它下面已有的文件夹。
///
/// # 为什么失败要落成「没有」而不是抛出去
///
/// Web 端**必然**拿不到它（那条路由只有本地 agent 有），而那不是故障 ——
/// 浏览器里本来就没有本机目录可选。让它抛的话，每个读这个 provider 的界面
/// 都得自己判一次「这个错是不是其实正常」，而其中一处判漏就会在 Web 上
/// 弹一个红框说本机工作空间取不到。
final localWorkspaceRootProvider = FutureProvider<LocalWorkspaceRoot>((
  ref,
) async {
  if (!kLocalAgentSupported) return LocalWorkspaceRoot.empty;
  final api = ref.watch(cortexApiProvider);
  try {
    return await api.localWorkspaceRoot();
  } on CortexApiException {
    return LocalWorkspaceRoot.empty;
  }
});

/// 这台机器上接着的那些 MCP server。
///
/// # 取不到时**回「没有」而不是抛**
///
/// 与 [localWorkspaceRootProvider] 同一条理由：Web 端与旧版本的本地 agent
/// 必然拿不到这几条路由，而那不是故障。让它抛的话，MCP 那一页得自己判一次
/// 「这个错是不是其实正常」，而判漏就会在 Web 上弹一个红框说 MCP 取不到。
///
/// 「没有」与「有但一台都没配」用 `path` 区分：前者是空串（这个后端答不了），
/// 后者有真实路径（答得了，只是列表空）—— 界面上那是两句完全不同的话。
final mcpConfigProvider = FutureProvider<McpConfigView>((ref) async {
  if (!kLocalAgentSupported) return McpConfigView.empty;
  final api = ref.watch(cortexApiProvider);
  try {
    return await api.mcpConfig();
  } on CortexApiException {
    return McpConfigView.empty;
  }
});

/// System-following theme mode with a manual override.
/// 浅色 / 深色 / 跟随系统。
///
/// # 为什么要落盘
///
/// 在此之前它**只活在内存里**：一个把应用调成深色的人，每次启动都会被
/// 白底闪一下，然后要自己再点回去。而顶栏那个按钮是循环的，
/// 「回到深色」在最坏情况下要点两下 —— 一个每次启动都要做一遍的动作。
///
/// 与权限档同一套（[settingsPatcherProvider]）：读不出来时默认浅色，
/// 让新安装先看到一致、明亮的桌面界面；用户仍可切换到深色或跟随系统。
class ThemeModeNotifier extends Notifier<ThemeMode> {
  static const String _key = 'theme_mode';

  @override
  ThemeMode build() {
    Future.microtask(_restore);
    return ThemeMode.light;
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final mode = switch (saved[_key]) {
      'light' => ThemeMode.light,
      'dark' => ThemeMode.dark,
      'system' => ThemeMode.system,
      // 认不出来（包括没存过、以及文件坏了）都用现代白色主题
      _ => ThemeMode.light,
    };
    if (mode != state) state = mode;
  }

  void set(ThemeMode mode) {
    if (state == mode) return;
    state = mode;
    unawaited(ref.read(settingsPatcherProvider)(_key, mode.name));
  }

  void cycle() => set(switch (state) {
    ThemeMode.system => ThemeMode.light,
    ThemeMode.light => ThemeMode.dark,
    ThemeMode.dark => ThemeMode.system,
  });
}

final themeModeProvider = NotifierProvider<ThemeModeNotifier, ThemeMode>(
  ThemeModeNotifier.new,
);

/// 界面密度：默认 / 紧凑。
///
/// # 为什么只有两档、且默认档一动不动
///
/// 「紧凑」是给桌面端一次看更多行的人的（行高 −6px、正文 14px/1.66）。
/// 默认档保持原样 —— 长回答需要那个行距，把它顺手收紧等于替所有人做了
/// 一个只有一部分人要的决定。落地在 `CortexTheme.light/dark(compact:)`。
class DensityNotifier extends Notifier<bool> {
  static const String _key = 'ui_density_compact';

  @override
  bool build() {
    Future.microtask(_restore);
    return false;
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    // 只有明确存过 'true' 才紧凑 —— 键名改过、文件坏了都落回默认，
    // 与电脑操作那个开关同一条纪律
    final compact = saved[_key] == 'true';
    if (compact != state) state = compact;
  }

  void set({required bool compact}) {
    if (state == compact) return;
    state = compact;
    unawaited(ref.read(settingsPatcherProvider)(_key, '$compact'));
  }
}

final densityProvider = NotifierProvider<DensityNotifier, bool>(
  DensityNotifier.new,
);

/// 动效三档。见 [MotionPref]。
///
/// 默认 [MotionPref.system]，也就是与加这个设置之前**行为完全一致** ——
/// 一个无障碍开关不该因为我们多给了一个选项就改变既有用户的观感。
class MotionPrefNotifier extends Notifier<MotionPref> {
  static const String _key = 'motion';

  @override
  MotionPref build() {
    Future.microtask(_restore);
    return MotionPref.system;
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final pref = MotionPref.fromWire(saved[_key]);
    if (pref != state) state = pref;
  }

  void set(MotionPref pref) {
    if (state == pref) return;
    state = pref;
    unawaited(ref.read(settingsPatcherProvider)(_key, pref.wire));
  }
}

final motionPrefProvider = NotifierProvider<MotionPrefNotifier, MotionPref>(
  MotionPrefNotifier.new,
);

/// 界面缩放与聊天文字大小。档位与组合规则见 [UiScale]（core/ui_scale.dart）。
///
/// # 为什么两个值合在一个 notifier
///
/// 聊天档的语义是「**乘在界面档之上**」—— 两个值要一起读才算得清聊天区
/// 最终多大。拆成两个的话，改界面档的重建会让聊天档的消费者各自读各自的
/// 旧值，一帧之内出现「界面变了聊天没跟上」的中间态。
class UiScaleNotifier extends Notifier<UiScale> {
  static const String _uiKey = 'ui_scale';
  static const String _chatKey = 'chat_text_scale';

  @override
  UiScale build() {
    Future.microtask(_restore);
    return const UiScale();
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final ui = parseScale(saved[_uiKey]) ?? kDefaultScale;
    final chat = parseScale(saved[_chatKey]) ?? kDefaultScale;
    if (ui != state.ui || chat != state.chat) {
      state = UiScale(ui: ui, chat: chat);
    }
  }

  void setUi(double v) {
    if (state.ui == v) return;
    state = UiScale(ui: v, chat: state.chat);
    unawaited(ref.read(settingsPatcherProvider)(_uiKey, '$v'));
  }

  void setChat(double v) {
    if (state.chat == v) return;
    state = UiScale(ui: state.ui, chat: v);
    unawaited(ref.read(settingsPatcherProvider)(_chatKey, '$v'));
  }
}

final uiScalePrefsProvider = NotifierProvider<UiScaleNotifier, UiScale>(
  UiScaleNotifier.new,
);
