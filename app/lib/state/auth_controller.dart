import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../api/http_cortex_api.dart';
import '../auth/token_store.dart';
import '../core/auth_gate_log.dart';
import '../models/health_status.dart';
import 'app_providers.dart';

/// Where the client stands with respect to `cortexd`'s front door.
enum AuthPhase {
  /// `GET /health` is in flight. Nothing is known yet, including whether a
  /// credential will be wanted.
  probing,

  /// 连不上 —— **而且是用户主动去连的那一次**。
  ///
  /// 与 [needsToken] 分开，因为要做的事不一样：去起服务、去改地址，而不是
  /// 去找一把 token。给一个服务器根本没起的人看密码框是一种小小的残忍。
  ///
  /// # 它不再包括「开机探测发现连不上」
  ///
  /// 那一档现在落进离线模式（`AuthController._fallBackToOffline`）。首次运行
  /// 时地址是编译期默认值，那儿多半什么都没有 —— 拦一张「登录一个不存在的
  /// 服务器」的表单，等于让新用户在见到产品之前先卡住。
  ///
  /// 于是这一档剩下的含义很窄，也更准：**你填了地址、按了登录，没通上。**
  /// 那时停在表单上是对的 —— 他正要改的就是那两个输入框。
  unreachable,

  /// The daemon checks credentials and we do not have an accepted one.
  needsToken,

  /// A credential was presented and accepted, or the daemon does not check.
  ready,
}

class AuthState {
  const AuthState({
    this.phase = AuthPhase.probing,
    this.token,
    this.health,
    this.error,
    this.busy = false,
    this.remember = false,
    this.refreshToken,
  });

  final AuthPhase phase;

  /// The accepted credential, or null when none is held.
  ///
  /// Never rendered. [toString] is overridden below because a `Notifier`'s
  /// state ends up in Riverpod observers and error messages, and "the token
  /// leaked through a debug print" is a failure mode with no symptom.
  final String? token;

  /// Last `/health` reading. Drives the "this deployment has authentication
  /// turned off" notice as well as the version line.
  final HealthStatus? health;

  /// Why the last attempt failed, in the daemon's own words where it had any.
  final String? error;

  /// A sign-in or probe is running.
  final bool busy;

  /// Whether the user asked the platform to keep the token. Only meaningful
  /// where [kCanRememberToken] is true.
  final bool remember;

  /// 长效凭据。**只有它值得存**，而且只存在系统凭据库里。
  ///
  /// 留在 state 里是为了两件事：续期时拿它去换，以及登出时告诉服务端
  /// 「作废这条链」—— 后者是本地删掉一份副本做不到的。
  ///
  /// 同样被 [toString] 抹掉。
  final String? refreshToken;

  bool get isReady => phase == AuthPhase.ready;

  /// True when the daemon told us it accepts anybody who can reach the port.
  bool get serverAuthDisabled => health?.authDisabled ?? false;

  AuthState copyWith({
    AuthPhase? phase,
    Object? token = _sentinel,
    Object? health = _sentinel,
    Object? error = _sentinel,
    bool? busy,
    bool? remember,
    Object? refreshToken = _sentinel,
  }) => AuthState(
    phase: phase ?? this.phase,
    token: token == _sentinel ? this.token : token as String?,
    health: health == _sentinel ? this.health : health as HealthStatus?,
    error: error == _sentinel ? this.error : error as String?,
    busy: busy ?? this.busy,
    remember: remember ?? this.remember,
    refreshToken: refreshToken == _sentinel
        ? this.refreshToken
        : refreshToken as String?,
  );

  static const Object _sentinel = Object();

  /// Redacted on purpose — see [token].
  @override
  String toString() =>
      'AuthState(phase: $phase, token: ${token == null ? 'none' : '<redacted>'},'
      ' busy: $busy)';
}

/// Owns the credential and the decision about whether one is needed.
///
/// ## Why the gate exists at all
///
/// `cortexd` refuses to start without credentials configured, so **every real
/// deployment checks them**. A client with no way to present one cannot talk to
/// any real server — it would show an empty session list and a `DOWN` badge,
/// with nothing anywhere explaining that the problem is a missing token.
///
/// ## Why `/health` decides, not a config flag
///
/// The alternative is a "this server needs a token" checkbox, which asks the
/// user a question the server can answer. `/health` is unauthenticated
/// precisely so it can be asked before anyone has a credential, and it reports
/// `auth: "token" | "disabled"`. So a loopback dev daemon running
/// `CORTEX_AUTH=disabled` lets the user straight through, and a real one asks —
/// with no setting to get wrong in either direction.
/// 把人送回登录页的几条路径。
///
/// # 为什么值得一个枚举
///
/// 三者对用户是差不多的一句红字，对我们却是三个不同的 bug 家族：
///
/// * [noRefreshToken] —— **本地存储**。手上压根没有 refresh token。
///   可能从来没存下来（存储写失败、Web 上被清），也可能被
///   `restoreSession` 里那次 `forgetToken()` 删掉了。
/// * [refreshRejected] —— **服务端真的拒了**（401/403）。这一条才需要看
///   服务端：被轮换过、判成重放、或者真的过期。配套日志在
///   `restoreSession` 的 catch 里，带状态码。
/// * [refreshStalled] —— **续期一直换不成，但不是凭据的错**（5xx、限流、
///   网络抖动），连着几次都这样。凭据**没有删**：下次启动还能拿它自动续。
/// * [refreshFutile] —— **续期一直成功，可新 token 照样被拒**，连着几次。
///   refresh 路由永远答应的话，上面两条都到不了 —— 这是那族「静默坏死」
///   唯一的终态（详见 `_futileRefreshes`）。
///
/// 2026-08-23 加的：用户报「开着不动几十分钟就掉」，而这几条在日志里
/// 长得一模一样，定位不了。
///
/// 这里曾有第四条 `cooloff`（刚续期成功 3 秒内又收到 401 → 踢）。
/// 2026-08-24 拆掉了：那个 401 几乎总是重建窗口里的迟到毛刺（新旧 client
/// 换代、服务端两台实例一新一旧），把它当「续了也没用」直接登出，用户
/// 每 15 分钟就有一次被误杀的机会 —— 正是「过一会儿突然退出」的来源之一。
/// 现在 cooloff 窗口内的 401 **忽略**；真坏的凭据等窗口过后由下一轮续期
/// 的 [refreshRejected] 裁决，循环风暴由 [refreshStalled] 的预算兜底。
enum _GateReason {
  noRefreshToken('手上没有 refresh token —— 要么从没存下来，要么被上一次续期失败时删了'),
  refreshRejected('服务端拒绝了这枚 refresh token —— 看紧邻的「续期被拒」那行日志的状态码'),
  refreshStalled('连续几次续期都没换成（服务端 5xx / 限流 / 网络）—— 凭据留着，稍后能自动续'),
  refreshFutile('续期一直成功、可新 token 照样被拒 —— 服务端多实例验签不同步或时钟偏差的形状，再签只是浪费');

  const _GateReason(this.explain);

  final String explain;
}

/// 一次续期尝试的三种结局 —— **谁的错，决定谁买单**。
///
/// 此前它是一个 bool，于是「服务端明确拒绝这枚凭据」（401/403）与
/// 「服务端此刻答不上来」（5xx、429 限流、连不上）落进同一个 false ——
/// 而两者该做的事截然相反：前者删凭据、回登录页；后者**什么都不该动**，
/// 凭据没毛病，动了它才是把一次服务端抖动升级成「你被登出了」。
///
/// 实测踩过的坑：生产每次发版重启 agentd 的那几秒里，恰逢续期的客户端
/// 拿到 502 —— 老代码把 30 天的凭据当场删掉，用户被踢回登录页，且下次
/// 启动再也自动续不上。日志里那句「续期被拒：status=501」同族。
enum RefreshOutcome {
  /// 换到了新的一对，已进主界面。
  ok,

  /// 服务端认得这条路由，并**明确拒绝了这枚凭据**（401/403）。
  /// 存储里的副本已删 —— 留着一个已作废的 token 只会让每次启动白跑。
  rejected,

  /// 没换成，但**不是凭据的错**：连不上、5xx、501。凭据原样留着。
  transient,

  /// 被服务端限流（429）。凭据原样留着。
  ///
  /// 与 [transient] 分开是因为**计账规则不同**：429 是服务端在说
  /// 「你来得太密，等 N 秒」，它与 5xx 叠加时（发版重启风暴正是这个形状：
  /// 全体客户端同时涌向 /auth/refresh，撞上按 token 摘要的每分钟 5 次限流）
  /// 会让 3 次的 transient 预算被一次不算长的抖动凑满 —— 于是一次发版把
  /// 在线的人集体送回登录页。429 走自己的（更宽的）预算。
  throttled,

  /// 这次尝试**被更新的操作取代**（换了地址、手动登录、_reset）。
  ///
  /// 单列一档而不是并进 [transient]：被取代与服务端健康毫无关系，
  /// 计进「服务端抖动」的踢人预算会让一次改地址的操作替服务端背黑锅 ——
  /// 日志与红字都会指向一个不存在的服务端问题。
  superseded,
}

class AuthController extends Notifier<AuthState> {
  /// Guards against a stale probe (from a previous base URL) writing its result
  /// over a newer one.
  int _generation = 0;

  @override
  AuthState build() {
    // A different daemon means a different credential: the token that opens one
    // is meaningless at the other, and silently carrying it over would produce
    // a 401 the user cannot connect to their own address change.
    ref.listen(appConfigProvider, (previous, next) {
      if (previous?.baseUrl != next.baseUrl ||
          previous?.useMock != next.useMock ||
          previous?.offline != next.offline) {
        _reset();
      }
    });
    Future.microtask(_bootstrap);
    return _seeded();
  }

  /// 启动时先**把上次的登录续上**，续不上再走探测。
  ///
  /// # 这一步曾经完全不存在
  ///
  /// 登录时把 refresh token 存进了系统凭据库（`rememberToken`），
  /// 而启动时**从来没有读回来** —— `restoreSession` 写好了、测过了，
  /// 全项目零调用。于是「登录一次管 30 天」这件事在产品里是不存在的：
  /// 每次重启都回到登录页，桌面端与 Web 都是。
  ///
  /// 读取是异步的（要跨进程问钥匙串），而 `build` 是同步的，
  /// 所以只能排到微任务里 —— 这也是当初漏掉它的原因：`_seeded()` 是同步的，
  /// 只拿得到环境变量那一份，看起来「已经在读了」。
  Future<void> _bootstrap() async {
    if (!ref.mounted) return;
    // 记下开工时的代次。**中途有别人接手就整个让位** ——
    // 这一段全是 await，而用户完全可能在它还没跑完时就手动登录了。
    //
    // 不让位的话，下面那次 `probe()` 会 `++_generation`，把那次**已经成功**
    // 的登录判成「已被取代」，于是他看到登录成功、界面却退回登录页。
    // 手快就会中招，而且一次都复现不出来。
    // ⚠️ **先等地址落定，再取代次。**
    //
    // 上次那个地址是异步从磁盘读回来的（`AppConfigNotifier._restore`），
    // 而它一落地就会触发下面那个 listener 的 `_reset()` → `++_generation`。
    // 不等的话，接下来那次「读凭据库 → 续会话」正好卡在中间，回来发现
    // 代次变了，把自己判成「已被取代」直接返回 —— **续期一次都没发出去**。
    //
    // 症状是存过自定义地址的用户每次启动都回到登录页，而登录页上写着
    // 「登录状态会记住 30 天」。这条路径原先的测试用一个覆写了 `build()`
    // 的替身替掉了 AppConfig，于是那段异步恢复从来没被跑过，测试一直是绿的。
    await ref.read(appConfigProvider.notifier).restored;
    if (!ref.mounted) return;

    var mine = _generation;
    bool superseded() => !ref.mounted || _generation != mine;
    // mock / 离线模式没有可续的会话，直接走各自的短路
    final config = ref.read(appConfigProvider);
    if (config.useMock || config.offline) {
      if (!superseded()) await probe();
      return;
    }

    final remembered = await ref.read(rememberedTokenProvider.future);
    if (superseded()) return;

    // 环境变量里那把预共享 token 不是 refresh token，续不了 —— 它走
    // 探测那条路（`/health` 说要凭据，而我们手上正好有一把）
    if (remembered != null && remembered != readSeedToken()) {
      state = state.copyWith(busy: true);
      // `restoreSession` **自己也会 `++_generation`**（它要防着自己被更新的
      // 请求盖掉）。那一次是我们发起的，不该算成「有人接手了」——
      // 算进去的话续期失败后这里直接返回，界面永远停在转圈上。
      //
      // 所以只认「除它之外还有人动过」：恰好多一次就是它自己。
      final before = _generation;
      if (await restoreSession(remembered) == RefreshOutcome.ok) return;
      if (_generation != before + 1) return;
      mine = _generation;
    }
    if (superseded()) return;
    await probe(fallBackToOffline: true);
  }

  /// Initial state, with whatever the platform can supply unattended.
  ///
  /// The seed is read here rather than at sign-in so that a token supplied by
  /// the environment (desktop) or kept for the tab (web) is already in hand
  /// when the probe discovers that one is required — that combination is what
  /// makes a correctly configured setup skip the login screen entirely.
  AuthState _seeded() {
    final seed = ref.read(authSeedTokenProvider);
    return AuthState(token: seed, remember: seed != null);
  }

  /// Asks the unauthenticated `/health`, then decides.
  ///
  /// [fallBackToOffline] 只有**开机那一次**该传 true。用户主动点「去连接」
  /// 之后再掉回离线，那个按钮就成了「点了没反应」—— 他要的正是看见失败原因。
  /// 见 [_fallBackToOffline]。
  Future<void> probe({bool fallBackToOffline = false}) async {
    final generation = ++_generation;
    if (!ref.mounted) return;

    // The mock source has no port and no daemon; probing it over HTTP would
    // hit whatever happens to be listening on the configured address. Short-
    // circuited here rather than by asking `cortexApiProvider` for the mock
    // instance, because that provider reads *this* controller for the token —
    // going the other way would close a dependency cycle.
    // 离线模式：没有 cortexd 可探。直接放行 —— 停在这里探一个明知
    // 不存在的地址，只会让用户对着一个转圈的界面等 20 秒超时
    if (ref.read(appConfigProvider).offline) {
      state = state.copyWith(
        phase: AuthPhase.ready,
        health: null,
        busy: false,
        error: null,
      );
      return;
    }

    if (ref.read(appConfigProvider).useMock) {
      state = state.copyWith(
        phase: AuthPhase.ready,
        health: null,
        busy: false,
        error: null,
      );
      return;
    }

    state = state.copyWith(busy: true, error: null);

    final probeApi = _probeApi();
    try {
      final health = await probeApi.health();
      if (!_alive(generation)) return;

      if (!health.requiresToken) {
        // Includes the mock source, which reports `disabled` because there is
        // no port to protect.
        state = state.copyWith(
          phase: AuthPhase.ready,
          health: health,
          busy: false,
          error: null,
        );
        return;
      }
      // A token in hand still has to be *accepted*; an expired or rotated one
      // looks identical to a good one until something authenticated is tried.
      final held = state.token;
      state = state.copyWith(health: health, busy: false);
      if (held != null && held.isNotEmpty) {
        await signIn(held, remember: state.remember);
      } else {
        state = state.copyWith(phase: AuthPhase.needsToken, busy: false);
      }
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return;
      _unreachable(e.message, fallBackToOffline);
    } on Object catch (e) {
      if (!_alive(generation)) return;
      _unreachable('$e', fallBackToOffline);
    } finally {
      probeApi.dispose();
    }
  }

  /// 连不上 cortexd —— **落进离线模式，而不是拦一张登录表单**。
  ///
  /// # 为什么不再拦
  ///
  /// 首次运行时地址是编译期默认值（`127.0.0.1:8080`），那儿多半什么都没有。
  /// 于是新用户第一眼看到的是「登录一个不存在的服务器」的表单 —— 他没有
  /// 账号、没有地址、也不知道自己该填什么，而产品本身一眼都没见着。
  ///
  /// 落进离线模式之后他至少能用：本地 agent 起得来、能对话、能读写他自己
  /// 机器上的文件。
  ///
  /// # 为什么必须同时把 `offline` 打开
  ///
  /// 不打开的话进去的是一个**坏掉的**界面：`localAgentOriginProvider` 那条
  /// 判据是「没有 token 且服务端要 token 就不起 agent」，而连不上时
  /// `health` 是 null、`requiresToken` 缺省为 true —— 于是本地 agent 根本
  /// 不启动，用户面对的是一个既没有记忆也没有工具的空壳。
  ///
  /// # 「这段时间没有记忆」谁来说
  ///
  /// `chat_pane` 里那条**常驻**横幅（`_OfflineBanner`）：「这些对话没有在
  /// 记忆里，它们排在本地队列，接上服务器后会自动补回去」，右边就是「去连接」。
  /// 那句话必须一直挂着而不是弹一次 —— 记忆是这个产品的主张，它关着的时候
  /// 界面上不能一个字都不说。
  ///
  /// # 那句失败原因去哪了
  ///
  /// **它在这里留不住，而这是对的。** `setOffline(true)` 会触发
  /// `ref.listen(appConfigProvider)` → `_reset()` → 重探，而重探的离线短路
  /// 那一支把 `error` 清成 null。
  ///
  /// 想过给它单开一个字段绕过去，但那是在解一个不该解的问题：此刻那句
  /// 「Connection refused」对用户没有可做的事 —— 他还没说他想连。等他点了
  /// 横幅上的「去连接」，`offline` 关掉、探测重来、这一次失败落进
  /// [AuthPhase.unreachable]，那句话就出现在他正要改的那两个输入框上面。
  ///
  /// 消息在它有用的那一刻出现，而不是一直挂着。
  void _unreachable(String why, bool fallBackToOffline) {
    if (!fallBackToOffline) {
      // 用户主动去连的那一次。停在表单上，把原因摆在他正要改的输入框上面
      state = state.copyWith(
        phase: AuthPhase.unreachable,
        busy: false,
        error: why,
      );
      return;
    }
    debugPrint('连不上 cortexd，落进离线模式：$why');
    ref.read(appConfigProvider.notifier).setOffline(true);
    state = state.copyWith(phase: AuthPhase.ready, busy: false);
  }

  /// 用户名 + 密码登录。
  ///
  /// 拿回来的 refresh token 存进系统凭据库，于是**下次打开不用再登录**。
  /// access token 留在内存里 —— 它 15 分钟就过期，存下来没有意义，
  /// 而每多一处副本就多一处泄露面。
  Future<void> signInWithPassword(String username, String password) async {
    // 说一句，而不是静默 return。**静默 return 的症状是「点登录没一点反应」**，
    // 而用户没法从一个什么都不做的按钮上看出自己漏了什么
    //
    // 例外：服务端指名了免密登成谁（开发机的 `CORTEX_DEV_LOGIN`）。
    // 那件事**只有服务端知道**，所以判据取自刚探到的 `/health`，
    // 而不是客户端这边的什么编译期开关 —— 后者会让「这个包是不是 dev 版」
    // 与「这台服务端开不开门」这两件独立的事绑在一起。
    final blankOk = state.health?.allowsBlankLogin ?? false;
    if (!blankOk && (username.trim().isEmpty || password.isEmpty)) {
      state = state.copyWith(error: '请填写用户名和密码。');
      return;
    }
    final generation = ++_generation;
    state = state.copyWith(busy: true, error: null);

    final api = _probeApi(token: null);
    try {
      final tokens = await api.login(username.trim(), password);
      if (!_alive(generation)) return;
      // 先存再进主界面：反过来的话，存储失败会发生在用户已经"看起来登录了"
      // 之后，而他要到下次打开才发现自己没被记住
      if (kCanRememberToken) await rememberToken(tokens.refreshToken);
      if (!_alive(generation)) return;
      // 新的一次登录，不背上一段会话的续期旧账
      _clearRefreshBookkeeping();
      state = state.copyWith(
        phase: AuthPhase.ready,
        token: tokens.accessToken,
        refreshToken: tokens.refreshToken,
        busy: false,
        error: null,
        remember: true,
      );
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(busy: false, error: e.message);
    } finally {
      api.dispose();
    }
  }

  /// 注册一个新账号，成功即登录。
  ///
  /// 服务端的 `POST /auth/register` 直接换回一对令牌（注册即登录），
  /// 所以成功路径与 [signInWithPassword] 完全同形：存 refresh token、
  /// 清旧账、进主界面 —— 用户注册完不该再被要求「现在去登录」。
  ///
  /// 密码规则（≥12 字节）**不在这里复刻**：服务端的拒绝文案本来就是写给
  /// 人看的（「密码至少需要 12 个字节…」），照抄一份判据等于两处规则等着
  /// 漂移，而漂移的症状是「客户端说行、服务端说不行」各执一词。
  Future<void> signUpWithPassword(String username, String password) async {
    if (username.trim().isEmpty || password.isEmpty) {
      // 与登录同一条理由：静默 return 的症状是「点了没一点反应」
      state = state.copyWith(error: '请填写用户名和密码。');
      return;
    }
    final generation = ++_generation;
    state = state.copyWith(busy: true, error: null);

    final api = _probeApi(token: null);
    try {
      final tokens = await api.register(username.trim(), password);
      if (!_alive(generation)) return;
      // 先存再进主界面 —— 顺序的理由见 signInWithPassword
      if (kCanRememberToken) await rememberToken(tokens.refreshToken);
      if (!_alive(generation)) return;
      _clearRefreshBookkeeping();
      state = state.copyWith(
        phase: AuthPhase.ready,
        token: tokens.accessToken,
        refreshToken: tokens.refreshToken,
        busy: false,
        error: null,
        remember: true,
      );
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return;
      // 403（部署没开注册）的正文里写着管理员该怎么开 —— 原样给用户看。
      // 正常情况下界面根本不摆这个入口（open_registration 判过了），
      // 走到这里说明两边漂了，那句话正是线索
      state = state.copyWith(busy: false, error: e.message);
    } finally {
      api.dispose();
    }
  }

  /// 拿存下来的 refresh token 换一对新的 —— **启动时走这条**。
  ///
  /// 成功就直接进主界面，用户什么都不用做。失败分两种，见 [RefreshOutcome]：
  /// 服务端明确拒了（过期 30 天、被登出、判成重放）才清凭据 ——
  /// 留着一个已经作废的 token 只会让每次启动都白跑一次失败的请求；
  /// 服务端此刻答不上来（5xx / 限流 / 连不上）则**一个字节都不动**。
  Future<RefreshOutcome> restoreSession(String refreshToken) async {
    final generation = ++_generation;
    final api = _probeApi(token: null);
    try {
      // ── 换 token 这件事要拿跨标签页的锁，且锁内先重读存储。──
      //
      // refresh token 是一次性轮换的：同一枚出示两次，服务端按重放处理，
      // 整条 family 作废。Web 上凭据在 localStorage 里被所有标签页共享，
      // 而每个标签页内存里的那份可能已经被别的标签页轮换掉 —— 浏览器
      // 重启恢复两个标签页时，两边同时拿同一枚去续，后到的就是重放，
      // **两页一起被登出**。锁把并发续期排成队；锁内重读让排在后面的
      // 用前一个留下的后继，而不是自己手里那枚已作废的。
      //
      // 落盘也在锁内、成功后立即做：下一个等锁的标签页靠这份找到后继。
      final tokens = await withRefreshLock(() async {
        var candidate = refreshToken;
        if (kCanRememberToken) {
          // **读不出来不该拖垮这次续期。**
          //
          // 这一步是给 Web 多标签页兜底的（别的标签页可能已经把 token 轮换
          // 掉了，锁内重读才拿得到后继）。它失败的含义只是「没拿到更新的」，
          // 而手上这枚本来就是我们要用的那一枚 —— 直接往下走。
          //
          // 不裹的话，钥匙串锁着 / 平台通道缺失时异常会穿出整个
          // `restoreSession`，表现是**续期请求一次都没发出去**，
          // 而日志里只有一个与登录毫无关系的 MissingPluginException。
          try {
            final stored = await readRememberedToken();
            if (stored != null && stored != candidate) candidate = stored;
          } on Object catch (e) {
            debugPrint('锁内重读凭据失败（$e），用手上这枚续');
          }
        }
        final fresh = await api.refreshSession(candidate);
        if (kCanRememberToken) await rememberToken(fresh.refreshToken);
        return fresh;
      });
      if (!_alive(generation)) return RefreshOutcome.superseded;
      state = state.copyWith(
        phase: AuthPhase.ready,
        token: tokens.accessToken,
        refreshToken: tokens.refreshToken,
        busy: false,
        error: null,
        remember: true,
      );
      return RefreshOutcome.ok;
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return RefreshOutcome.superseded;
      // ⚠️ **续期失败是「用户被登出」这条链上唯一有服务端参与的一环，
      // 而它此前一个字都不说。** 上层几条路径共用差不多的红字，日志里分不出
      // 是手上没有 refresh token、被拒了、还是服务端暂时答不上来 ——
      // 三者的修法完全不同（分别是本地存储、服务端凭据、等着就好）。
      debugPrint(
        '续期没成：status=${e.statusCode} unreachable=${e.isUnreachable} '
        'msg=${e.message}',
      );
      // **只有 401/403 才算凭据失效。** 这里曾经写成「只要不是连不上就删」，
      // 于是 502（发版重启的那几秒）、429（自家限流）、501（打到一个没有
      // 账号功能的后端）都把 30 天的凭据删了 —— 一次服务端抖动就变成
      // 「你被登出了，而且下次启动也自动续不上」。用户报的「过一会儿突然
      // 退出」有一半就是它。
      final rejected = e.statusCode == 401 || e.statusCode == 403;
      if (rejected && kCanRememberToken) await forgetToken();
      if (rejected) return RefreshOutcome.rejected;
      // 429 单列：它不消耗 transient 预算（计账规则见 RefreshOutcome.throttled）
      if (e.statusCode == 429) return RefreshOutcome.throttled;
      return RefreshOutcome.transient;
    } on Object catch (e) {
      // 超时、序列化炸了、平台通道缺失 …… 都不是服务端对凭据的裁决。
      // 不接住的话异常会从 `onUnauthorized` 的微任务里穿出去成为 uncaught，
      // 而凭据的命运悬在半空 —— 按「暂时没成」处理，最坏也就是下次再试。
      if (_alive(generation)) debugPrint('续期没跑完（$e），按暂时失败处理');
      return RefreshOutcome.transient;
    } finally {
      api.dispose();
    }
  }

  /// Validates [rawToken] and, if the daemon accepts it, opens the app.
  ///
  /// ## Why `POST /auth/ticket` is the probe
  ///
  /// It is authenticated (so it answers the question), it is tiny, and it has
  /// no side effect beyond minting a credential that expires in a minute. The
  /// obvious alternative — `GET /sessions` — would work but pulls a page of
  /// real data purely to test a header, and it fails for reasons unrelated to
  /// authentication (an empty database, a storage hiccup), which would surface
  /// to the user as "wrong token".
  Future<void> signIn(String rawToken, {bool remember = false}) async {
    final token = rawToken.trim();
    if (token.isEmpty) {
      state = state.copyWith(error: '请填写 token。');
      return;
    }
    final generation = ++_generation;
    state = state.copyWith(busy: true, error: null);

    final probeApi = _probeApi(token: token);
    try {
      await probeApi.issueTicket();
      if (!_alive(generation)) return;

      if (remember && kCanRememberToken) {
        await rememberToken(token);
      } else if (!remember && kCanRememberToken) {
        await forgetToken();
      }
      if (!_alive(generation)) return;
      state = state.copyWith(
        phase: AuthPhase.ready,
        token: token,
        busy: false,
        error: null,
        remember: remember,
      );
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(
        // An unreachable daemon is not a rejected token. Conflating them puts
        // the user in a loop of retyping a credential that was never the
        // problem.
        phase: e.isUnauthorized ? AuthPhase.needsToken : AuthPhase.unreachable,
        busy: false,
        // 这句话以前专讲预共享 token（「确认复制的是 --generate-token 输出的
        // 明文」）。那条路已经从登录页上拿掉了，而这段代码还会在**手上那份
        // 存下来的凭据过期**时走到 —— 于是用户看到的是一段关于他从来没用过
        // 的东西的指路。改成说清「发生了什么、下一步做什么」
        error: e.isUnauthorized ? '存下来的登录凭据已经失效了（服务端不认）。重新登录一次即可。' : e.message,
        token: null,
      );
    } on Object catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(
        phase: AuthPhase.unreachable,
        busy: false,
        error: '$e',
      );
    } finally {
      probeApi.dispose();
    }
  }

  /// 同一时刻只允许**一次**续期在飞。
  ///
  /// # 不去重会把「掉一次线」变成「被登出」
  ///
  /// refresh token 是**一次性轮转**的：换一对新的，旧的立刻作废。服务端把
  /// 「拿一个已经轮转过的来换」判定为泄露，于是**整条 family 一起作废**
  /// （见 `credentials::judge_refresh`）。
  ///
  /// 而 401 是成批来的 —— 服务端一重启，页面上那七八个在飞的请求同时被拒。
  /// 每个都去刷的话，第一个成功、其余全部拿着刚作废的那份去换 ——
  /// 服务端看到的是一串重放，反手把这个人彻底登出。
  ///
  /// 所以：第一个发起，其余的**等它**。
  Future<RefreshOutcome>? _refreshInFlight;

  /// 连续多少次续期以 [RefreshOutcome.transient] 收场。成功清零。
  ///
  /// # 为什么 transient 也要有预算
  ///
  /// 「暂时失败不踢人」单独存在会造出一台永动机：服务端持续 5xx 时，
  /// 每条 401 都触发一次「续一下 → 没成 → 算了」，请求循环永远不收敛 ——
  /// 与本仓库那次「agent 被 1 秒一次杀了 730 次」同一个形状：
  /// **没有上限的自愈不是自愈**。三次都换不成就老实回登录页 ——
  /// 但凭据留着：服务端缓过来之后，下次启动还能拿它无感续上。
  int _transientRefreshes = 0;

  /// 连续 [RefreshOutcome.transient] 多少次后放弃、回登录页。
  static const int _maxTransientRefreshes = 3;

  /// 连续多少次续期被限流（429）。成功清零。
  ///
  /// # 为什么不并进 [_transientRefreshes]
  ///
  /// 服务端重启风暴时（AccessBook 在内存，重启后全体 access token 作废），
  /// 全体客户端同时涌向 /auth/refresh —— 5xx 与自家限流的 429 交替出现，
  /// 混在一个 3 次的预算里，一次发版就能把人送回登录页。429 的语义是
  /// 「等等再来」，比 5xx 更明确地说明凭据没毛病，所以预算给得更宽。
  ///
  /// # 为什么仍然有预算
  ///
  /// 「429 完全不计账」是台永动机：一个跑飞的续期循环每次都拿 429，永远
  /// 不收敛 —— 与「agent 被 1 秒一次杀了 730 次」同形状。上限之后照走
  /// [_GateReason.refreshStalled]（凭据留着，下次启动能续）。
  int _throttledRefreshes = 0;

  /// 连续被限流多少次后收口。8 次 ≈ 一分多钟的限流窗口滑过一轮还没好。
  static const int _maxThrottledRefreshes = 8;

  /// 连续多少次「白续」（续期成功、可新 token 没撑过 [_futileWindow] 又被
  /// 叫来续）。成功且**不白**时清零。
  ///
  /// # 为什么 transient 预算管不了这一族
  ///
  /// 有一族坏法是：refresh 路由一直答应（每轮 ok、transient 计数归零），
  /// 可换来的 access token 照样被每个请求 401 —— 服务端多实例验签密钥
  /// 不同步、时钟偏差都长这样。2765 对 refresh token 的那场生产风暴正是
  /// 它。cooloff 从「踢」改成「忽略」之后，这一族一度**失去了全部出口**：
  /// 不 rejected（refresh 没被拒）、不 stalled（没有 transient）——
  /// 界面停在 ready、请求全失败、每 3 秒签一对新 token，静默坏死。
  /// 这个计数器就是补回来的终态。
  int _futileRefreshes = 0;

  /// 一对新 token 至少该活多久。ok 之后不到这个时长又要续，
  /// 说明上一对根本没起作用 —— access 的正常寿命是 15 分钟。
  static const Duration _futileWindow = Duration(seconds: 60);

  /// 连续白续多少次后收口。3 次 = 三对没用的 token、约三分钟 ——
  /// 比老 cooloff 的「秒杀」温和，比无限循环有终点。
  static const int _maxFutileRefreshes = 3;

  /// 白续收口时，因「裁决探测通过」而重启本机 agent 的次数。
  ///
  /// # 为什么白续要先裁决、裁决要有预算
  ///
  /// 「续期一直成功、新 token 照样被拒」有**两个**病灶，修法相反：
  ///
  /// * 服务端多实例验签不同步 / 时钟偏差 —— 回登录页是对的（futile 熔断
  ///   防的正是这族，AccessBook 在内存，多副本时必然复现）。
  /// * **本机 agent 的转发链坏了** —— 请求全经它反代，它出的 401 与服务端
  ///   的长得一样。此时回登录页是误杀：直连服务端的续期明明一直成功。
  ///
  /// 收口前拿刚续到的新 token **直连**部署入口探一次就能分开两者：直连也
  /// 401 → 真是服务端不认，照旧收口；直连通 → 坏的是本机链路，重启 agent
  /// 而不是登出。重启要预算（[_maxFutileAgentKicks]）：重启治不好的链路
  /// 问题再杀多少次进程也没用，没有上限的自愈是永动机（agent 被 1 秒一次
  /// 杀了 730 次的教训）。预算用完后按原样收口回登录页。
  int _futileAgentKicks = 0;

  /// 白续裁决最多重启几次 agent。2 = 一次真重启 + 容忍重启窗口内的余震。
  static const int _maxFutileAgentKicks = 2;

  /// 测试拨表用。生产恒 [DateTime.now] —— cooloff / futile 的判据都是
  /// 秒级窗口，真等墙钟会把测试拖成分钟级。
  @visibleForTesting
  DateTime Function() clock = DateTime.now;

  /// 把续期的记账清干净 —— **换人、换后端、手动登录都要调。**
  ///
  /// 漏掉的后果（审查抓到的）：对后端 A 攒下的两次 transient 记到后端 B
  /// 头上，B 的第一次抖动就把人踢了；或者登出再登录的新会话，背着上一个
  /// 会话的旧账。
  void _clearRefreshBookkeeping() {
    _transientRefreshes = 0;
    _throttledRefreshes = 0;
    _futileRefreshes = 0;
    _futileAgentKicks = 0;
    _lastRefreshOk = null;
  }

  /// 上一次**续期成功**是什么时候。
  ///
  /// # 没有它就是一场续期风暴
  ///
  /// 续期成功之后客户端会重建、各面板重新拉一遍。如果那之后**立刻又 401**，
  /// 说明新 token 也不好使 —— 而再续一次只会重复同一个循环，每圈还签一对
  /// 新的 refresh token。
  ///
  /// 实测：加上「先续期」之后没有这道闸，五分钟里签了 **2765** 对，页面
  /// 疯狂闪烁（每次状态变化都重建整棵树）。
  ///
  /// 所以判据是：刚续过还 401 = 续期解决不了这个问题 = 回登录页。
  DateTime? _lastRefreshOk;

  /// 续期成功之后，多久之内再收到 401 就认定「续了也没用」。
  ///
  /// 3 秒：足够覆盖一次重建 + 各面板重新拉一轮，又短到不会把两次真正独立的
  /// 过期（相隔 15 分钟）误判成同一次。
  static const Duration _refreshCooloff = Duration(seconds: 3);

  /// 凭据在半路失效了 —— **先自己续，续不动才回登录页**。
  ///
  /// Called from any 401, from anywhere, via `HttpCortexApi.onUnauthorized`.
  ///
  /// # 为什么不是直接回登录页
  ///
  /// 那正是这段代码原本的样子，而它的后果是：access token 只活 15 分钟，
  /// 服务端每重启一次那本簿子就清空一次（它在内存里）—— 于是用户手上明明
  /// 揣着一张 **30 天**的 refresh token，却被反复弹回登录框。
  /// 生产上每发一次版，所有在线的人当场掉线。
  ///
  /// 续期成功之后**不做重试**：拿到 401 的那一次请求就让它失败。
  /// 在这一层重放请求要把 body、幂等性、流式那三样都想清楚，而收益只是省掉
  /// 一次「重试」的点击 —— 相比之下，「下次打开不用重新登录」才是那个真需求。
  /// 新 token 一到，`cortexApiProvider` 重建，各个面板自己会再拉一次。
  /// 返回值是**给测试用的**：让「续完了没有」可以被 await。
  ///
  /// 生产上的调用点（`HttpCortexApi.onUnauthorized`）把它丢掉 —— 那条路是从
  /// 请求失败里以微任务调起来的，挂在那儿等于把一次网络往返压进失败路径。
  Future<void> onUnauthorized() async {
    if (state.phase == AuthPhase.needsToken) return; // already there

    // **刚续过又 401 —— 这一条不算数。**
    //
    // 再续一次只会重复同一个循环，而每圈都签一对新的 refresh token
    // （见 `_lastRefreshOk` 上那段：没有这道闸，实测五分钟签了 2765 对）。
    // 但它也**不构成登出的理由**：新 token 生效后的头几秒里，重建窗口的
    // 迟到毛刺、负载均衡后面一台还没跟上的旧实例，都会掉出一两条 401。
    // 这里从前直接 `_fallBackToGate`，于是每 15 分钟（access 的寿命）就有
    // 一次被误杀的机会 —— 「开着不动过一会儿就被退出」的另一半来源。
    //
    // 真坏的凭据不会因此漏网：窗口一过，下一条 401 照常走续期，服务端
    // 拒绝它的话由 [_GateReason.refreshRejected] 收口。
    final last = _lastRefreshOk;
    if (last != null && clock().difference(last) < _refreshCooloff) {
      debugPrint('刚续期成功又收到 401 —— 当作重建窗口的迟到毛刺忽略，不踢');
      return;
    }

    final refresh = state.refreshToken;
    if (refresh == null) {
      _fallBackToGate(_GateReason.noRefreshToken);
      return;
    }
    // `??=` 是这条并发保护的全部：第一个发起，其余的拿到同一个 future。
    // 见字段上那段 —— 各刷各的会被服务端判成重放，把人彻底登出。
    //
    // ⚠️ **记账与踢人都在创建链里做，一次尝试只结一次账。**
    // 此前 switch 写在每个 awaiter 手里 —— 于是「服务端一重启，七八个
    // 在飞请求同时 401」这个常态下，同一次失败的续期被七个等待者各记
    // 一笔，「连续 3 次才踢」的预算在最典型的场景里实际是 1 次就踢：
    // 发版重启踢人换了句红字回来了。审查抓到的，测试没抓到 ——
    // 串行调用的测试里每次尝试恰好一个等待者，数出来永远是对的。
    final inflight = _refreshInFlight ??= _refreshAndSettle(
      refresh,
    ).whenComplete(() => _refreshInFlight = null);
    await inflight;
  }

  /// 发起一次续期并**结账**：计数、踢人、记时都只在这里发生。
  ///
  /// awaiter 无论有几个都只是 await —— 见 [onUnauthorized] 里那段。
  Future<RefreshOutcome> _refreshAndSettle(String refresh) async {
    // 「这次续期是不是白续」要在**发起前**判：ok 会刷新 _lastRefreshOk，
    // 结账时已经读不到上一轮的时刻了
    final lastOk = _lastRefreshOk;
    final futile = lastOk != null && clock().difference(lastOk) < _futileWindow;

    final outcome = await restoreSession(refresh);
    if (!ref.mounted) return outcome;
    switch (outcome) {
      case RefreshOutcome.ok:
        _lastRefreshOk = clock();
        _transientRefreshes = 0;
        _throttledRefreshes = 0;
        // 上一对 token 没活过 [_futileWindow] 就又被叫来续 —— 续是续上了，
        // 但显然没解决问题。连着几次就收口：refresh 路由永远答应的话，
        // rejected 与 stalled 都到不了，这里是这一族唯一的终态
        if (!futile) {
          _futileRefreshes = 0;
        } else if (++_futileRefreshes >= _maxFutileRefreshes) {
          _futileRefreshes = 0;
          // 收口前先裁决：坏的是服务端，还是本机 agent 的转发链。
          // 见 [_futileAgentKicks] —— 后者回登录页是误杀
          await _settleFutile();
        } else {
          debugPrint('续上了，但上一对 token 没起作用（第 $_futileRefreshes 次白续）');
        }
      case RefreshOutcome.rejected:
        _transientRefreshes = 0;
        _throttledRefreshes = 0;
        _futileRefreshes = 0;
        _fallBackToGate(_GateReason.refreshRejected);
      case RefreshOutcome.transient:
        // 服务端此刻答不上来（5xx / 网络）。凭据没毛病，这条 401
        // 对应的请求失败就失败 —— 等服务端缓过来，下一条 401 会再试。
        // 预算见 `_transientRefreshes`：不设上限的话这就是台永动机。
        if (++_transientRefreshes >= _maxTransientRefreshes) {
          _transientRefreshes = 0;
          _fallBackToGate(_GateReason.refreshStalled);
        } else {
          debugPrint('续期暂时没成（第 $_transientRefreshes 次）—— 凭据留着，这条 401 放过');
        }
      case RefreshOutcome.throttled:
        // 429 不消耗 transient 预算：发版重启风暴里它与 5xx 交替出现，
        // 混在同一个 3 次预算里会让一次不算长的抖动就把人送回登录页。
        // 自己的（更宽的）预算兜住「永远 429」那台永动机。
        if (++_throttledRefreshes >= _maxThrottledRefreshes) {
          _throttledRefreshes = 0;
          _fallBackToGate(_GateReason.refreshStalled);
        } else {
          debugPrint('续期被限流（第 $_throttledRefreshes 次 429）—— 凭据留着，不计入抖动预算');
        }
      case RefreshOutcome.superseded:
        // 被换地址 / 手动登录 / _reset 取代。与服务端健康无关，
        // 不记任何账 —— 记了就是让一次用户操作替服务端背黑锅
        break;
    }
    return outcome;
  }

  /// 白续收口前的**裁决探测**：分清「服务端不认」与「本机转发链坏了」。
  ///
  /// 拿刚续到的新 access token **直连**部署入口打一次 `POST /auth/ticket`
  /// （与 [signIn] 用的同一个最便宜的认证探针；`authProbeApiProvider`
  /// 从不经过本机 agent）：
  ///
  /// * 探测通过 → 服务端认这把新凭据，坏的是本机 agent 那条反代链 ——
  ///   重启 agent（带预算），**不登出**。此前这一族被一律判成
  ///   「服务端配置问题」踢回登录页，正是桌面端「每 15 分钟被踢」的收口点。
  /// * 探测 401/403 → 真是服务端不认（多实例验签不同步那族），照旧收口。
  /// * 探测连不上 / 5xx → 判不出来。照旧收口 —— 保留熔断，最坏也只是
  ///   多登录一次；不收口的话这族回到静默坏死（每 3 秒签一对 token）。
  ///
  /// # 为什么不是删掉 futile 熔断
  ///
  /// 它防的「多实例验签不同步」真实存在（AccessBook 在内存，将来多副本
  /// 必然复现）。裁决只是把「误杀桌面用户」从那族里分出去，代价是收口前
  /// 多一个请求。
  Future<void> _settleFutile() async {
    // 经网关而不是直接 read `localAgentOriginProvider`：那个 provider watch
    // 着本控制器，直接 read 是依赖图上的环（riverpod 当场抛
    // CircularDependencyError）。见 `localAgentGatewayProvider`
    final agent = ref.read(localAgentGatewayProvider);
    final viaAgent = agent.origin != null;
    final freshToken = state.token;
    // 没经过 agent 的部署（Web、无 agent 构建）没有第二个嫌疑人，直接收口
    if (!viaAgent ||
        freshToken == null ||
        _futileAgentKicks >= _maxFutileAgentKicks) {
      _fallBackToGate(_GateReason.refreshFutile);
      return;
    }
    final api = _probeApi(token: freshToken);
    try {
      await api.issueTicket();
      // 直连认这把凭据 —— 病灶在本机链路。重启 agent，登录态不动
      _futileAgentKicks++;
      logAuthGate(
        reason: 'futile-agent-restart',
        detail:
            '白续 $_maxFutileRefreshes 次但直连裁决通过 —— '
            '重启本机 agent（第 $_futileAgentKicks/$_maxFutileAgentKicks 次），不登出',
        wasReady: state.phase == AuthPhase.ready,
      );
      debugPrint('白续裁决：直连认新凭据，坏的是本机 agent 链路 —— 重启它，不登出');
      agent.restart();
    } on CortexApiException catch (e) {
      debugPrint('白续裁决：直连探测也过不去（status=${e.statusCode}）—— 按服务端问题收口');
      _fallBackToGate(_GateReason.refreshFutile);
    } on Object catch (e) {
      debugPrint('白续裁决没跑完（$e）—— 判不出来，按原样收口');
      _fallBackToGate(_GateReason.refreshFutile);
    } finally {
      api.dispose();
    }
  }

  /// 回登录页。**只有从「正在用」掉下来才值得一句红字。**
  ///
  /// # 启动阶段的 401 是清场，不是事故
  ///
  /// 还没登录时就有 401 打进来是常态（过期凭据、或后台某个还没被门挡住的
  /// 请求）。那个 401 的意思只是「请登录」，而登录页本身已经在说这句话了
  /// —— 再压一条「凭据已失效或被拒绝（HTTP 401）」上去，用户读到的是
  /// 「我还没输密码就被拒绝了」，然后把一次正常的登录当成失败来排查。
  /// 实际发生过，且反复发生。
  ///
  /// # 这段代码曾经「被修复过」但没有
  ///
  /// 上一次改它用的是脚本替换，锚点少了函数体里一段注释，没匹配上，而
  /// 那次替换**没有 assert** —— 静默落空，此后每个人（包括改它的人）都
  /// 以为它已经是新的了。配套测试也是绿的，因为那条测试走的路径根本不经过
  /// 这里。教训写在这儿：**锚不上要响，测试要真的踩到被改的行。**
  void _fallBackToGate(_GateReason reason) {
    if (state.phase == AuthPhase.needsToken) return;
    final wasReady = state.phase == AuthPhase.ready;
    // ⚠️ **红字接近一样，日志必须分得清。**
    //
    // 这几条路径的病因与修法毫不相干（本地存储 / 服务端凭据 / 服务端抖动），
    // 而用户看到的只有一句短话。2026-08-23 用户报「开着不动几十分钟就掉」
    // 时，光看代码定位不出是哪一条 —— 服务端配置（access 15 分钟、refresh
    // 30 天且每次续期）是对的，生产日志里也没有任何一条 refresh 被拒的记录。
    if (wasReady) debugPrint('回登录页：${reason.explain}');
    // **每条掉回登录页的路径都落盘，带 reason code。** debugPrint 跟着
    // 进程死，而「开着不动几十分钟就掉」这类报障到手时进程早换了几代 ——
    // 桌面端写进 agent-launch.jsonl（跨进程活着），Web 上是空实现
    logAuthGate(
      reason: reason.name,
      detail: reason.explain,
      wasReady: wasReady,
    );
    // Deliberately not `forgetToken()`: a rotated server-side secret is not a
    // reason to also destroy the copy the user may still be editing, and on
    // desktop the "stored" copy is an environment variable this app must not
    // pretend it can unset.
    state = state.copyWith(
      phase: AuthPhase.needsToken,
      token: null,
      // stalled 那条**不许说「已过期」**：凭据好好的，是服务端此刻答不上
      // 来。说过期会让用户以为自己的登录坏了，而他该做的只是等一等
      error: !wasReady
          ? null
          : reason == _GateReason.refreshStalled
          ? '续期一直没有成功（服务端暂时答不上来）。稍等片刻再试；反复出现的话，检查部署入口地址。'
          : reason == _GateReason.refreshFutile
          ? '续期成功但新凭据仍不被接受 —— 服务端可能有配置问题。请稍后重新登录。'
          : '登录已过期，续期也没有成功。请重新登录。',
    );
  }

  /// Explicit sign-out. Drops the in-memory credential and any stored copy.
  Future<void> signOut() async {
    // **先作废在飞的续期。** 不作废的话：登出时恰有一次续期在飞，它成功
    // 回来发现代次没变，把 state 写回 ready + 新 token —— 用户明确点了
    // 登出却被「登回去」，锁内还把新凭据写回了系统凭据库
    ++_generation;
    _clearRefreshBookkeeping();
    // **先告诉服务端作废，再删本地那份。**
    //
    // 只删本地只是「这台机器忘了」——已经泄露出去的那一份照样能用到 30 天后。
    // 顺序也不能反：先删本地就没有凭据去请求作废了。
    final refresh = state.refreshToken;
    if (refresh != null) {
      final api = _probeApi(token: null);
      try {
        await api.logout(refresh);
      } finally {
        api.dispose();
      }
    }
    if (kCanRememberToken) await forgetToken();
    if (!ref.mounted) return;
    state = state.copyWith(
      phase: AuthPhase.needsToken,
      token: null,
      refreshToken: null,
      error: null,
      remember: false,
    );
  }

  /// 登录页本地校验的错误（如「两次输入的密码不一致」）。
  ///
  /// 走 `state.error` 而不是页面自己的局部状态：那一屏的错误框只有一个，
  /// 服务端的拒绝与本地校验各画一份的话，两条同时出现时会叠两个红框。
  void reportFormError(String message) {
    state = state.copyWith(error: message);
  }

  void setRemember(bool value) {
    if (state.remember == value) return;
    state = state.copyWith(remember: value);
  }

  void _reset() {
    ++_generation;
    // 换了后端就是换了账本 —— 对 A 攒的 transient 不许记到 B 头上
    _clearRefreshBookkeeping();
    state = _seeded();
    Future.microtask(probe);
  }

  bool _alive(int generation) => ref.mounted && generation == _generation;

  CortexApi _probeApi({String? token}) => ref.read(authProbeApiProvider)(token);
}

/// The credential the platform can hand over with no user action.
///
/// Desktop reads `CORTEXD_TOKEN` from the environment; Web reads the copy the
/// user opted into keeping for the tab. A provider rather than a direct call so
/// that the tests can pin it — otherwise every case about the login screen
/// would quietly change behaviour on a developer machine that has the variable
/// set, which is precisely the machine we tell people to have.
final authSeedTokenProvider = Provider<String?>((ref) => readSeedToken());

/// 上一次登录留下的 refresh token（系统凭据库 / sessionStorage）。
///
/// 做成 provider 而不是直接调 `readRememberedToken()`：那一步要跨进程问
/// 钥匙串，在测试里既跑不动也不该跑。而这条路径**曾经整个不存在**
/// （存了不读），所以它尤其需要能被测到。
final rememberedTokenProvider = FutureProvider<String?>((ref) async {
  // **必须有超时。** 读凭据库是一次跨进程调用：钥匙串锁着、libsecret 没跑、
  // 或者 D-Bus 卡住时，它可以**永远不返回** —— 而这一步挡在启动路径上，
  // 表现是应用停在一片空白上，没有任何提示，也没有任何出路。
  //
  // 超时之后按「没有记住的凭据」处理：最坏的结果是让人重登一次，
  // 而那远好过打不开。
  try {
    return await readRememberedToken().timeout(const Duration(seconds: 3));
  } on Object catch (e) {
    debugPrint('读不出记住的登录状态（$e），这次需要重新登录');
    return null;
  }
});

/// Builds the throwaway client the gate probes with.
///
/// A separate provider from [cortexApiProvider] for two reasons, one structural
/// and one practical:
///
/// * **Structural** — that provider's client is built *from* the credential
///   this controller is trying to validate. Using it here would make the gate
///   depend on the thing it gates, and every rejected sign-in attempt would
///   tear down and rebuild the entire data layer.
/// * **Practical** — it is the seam the tests substitute, which is what lets
///   the gate's state machine be exercised without a socket.
///
/// The instances it returns carry no `onUnauthorized` hook: a 401 *here* is the
/// answer to the question being asked, not an event to broadcast, and feeding
/// it back into [AuthController.onUnauthorized] would overwrite the specific
/// message [AuthController.signIn] is about to write with a generic one.
final authProbeApiProvider = Provider<CortexApi Function(String? token)>((ref) {
  final baseUrl = ref.watch(appConfigProvider).baseUrl;
  return (token) => HttpCortexApi(baseUrl: baseUrl, token: token);
});

final authControllerProvider = NotifierProvider<AuthController, AuthState>(
  AuthController.new,
);

/// 这个部署开着注册吗 —— 登录页据此决定**摆不摆**「注册」入口。
///
/// # 判据只算一处
///
/// 服务端侧 `/auth/register` 的 403 与 health 的 `open_registration` 已经是
/// 同一个函数；客户端侧所有想知道「开没开」的地方都必须走这个 provider ——
/// 界面与提交逻辑各判一次的话，漏改一处不会有任何测试红。
///
/// # 为什么可能要问两条路
///
/// `/health` 是登录页本来就在探的那条，dev 上它就是 agentd。但**生产边缘把
/// `/api/health` 分给了记忆服务**（deploy compose 里那条 `!Path` 规则），
/// 那份响应里没有这个字段（解析成 null）—— 此时补问 `/sandbox/health`：
/// agentd 的同一个 handler 挂在两条路径上，生产上那条才是它在答。
///
/// 任何一步答不出来都按**关闭**处理：藏一个开着的入口（用户少一条捷径，
/// 管理员还有 `--create-user`），好过摆一个必然 403 的入口（约束 2）。
final openRegistrationProvider = FutureProvider<bool>((ref) async {
  final health = ref.watch(
    authControllerProvider.select((s) => s.health?.openRegistration),
  );
  if (health != null) return health;
  // health 还没探到、或者答话的那份没带字段。前者等下一次重算（watch 会
  // 触发）；两种都值得去 /sandbox/health 补问一次 —— 探测失败按关闭处理
  final api = ref.read(authProbeApiProvider)(null);
  try {
    final sandbox = await api.sandboxHealth();
    return sandbox.openRegistration ?? false;
  } on Object {
    return false;
  } finally {
    api.dispose();
  }
});
