part of '../mock_cortex_api.dart';

mixin _ModelsMock on MockCortexApiBase {
  /// 来源列表要**真的有东西**。
  ///
  /// `ModelSourcesUnsupported` 回的是空的 `ModelSources`，而空列表在界面上
  /// 的含义是「这个部署一条来源都没有」。照搬它，模型那一页在 mock 形态下
  /// 就是一片空白 —— 测试与离线演示看到的都不是真实形状。
  ///
  /// 三条足够：部署提供的那条（内置、计配额）、一条自带 key 的、
  /// 一条关着的。
  /// mock 上「没配过、部署也没有 key」—— 于是设置页画的是「还没接」那一态，
  /// 而那正是一个新用户第一次打开它时看到的
  SearchPrefs _search = const SearchPrefs(
    providers: [
      SearchProviderInfo(
        id: 'tavily',
        name: 'Tavily',
        defaultBase: 'https://api.tavily.com',
        canFetch: true,
      ),
      SearchProviderInfo(
        id: 'bocha',
        name: '博查',
        defaultBase: 'https://api.bochaai.com',
        canFetch: false,
      ),
      SearchProviderInfo(
        id: 'exa',
        name: 'Exa',
        defaultBase: 'https://api.exa.ai',
        canFetch: true,
      ),
      SearchProviderInfo(
        id: 'brave',
        name: 'Brave',
        defaultBase: 'https://api.search.brave.com',
        canFetch: false,
      ),
    ],
  );

  @override
  Future<SearchPrefs> searchPrefs() async => _search;

  @override
  Future<SearchPrefs> saveSearchPrefs({
    String? provider,
    String? apiKey,
    String? baseUrl,
    int? maxResults,
    String? depth,
    int? cutoffLimit,
    List<String>? excludeDomains,
  }) async {
    _search = SearchPrefs(
      provider: provider ?? _search.provider,
      // 与服务端同一条约定：空串 = 清掉，缺省 = 不动
      keyTail: apiKey == null
          ? _search.keyTail
          : (apiKey.length < 4 ? '' : apiKey.substring(apiKey.length - 4)),
      baseUrl: baseUrl ?? _search.baseUrl,
      maxResults: maxResults ?? _search.maxResults,
      depth: depth ?? _search.depth,
      cutoffLimit: cutoffLimit ?? _search.cutoffLimit,
      excludeDomains: excludeDomains ?? _search.excludeDomains,
      deploymentKey: _search.deploymentKey,
      providers: _search.providers,
    );
    return _search;
  }

  @override
  Future<ModelSources> modelSources() async {
    await _latency(90);
    return const ModelSources(
      canAdd: true,
      sources: [
        ModelSource(
          id: kDeploymentSource,
          provider: 'deepseek',
          label: '部署提供',
          builtin: true,
          models: [
            'deepseek-v4-pro',
            'deepseek-v4-flash',
            'legacy-completion',
            'unknown-model',
          ],
        ),
        ModelSource(
          id: '01M0MOCKSOURCEAAAAAAAAAAAA',
          provider: 'alibaba',
          keyTail: '5236',
          freeOfQuota: true,
          models: ['qwen-flash', 'qwen-plus'],
        ),
        ModelSource(
          id: '01M0MOCKSOURCEBBBBBBBBBBBB',
          provider: 'ollama',
          label: '本机 Ollama',
          keyTail: '',
          enabled: false,
          baseUrl: 'http://localhost:11434',
          freeOfQuota: true,
        ),
      ],
      providers: [
        ProviderChoice(
          id: 'deepseek',
          displayName: 'DeepSeek',
          description: 'DeepSeek 官方 API（OpenAI 兼容）',
          baseUrl: 'https://api.deepseek.com',
        ),
        ProviderChoice(
          id: 'alibaba',
          displayName: 'Alibaba (Qwen)',
          description: '通义千问',
          baseUrl: 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1',
        ),
        ProviderChoice(
          id: 'ollama',
          displayName: 'Ollama',
          description: '本机 Ollama，无需密钥',
          baseUrl: 'http://localhost:11434',
          requiresAuth: false,
        ),
      ],
    );
  }

  /// 拉型号。
  ///
  /// 夹具里**特意混齐五种形态**，否则筛选那几条界面分支一条都画不出来：
  /// 能跑 agent 的、看得懂图的、能生图的、有思考的、以及目录里查不到的
  /// （能力字段全 null —— 那是「不知道」，不是「不行」）。
  ///
  /// `live = false` 也是故意的：让「回落要说出来」那条分支在演示形态下
  /// 也看得见。
  @override
  Future<FetchedModels> fetchSourceModels(String id) async {
    await _latency(200);
    return const FetchedModels(
      live: false,
      note: '问不到这家的型号列表（mock 后端不联网），下面这份是内置的',
      models: [
        FetchedModel(
          id: 'qwen-flash',
          displayName: 'Qwen Flash',
          context: 1000000,
          toolCall: true,
          vision: false,
          imageOutput: false,
          reasoning: true,
          inputMicrosPerMtok: 50000,
          outputMicrosPerMtok: 400000,
        ),
        FetchedModel(
          id: 'qwen-vl-max',
          displayName: 'Qwen VL Max',
          context: 262144,
          toolCall: true,
          vision: true,
          imageOutput: false,
          inputMicrosPerMtok: 200000,
          outputMicrosPerMtok: 600000,
        ),
        FetchedModel(
          id: 'qwen-image-3.0',
          displayName: 'Qwen Image 3.0',
          toolCall: false,
          imageOutput: true,
        ),
        FetchedModel(
          id: 'legacy-completion',
          displayName: '老式补全模型',
          context: 8000,
          toolCall: false,
          inputMicrosPerMtok: 50000,
          outputMicrosPerMtok: 50000,
        ),
        // 目录里查不到的：三个能力字段全 null
        FetchedModel(id: 'brand-new-model'),
      ],
    );
  }

  @override
  Future<ModelCatalog> llmModels() async {
    await _latency(140);
    // 夹具里**特意混三种形态**：能跑 agent 的、不支持工具调用的、
    // 以及目录里查不到的（三个字段全 null）。三条界面分支只有在有这三种
    // 数据时才画得出来
    return const ModelCatalog(
      provider: 'deepseek',
      defaultModel: 'deepseek-v4-pro',
      cheapModel: 'deepseek-v4-flash',
      autoAvailable: true,
      // **两条来源**：部署提供的（占配额）与用户自带的 alibaba（他的 key）。
      // 一份只有部署那家型号的夹具画不出「按来源分组」那条界面分支，
      // 而那正是从前那个 bug 的所在 —— 配了 alibaba key 的人在选择器里
      // 根本看不到 qwen
      models: [
        ModelOption(
          id: 'deepseek-v4-pro',
          displayName: 'DeepSeek V4 Pro',
          source: kDeploymentSource,
          sourceLabel: '部署提供',
          context: 1000000,
          toolCall: true,
          vision: false,
          reasoning: true,
          inputMicrosPerMtok: 435000,
          outputMicrosPerMtok: 870000,
        ),
        ModelOption(
          id: 'deepseek-v4-flash',
          displayName: 'DeepSeek V4 Flash',
          source: kDeploymentSource,
          sourceLabel: '部署提供',
          context: 1000000,
          toolCall: true,
          vision: false,
          inputMicrosPerMtok: 140000,
          outputMicrosPerMtok: 280000,
        ),
        ModelOption(
          id: 'legacy-completion',
          displayName: '老式补全模型',
          source: kDeploymentSource,
          sourceLabel: '部署提供',
          context: 8000,
          toolCall: false,
          inputMicrosPerMtok: 50000,
          outputMicrosPerMtok: 50000,
        ),
        ModelOption(
          id: 'unknown-model',
          displayName: 'unknown-model',
          source: kDeploymentSource,
          sourceLabel: '部署提供',
        ),
        ModelOption(
          id: 'qwen-flash',
          displayName: 'Qwen Flash',
          source: '01M0MOCKSOURCEAAAAAAAAAAAA',
          sourceLabel: 'Alibaba (Qwen)',
          freeOfQuota: true,
          context: 1000000,
          toolCall: true,
          inputMicrosPerMtok: 50000,
          outputMicrosPerMtok: 400000,
        ),
      ],
    );
  }

  @override
  Future<UsageReport> usage() async {
    await _latency(160);
    // 夹具里**特意混一个没有价目的模型**：那条路（金额算不出来时说清楚，
    // 而不是显示 ¥0.00）只有在有这种行时才画得出来
    return const UsageReport(
      usedTokens: 1167079,
      ownKeyTokens: 121,
      limitTokens: 2000000,
      remainingTokens: 832921,
      windowDays: 30,
      costMicros: 2729748,
      currency: 'CNY',
      unpricedTokens: 12040,
      byModel: [
        ModelUsage(
          model: 'deepseek-v4-pro',
          inputTokens: 1101156,
          outputTokens: 65923,
          costMicros: 2729696,
        ),
        ModelUsage(
          model: 'deepseek-v4-flash',
          inputTokens: 91,
          outputTokens: 30,
          costMicros: 105,
        ),
        ModelUsage(
          model: 'local-qwen3-32b',
          inputTokens: 9000,
          outputTokens: 3040,
        ),
      ],
    );
  }

  // ---------------------------------------------------------------- projects
}
