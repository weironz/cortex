part of '../http_cortex_api.dart';

mixin _ModelsEndpoints on HttpCortexApiBase {
  @override
  Future<ModelCatalog> llmModels() async =>
      ModelCatalog.fromJson(await _getJson('/llm/models'));

  @override
  Future<UsageReport> usage() async =>
      UsageReport.fromJson(await _getJson('/auth/usage'));

  // ---------------------------------------------------------------- projects

  @override
  Future<SearchPrefs> searchPrefs() async =>
      SearchPrefs.fromJson(await _getJson('/settings/search'));

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
    // ⚠️ **每一位缺省 = 不动**，所以这里只放真的要改的那几个。
    // 全都发一遍的话，「只改结果个数」会把 key 一起写成空串（界面手上
    // 没有明文），而那等于替用户清掉了它
    final body = <String, dynamic>{
      'provider': ?provider,
      'api_key': ?apiKey,
      'base_url': ?baseUrl,
      'max_results': ?maxResults,
      'depth': ?depth,
      'cutoff_limit': ?cutoffLimit,
      'exclude_domains': ?excludeDomains,
    };
    return SearchPrefs.fromJson(await _patchJson('/settings/search', body));
  }

  @override
  Future<ModelSources> modelSources() => _sources('GET', '', null);

  @override
  Future<ModelSources> saveModelSource({
    String? id,
    required String provider,
    String apiKey = '',
    String label = '',
    String? baseUrl,
    bool? enabled,
    List<String>? models,
  }) => _sources(id == null ? 'POST' : 'PUT', id == null ? '' : '/$id', {
    'provider': provider,
    // 改一条时留空 = 不动原来那把。界面永远拿不到明文（只回后四位），
    // 所以「改个端点」这种操作根本没有 key 可以回传
    'api_key': apiKey,
    'label': label,
    if (baseUrl != null) 'base_url': baseUrl.trim(),
    // ⚠️ 这两个用 `?` 而不是 `if (x != null)` —— lint 要求的写法，
    // 语义相同：null 就整个键不出现，而服务端那侧「键不出现」正是
    // 「保持原样」
    'enabled': ?enabled,
    'models': ?models,
  });

  @override
  Future<ModelSources> deleteModelSource(String id) =>
      _sources('DELETE', '/$id', null);

  @override
  Future<FetchedModels> fetchSourceModels(String id) async {
    final body = await _sourcesRaw('POST', '/$id/models', null);
    return FetchedModels.fromJson(body);
  }

  @override
  Future<ModelSources> saveModelCaps(
    String sourceId,
    String modelId,
    CapsOverride caps,
  ) async {
    // 型号名进路径，必须转义：ollama 的名字带冒号（`llama3:8b`），
    // 中转站的还带斜杠（`deepseek/deepseek-v4-pro`）—— 不转义的话
    // 后者会被路由当成多出来的一段，表现是 404 而不是保存失败
    final body = await _sourcesRaw(
      'PUT',
      '/$sourceId/models/${Uri.encodeComponent(modelId)}',
      caps.toJson(),
    );
    return ModelSources.fromJson(body);
  }

  @override
  Future<SourceCheck> checkModelSource(String id, {String model = ''}) async {
    final body = await _sourcesRaw('POST', '/$id/check', {'model': model});
    return SourceCheck.fromJson(body);
  }

  @override
  Future<RoleAssignments> modelRoles() async =>
      RoleAssignments.fromJson(await _rolesRaw('GET', null));

  @override
  Future<RoleAssignments> saveModelRoles(RoleAssignments roles) async =>
      RoleAssignments.fromJson(await _rolesRaw('PUT', roles.toJson()));

  /// 两个动作共用一段 —— 与 [_sourcesRaw] 同一个理由：各写一遍的话，
  /// 迟早有一处忘了检查状态码，然后把一段错误 JSON 当成正常响应解析，
  /// 界面上显示「一个角色都没指派」，而实际是存失败了。
  Future<Map<String, dynamic>> _rolesRaw(
    String method,
    Map<String, Object?>? body,
  ) async {
    const path = '/settings/model-roles';
    final http.Response response;
    try {
      final req = http.Request(method, _uri(path))
        ..headers.addAll(_headers(const {'accept': 'application/json'}));
      if (body != null) {
        req.headers['content-type'] = 'application/json';
        req.body = jsonEncode(body);
      }
      response = await http.Response.fromStream(await _client.send(req));
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    return _decodeObject(path, utf8.decode(response.bodyBytes));
  }

  Future<ModelSources> _sources(
    String method,
    String suffix,
    Map<String, Object?>? body,
  ) async => ModelSources.fromJson(await _sourcesRaw(method, suffix, body));

  /// 四个动作共用一段。
  ///
  /// 合成一个是因为**错误处理必须逐字相同**：各写一遍的话，迟早有一处
  /// 忘了检查状态码，然后把一段错误 JSON 当成正常响应解析，
  /// 界面上显示「一条来源都没有」—— 而实际是存失败了。
  Future<Map<String, dynamic>> _sourcesRaw(
    String method,
    String suffix,
    Map<String, Object?>? body,
  ) async {
    final path = '/settings/model-sources$suffix';
    final uri = _uri(path);
    final http.Response response;
    try {
      final req = http.Request(method, uri)
        ..headers.addAll(_headers(const {'accept': 'application/json'}));
      if (body != null) {
        req.headers['content-type'] = 'application/json';
        // 明文只在这里出现一次。**绝不进 query string** —— 那会落进
        // nginx 的访问日志，而访问日志通常比数据库更容易被人翻到
        req.body = jsonEncode(body);
      }
      response = await http.Response.fromStream(await _client.send(req));
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    return _decodeObject(path, utf8.decode(response.bodyBytes));
  }
}
