import 'json.dart';

/// 一家搜索服务商，以及它**有哪些能力**。
///
/// # 为什么能力要跟着服务商一起下发
///
/// 抄 Cherry Studio 那个抽象里最有价值的一点：不是每家都又能搜又能抓
/// （博查只做搜索）。界面据此决定它出现在哪个下拉框里 —— 把只做搜索的
/// 摆进「URL 获取服务商」里，用户选完之后每次抓取都失败，而原因藏在一个
/// 404 里（CLAUDE.md 约束 2）。
///
/// 这份清单**由服务端给**，不在客户端写死：写死的话，服务端加了一家而
/// 老客户端看不见（还算好），或者客户端列了一家而服务端不认（用户选了
/// 之后每次搜索都 400，且说不清为什么）。
class SearchProviderInfo {
  const SearchProviderInfo({
    required this.id,
    required this.name,
    required this.defaultBase,
    required this.canFetch,
  });

  factory SearchProviderInfo.fromJson(Map<String, dynamic> json) =>
      SearchProviderInfo(
        id: asString(json['id']),
        name: asString(json['name']),
        defaultBase: asString(json['default_base']),
        canFetch: json['can_fetch'] == true,
      );

  final String id;
  final String name;

  /// 官方地址。用作「API 地址」那个框的占位符 —— 用户据此知道留空会打哪儿。
  final String defaultBase;

  /// 抓得了网页正文吗。
  final bool canFetch;
}

/// 联网检索的配置。
class SearchPrefs {
  const SearchPrefs({
    this.provider = '',
    this.keyTail = '',
    this.baseUrl,
    this.maxResults = 5,
    this.depth = 'basic',
    this.cutoffLimit = 2000,
    this.excludeDomains = const [],
    this.deploymentKey = false,
    this.providers = const [],
  });

  factory SearchPrefs.fromJson(Map<String, dynamic> json) => SearchPrefs(
    provider: asString(json['provider']),
    keyTail: asString(json['key_tail']),
    baseUrl: json['base_url'] as String?,
    maxResults: asIntOrNull(json['max_results']) ?? 5,
    depth: asString(json['depth']).isEmpty ? 'basic' : asString(json['depth']),
    cutoffLimit: asIntOrNull(json['cutoff_limit']) ?? 2000,
    excludeDomains: [
      for (final d in (json['exclude_domains'] as List? ?? const []))
        asString(d),
    ],
    deploymentKey: json['deployment_key'] == true,
    providers: [
      for (final p in (json['providers'] as List? ?? const []))
        SearchProviderInfo.fromJson(p as Map<String, dynamic>),
    ],
  );

  /// 空串 = 用部署提供的那一份。
  final String provider;

  /// 已存那把 key 的后四位。空 = 没填过。**永远拿不到明文。**
  final String keyTail;

  final String? baseUrl;
  final int maxResults;

  /// `basic` / `advanced`。
  final String depth;

  /// 每条结果截到多长。0 = 不截。
  final int cutoffLimit;

  final List<String> excludeDomains;

  /// 服务端 `.env` 里有没有 key —— 界面据此决定要不要说「不填也能用」。
  final bool deploymentKey;

  final List<SearchProviderInfo> providers;

  /// 抓得了正文的那几家。「URL 获取服务商」那个下拉框只列它们。
  List<SearchProviderInfo> get fetchers =>
      providers.where((p) => p.canFetch).toList();

  /// 按 id 找一家。找不到回 `null`。
  ///
  /// ⚠️ **界面上问「我选的这家能干什么」时，一律走它，不要走 [current]。**
  /// [current] 认的是**已保存**的那个 id，而设置页上还有一个**草稿** id
  /// （用户刚在下拉框里选的、还没点保存）。两者不一致时 [current] 回 `null`，
  /// 于是调用方回落到「不知道」—— 2026-08-27 的现场是：下拉框选到 Exa，
  /// 下面那行写着「⚠️ exa 只做搜索，不抓正文……要抓正文的话，换成：Tavily、Exa」，
  /// 同一句话既说它不行又叫你换成它。
  SearchProviderInfo? byId(String id) {
    for (final p in providers) {
      if (p.id == id) return p;
    }
    return null;
  }

  /// 当前**已保存**的那家（`null` = 用部署提供的，或者认不出这个 id）。
  SearchProviderInfo? get current => byId(provider);

  /// 这一刻到底能不能搜。
  ///
  /// ⚠️ 判据有两半，缺一半就会画出一个骗人的界面：选了一家但没填 key 时
  /// **不会**回落到部署那把（服务端 `SearchPrefs::resolve` 那段），
  /// 所以「选了一家」并不等于「能用」。
  bool get usable => provider.isEmpty ? deploymentKey : keyTail.isNotEmpty;
}
