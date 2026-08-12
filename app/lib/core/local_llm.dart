/// 本机 LLM 配置 —— **离线模式专用**。
///
/// # 它与设置里那个「自己的 API key」是两件事
///
/// | | 存在哪 | 什么时候用 | 跟着谁走 |
/// |---|---|---|---|
/// | 自己的 API key | **cortexd**（你自己的 schema，加密） | 连着服务器时 | 账号，多设备共享 |
/// | 本机 LLM（这个） | **这台机器**的系统凭据库 | 离线模式 | 这台机器，不上传 |
///
/// 两者不能互相替代，而且必须让人分得清 —— 否则会出现「我明明填过 key 了，
/// 怎么离线还是用不了」，而那时他找不到任何线索。
///
/// # 为什么离线模式非要这个不可
///
/// 连着服务器时本地 agent 走 `proxy`：模型调用转给 cortexd，key 只在服务端
/// 一处。离线模式里没有 cortexd —— 代理无处可代，agent 必须自己直连，
/// 于是它需要知道**打给谁、用什么 key**。
///
/// 在此之前这只能靠启动桌面端**之前**设好环境变量（本地 agent 继承父进程
/// 环境）。对一个双击图标的人来说等于没有。
library;

/// 一份本机模型配置。
///
/// 四项都可空：一个指向本机 ollama 的配置只需要 provider 与 model
/// （免鉴权），而一个走公司网关的配置四项都要。
class LocalLlmConfig {
  const LocalLlmConfig({
    this.provider = '',
    this.apiKey = '',
    this.baseUrl = '',
    this.model = '',
  });

  /// 供应商 id，与 `cortex-llm` 的 provider 名一致（deepseek / openai / ollama…）
  final String provider;

  /// 明文 key。免鉴权的供应商（ollama）留空。
  final String apiKey;

  /// 自建 / 中转端点。留空 = 用供应商定义里那个。
  final String baseUrl;

  /// 模型名。留空 = 用供应商定义里的默认。
  final String model;

  /// 够不够本地 agent 直连起来。
  ///
  /// 只要求 provider：key 与端点都可能是不需要的（本机 ollama），
  /// 而模型名有默认值。**不在这里判断 key 是否必需** —— 那是
  /// `cortex-llm` 的 `api_key_env` 说了算，客户端抄一份判断迟早与它漂开。
  bool get isUsable => provider.trim().isNotEmpty;

  bool get isEmpty =>
      provider.trim().isEmpty &&
      apiKey.trim().isEmpty &&
      baseUrl.trim().isEmpty &&
      model.trim().isEmpty;

  /// 塞给本地 agent 进程的环境变量。
  ///
  /// 名字与 cortexd / cortex-local 读的**完全一致** —— 两侧配法一样，
  /// 不必记两套。空值一律不塞：`env::var` 对空串返回 `Ok("")`，
  /// 会把下游的默认值顶掉（这个坑在本仓库出现过四次）。
  Map<String, String> toEnvironment() {
    final env = <String, String>{};
    void put(String k, String v) {
      if (v.trim().isNotEmpty) env[k] = v.trim();
    }

    put('CORTEX_LLM_PROVIDER', provider);
    put('CORTEX_LLM_BASE_URL', baseUrl);
    put('CORTEX_LLM_MODEL', model);
    // 抽取用的廉价模型：离线时没有抽取（那在 cortexd 上），但 agent 构造
    // LlmClient 时两个档位都要有名字。用同一个，省掉一项要填的东西
    put('CORTEX_LLM_CHEAP_MODEL', model);

    // key 的变量名由供应商决定（DEEPSEEK_API_KEY / OPENAI_API_KEY…）。
    // 这里两个都塞：agent 那侧按 `api_key_env` 取用哪一个，
    // 多塞一个不生效的变量没有代价，而漏塞正确的那个就起不来
    if (apiKey.trim().isNotEmpty) {
      final upper = provider.trim().toUpperCase().replaceAll('-', '_');
      env['${upper}_API_KEY'] = apiKey.trim();
    }
    return env;
  }

  LocalLlmConfig copyWith({
    String? provider,
    String? apiKey,
    String? baseUrl,
    String? model,
  }) => LocalLlmConfig(
    provider: provider ?? this.provider,
    apiKey: apiKey ?? this.apiKey,
    baseUrl: baseUrl ?? this.baseUrl,
    model: model ?? this.model,
  );

  Map<String, dynamic> toJson() => {
    'provider': provider,
    'api_key': apiKey,
    'base_url': baseUrl,
    'model': model,
  };

  factory LocalLlmConfig.fromJson(Map<String, dynamic> json) => LocalLlmConfig(
    provider: json['provider'] as String? ?? '',
    apiKey: json['api_key'] as String? ?? '',
    baseUrl: json['base_url'] as String? ?? '',
    model: json['model'] as String? ?? '',
  );

  @override
  bool operator ==(Object other) =>
      other is LocalLlmConfig &&
      other.provider == provider &&
      other.apiKey == apiKey &&
      other.baseUrl == baseUrl &&
      other.model == model;

  @override
  int get hashCode => Object.hash(provider, apiKey, baseUrl, model);

  static const empty = LocalLlmConfig();
}
