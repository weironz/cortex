/// 自带 API key 的状态。**永远不含明文** —— 服务端只回后 4 位。
///
/// 回明文会让这把 key 出现在浏览器缓存、日志、以及任何一次「把请求粘给
/// 别人看」里。要换 key 时重新填一遍即可。
class LlmKeyStatus {
  const LlmKeyStatus({
    required this.configured,
    required this.supported,
    this.provider,
    this.keyTail,
    this.baseUrl,
    this.updatedAt,
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
  );

  static const empty = LlmKeyStatus(configured: false, supported: false);
}
