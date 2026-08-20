/// Runtime configuration.
///
/// Compile-time defaults come from `--dart-define`; the settings sheet can
/// override them for the running session (handy for pointing a Web build at a
/// different daemon without rebuilding).
class AppConfig {
  const AppConfig({
    required this.useMock,
    required this.baseUrl,
    this.offline = false,
    this.knownBaseUrls = const [],
  });

  /// When true, [MockCortexApi] is used and no network call is ever made.
  final bool useMock;

  /// `cortexd` origin, e.g. `http://127.0.0.1:8080`.
  final String baseUrl;

  /// 这台机器上**用过的**部署入口，最近用的在前，含当前这个。
  ///
  /// # 为什么要记
  ///
  /// 地址收进一行小字之后，它变成了一个纯文本框 —— 而一个在公司部署和
  /// 本机 dev 之间来回切的人，每切一次都要**凭记忆重打一串 URL**。
  /// 2026-08-21 实测到的症状是「进到这里就没办法切回去了」：
  /// 界面上没有任何一处还记得另一个地址长什么样。
  ///
  /// 只记**显式设过的**，编译期默认值不进这份清单：那个值
  /// （`127.0.0.1:8080`）没有人真的选过它，摆出来只是噪音。
  final List<String> knownBaseUrls;

  /// 离线模式：**没有 cortexd**，只有本地 agent。
  ///
  /// # 它与 [useMock] 的区别
  ///
  /// mock 是**假数据**——为了在没有后端时看界面。离线模式里一切都是真的：
  /// 真的模型、真的工具、真的读写你本机的文件。唯一缺的是记忆
  /// （没有 cortexd 就没有记忆库），而这一轮轮对话会排进本地队列，
  /// 以后接上服务器时自动灌回去。
  ///
  /// # 为什么这个模式值得存在
  ///
  /// 「装了就能用」与「记忆是核心」这两件事有张力，而把它藏起来解决不了：
  /// 一个连不上服务器的人现在停在登录界面，什么也做不了。给他一条明说
  /// 「没有记忆」的路，比让他对着一个连不上的地址反复重试要诚实。
  final bool offline;

  /// `--dart-define=USE_MOCK=true|false`
  ///
  /// Defaults to **false**: `cortexd` is running locally, so the honest default
  /// is to talk to it. CI and offline demos pass `USE_MOCK=true`.
  static const bool defaultUseMock = bool.fromEnvironment('USE_MOCK');

  /// `--dart-define=CORTEX_BASE_URL=http://host:port`
  ///
  /// # ⚠️ 这个默认值只够「纯自托管记忆服务」用
  ///
  /// `127.0.0.1:8080` 是**记忆服务本身**。填它的话会话、历史、实时同步全都
  /// 正常，**只有云端对话不通** —— 拆成 cortexd + agentd 之后 `/chat` 归
  /// agentd，而知道该转给谁的只有边缘（dev 的 nginx / prod 的 traefik）。
  ///
  /// 那种形态下（本机跑一个 cortexd、没有 agentd）这个默认值是对的：所有
  /// 对话本来就跑在本机 agent 上，不需要云端那条路。但只要环境里有 agentd，
  /// 地址就得指向入口 —— dev 是 `http://127.0.0.1:5173`。
  ///
  /// **Web 构建不受影响**：它由 nginx 同源发出，`CORTEX_BASE_URL` 传空串，
  /// 于是一切走根路径、天然经过边缘。
  static const String defaultBaseUrl = String.fromEnvironment(
    'CORTEX_BASE_URL',
    defaultValue: 'http://127.0.0.1:8080',
  );

  /// 这份构建自己的版本号，由 `scripts/release-desktop-windows.sh` 传入。
  ///
  /// # 默认是空串，而空串**关掉整个更新功能**
  ///
  /// 没传的构建（`flutter run`、`flutter test`、任何人的本地编译）不知道自己
  /// 是哪一版，那它就没有资格判断「有没有新版本」。
  ///
  /// 拿不到时给一个 `0.0.0` 之类的兜底是错的：那样每个开发构建都会看到
  /// 「有新版本」，而点一下就把自己手上的开发版**换成了正式版**——
  /// 一次不可撤销的、由一个占位默认值引发的覆盖。
  ///
  /// 这正是本仓库数到第 6 次的「空串顶掉默认值」形状，只是方向反过来：
  /// 那几次是空串被当成「配过了」，这次是空串必须被当成「没配」。
  /// 所以判的是 [updatesSupported] 里的 `isNotEmpty`，不是 `!= null` ——
  /// `String.fromEnvironment` 永远不返回 null。
  static const String appVersion = String.fromEnvironment('CORTEX_APP_VERSION');

  /// 这份构建能不能谈论「更新」。
  static bool get updatesSupported => appVersion.trim().isNotEmpty;

  /// `--dart-define=CORTEX_UPDATE_FEED=https://…`
  ///
  /// 覆盖版本来源。留着两个用处：自建/内网分发，以及**真机验证不必真发一版**
  /// （指向本地起的一个假 feed 就能把整条下载—校验—安装走通）。
  static const String updateFeed = String.fromEnvironment(
    'CORTEX_UPDATE_FEED',
    defaultValue: 'https://api.github.com/repos/weironz/cortex/releases/latest',
  );

  static const AppConfig initial = AppConfig(
    useMock: defaultUseMock,
    baseUrl: defaultBaseUrl,
  );

  AppConfig copyWith({
    bool? useMock,
    String? baseUrl,
    bool? offline,
    List<String>? knownBaseUrls,
  }) => AppConfig(
    useMock: useMock ?? this.useMock,
    baseUrl: baseUrl ?? this.baseUrl,
    offline: offline ?? this.offline,
    knownBaseUrls: knownBaseUrls ?? this.knownBaseUrls,
  );

  /// 把 [url] 记成「最近用过」。返回新的清单，最近的在前、去重、封顶。
  ///
  /// 纯函数：前插去重这件事看着显然，实际有三个坑（重复项、大小写与尾斜杠
  /// 不一致、无上限增长），而它们全都只在用了几个月之后才显形。
  static List<String> remember(List<String> known, String url, {int cap = 6}) {
    final u = url.trim();
    if (u.isEmpty) return known;
    // 只按**原样**去重，不做 URL 归一化：`https://x/api` 与 `https://x/api/`
    // 在服务端可能真的是两条路，替用户合并等于替他改配置
    final next = [u, ...known.where((k) => k != u)];
    return next.length > cap ? next.sublist(0, cap) : next;
  }

  @override
  bool operator ==(Object other) =>
      other is AppConfig &&
      other.useMock == useMock &&
      other.baseUrl == baseUrl &&
      other.offline == offline &&
      _sameList(other.knownBaseUrls, knownBaseUrls);

  static bool _sameList(List<String> a, List<String> b) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (a[i] != b[i]) return false;
    }
    return true;
  }

  @override
  int get hashCode =>
      Object.hash(useMock, baseUrl, offline, Object.hashAll(knownBaseUrls));
}
