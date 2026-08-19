/// 自带 API key 的状态。**永远不含明文** —— 服务端只回后 4 位。
///
/// 回明文会让这把 key 出现在浏览器缓存、日志、以及任何一次「把请求粘给
/// 别人看」里。要换 key 时重新填一遍即可。
/// 下拉里的一家供应商（`GET /settings/llm-key` 的 `providers`）。
class ProviderChoice {
  const ProviderChoice({
    required this.id,
    required this.displayName,
    this.description = '',
    this.baseUrl = '',
    this.requiresAuth = true,
  });

  factory ProviderChoice.fromJson(Map<String, dynamic> json) => ProviderChoice(
    id: json['id'] as String? ?? '',
    displayName: json['display_name'] as String? ?? '',
    description: json['description'] as String? ?? '',
    baseUrl: json['base_url'] as String? ?? '',
    requiresAuth: json['requires_auth'] as bool? ?? true,
  );

  /// 填进请求里的那个 id，如 `deepseek`。
  final String id;
  final String displayName;
  final String description;

  /// 官方端点。界面拿它当「留空 = 连到这里」的提示 ——
  /// 一个空的端点输入框说不出留空会去哪。
  final String baseUrl;

  /// 要不要 key。Ollama 这类是 false，界面据此不逼用户填一把他没有的密钥。
  final bool requiresAuth;
}

class LlmKeyStatus {
  const LlmKeyStatus({
    required this.configured,
    required this.supported,
    this.provider,
    this.keyTail,
    this.baseUrl,
    this.updatedAt,
    this.providers = const [],
  });

  /// 填过而且还没撤下
  final bool configured;

  /// 这个部署**能不能**存自带 key。
  ///
  /// 服务端没配主密钥时是 false —— 那种情况下它拒绝保存而不是明文存下去，
  /// 界面要如实说，别让人对着一个存不进去的输入框较劲。
  final bool supported;

  final String? provider;

  /// 明文 key 的后 4 位，用来认出「填的是哪一把」
  final String? keyTail;

  /// 自建端点。`null` = 走供应商官方的那个。
  ///
  /// 一个人「自己的 key」很少是官方那把 —— 更常见的是公司网关、
  /// one-api / LiteLLM 中转、某个更便宜的兼容服务。
  final String? baseUrl;

  final DateTime? updatedAt;

  /// 能填哪几家。**空 = 老服务端没有这个字段**，那时只能退回手打输入框。
  final List<ProviderChoice> providers;

  factory LlmKeyStatus.fromJson(Map<String, dynamic> json) => LlmKeyStatus(
    configured: json['configured'] as bool? ?? false,
    supported: json['supported'] as bool? ?? false,
    provider: json['provider'] as String?,
    keyTail: json['key_tail'] as String?,
    baseUrl: json['base_url'] as String?,
    updatedAt: switch (json['updated_at']) {
      final String s => DateTime.tryParse(s),
      _ => null,
    },
    providers: switch (json['providers']) {
      final List<dynamic> l =>
        l
            .whereType<Map<String, dynamic>>()
            .map(ProviderChoice.fromJson)
            .where((p) => p.id.isNotEmpty)
            .toList(growable: false),
      _ => const [],
    },
  );

  static const empty = LlmKeyStatus(configured: false, supported: false);
}
