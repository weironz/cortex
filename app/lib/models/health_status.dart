import 'json.dart';

/// `GET /health` → `{"status":"ok","version":"0.0.1","database":"ok"}`
class HealthStatus {
  const HealthStatus({
    required this.status,
    required this.version,
    required this.database,
    this.auth = authUnknown,
    this.devLogin,
    this.role = roleCortexd,
    this.server,
  });

  final String status;
  final String version;
  final String database;

  /// 答话的是谁：`cortexd` / `local-agent`。
  ///
  /// # 为什么客户端必须认这一位
  ///
  /// 本地 agent 在跑时，桌面端的**每一个**请求（含 `/health`）都打给它 ——
  /// 于是「后端状态」那几行显示的其实是 **agent 自己**的健康，
  /// 而界面把它标成了部署的。用户看到的 `database unknown`、`auth unknown`
  /// 正是 agent 的答法（它没有数据库），却读起来像「我的服务端坏了」。
  ///
  /// 老服务端不报这一位，按 `cortexd` 处理 —— 那时本地 agent 还不存在。
  final String role;

  /// agent 上游那台的状态。`null` = 答话的不是 agent（或它没报）。
  final ServerLink? server;

  /// 答话的是本地 agent 吗。
  bool get isLocalAgent => role == roleLocalAgent;

  static const String roleCortexd = 'cortexd';
  static const String roleLocalAgent = 'local-agent';

  /// `"token"` / `"disabled"`, or [authUnknown] against a daemon that predates
  /// the field.
  ///
  /// This is the one thing on `/health` the client acts on rather than merely
  /// displays: it is what lets the login gate avoid demanding a token from a
  /// deployment that does not check one. `/health` is the *only* unauthenticated
  /// route, which is precisely why the answer has to live here — anywhere else
  /// and asking the question would require already being past the gate.
  final String auth;

  /// 这台服务端开着**免密登录**吗，开着的话登进去是谁。
  ///
  /// 与 [auth] 同类：客户端要据它决定行为，而不是自己猜。空登录框点「登录」
  /// 之所以能通，是因为服务端指名了登成谁（`CORTEX_DEV_LOGIN`）——
  /// 客户端无从得知那个配置，只能问。
  ///
  /// 报的是**用户名**而不是布尔：登录页据此能说清「会登成 admin」，
  /// 而一个 `true` 只能说「大概能进」。
  final String? devLogin;

  /// 空着用户名密码点「登录」会不会成功。
  bool get allowsBlankLogin => (devLogin ?? '').isNotEmpty;

  /// A daemon too old to report the field. Treated as "assume a token is
  /// needed": guessing `disabled` would send the user into a UI that then 401s
  /// on every call with no way back to a token prompt.
  static const String authUnknown = 'unknown';

  /// The daemon checks credentials, so the client must hold one.
  bool get requiresToken => auth != 'disabled';

  /// The daemon accepts anyone who can reach the port.
  ///
  /// Surfaced in the UI rather than silently enjoyed: `cortexd` only reaches
  /// this state when someone wrote `CORTEX_AUTH=disabled` on purpose, and the
  /// daemon itself logs a warning every time it starts. A client that showed
  /// nothing would be the only place in the system that failed to mention it.
  bool get authDisabled => auth == 'disabled';

  /// Only the process itself has to be up for the client to be usable.
  ///
  /// Deliberately does **not** require `database == 'ok'`: during M2/M5
  /// development the daemon legitimately reports `not_wired`, and the chat and
  /// memory routes still answer. Storage state is surfaced separately by
  /// [databaseNote] rather than turning the whole UI red.
  bool get isHealthy => status == 'ok';

  /// Null when storage is fully wired; otherwise a short note for the status
  /// strip.
  String? get databaseNote => switch (database) {
    'ok' => null,
    'not_wired' => '存储层未接线',
    final other => '存储: $other',
  };

  factory HealthStatus.fromJson(Map<String, dynamic> json) => HealthStatus(
    status: asString(json['status'], 'unknown'),
    version: asString(json['version'], '?'),
    database: asString(json['database'], 'unknown'),
    auth: asString(json['auth'], authUnknown),
    devLogin: json['dev_login'] as String?,
    role: asString(json['role'], roleCortexd),
    server: json['server'] is Map<String, dynamic>
        ? ServerLink.fromJson(json['server'] as Map<String, dynamic>)
        : null,
  );
}

/// 本地 agent 报的「我上游那台怎么样」。
///
/// 只有 `role == local-agent` 时才有 —— 部署本身没有上游。
class ServerLink {
  const ServerLink({
    required this.remote,
    required this.reachable,
    this.backlog = 0,
  });

  factory ServerLink.fromJson(Map<String, dynamic> json) => ServerLink(
    remote: asString(json['remote']),
    reachable: json['reachable'] == true,
    backlog: asIntOrNull(json['backlog']) ?? 0,
  );

  /// agent 的 `--remote`，也就是**用户配的那个部署**。
  final String remote;

  /// 刚才那次探活通没通。
  final bool reachable;

  /// 离线队列里还压着多少条没刷出去。
  final int backlog;
}
