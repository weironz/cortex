part of '../http_cortex_api.dart';

mixin _McpEndpoints on HttpCortexApiBase {
  @override
  Future<McpConfigView> mcpConfig() => _mcpCall(
    'GET /local/mcp',
    () => _client.get(
      _uri('/local/mcp'),
      headers: _headers(const {'accept': 'application/json'}),
    ),
  );

  @override
  Future<McpConfigView> saveMcpServer({
    required String name,
    required Map<String, dynamic> config,
    String trust = 'ask',
    bool disabled = false,
    List<String> removeEnv = const [],
  }) => _mcpCall(
    'PUT /local/mcp/servers/{name}',
    () => _client.put(
      _uri('/local/mcp/servers/${Uri.encodeComponent(name)}'),
      headers: _headers(const {
        'content-type': 'application/json',
        'accept': 'application/json',
      }),
      // 传输配置原样铺开，再挂上我们自己的两个字段。服务端那边是
      // `#[serde(flatten)]`，两侧形状必须一致
      body: jsonEncode({
        ...config,
        'trust': trust,
        'disabled': disabled,
        'remove_env': removeEnv,
      }),
    ),
  );

  @override
  Future<McpConfigView> deleteMcpServer(String name) => _mcpCall(
    'DELETE /local/mcp/servers/{name}',
    () => _client.delete(
      _uri('/local/mcp/servers/${Uri.encodeComponent(name)}'),
      headers: _headers(const {'accept': 'application/json'}),
    ),
  );

  @override
  Future<McpConfigView> reloadMcp() => _mcpCall(
    'POST /local/mcp/reload',
    () => _client.post(
      _uri('/local/mcp/reload'),
      headers: _headers(const {'accept': 'application/json'}),
    ),
  );

  /// 四条回同一个形状的路由的共同外壳。
  Future<McpConfigView> _mcpCall(
    String label,
    Future<http.Response> Function() send,
  ) async {
    final body = await _mcpRaw(label, send);
    return McpConfigView.fromJson(
      body is Map<String, dynamic> ? body : const {},
    );
  }

  /// 发一次、把 404/405 翻译成「这个后端没有本机 MCP」、解出 JSON。
  ///
  /// 404 单独拎出来的理由与 `_rootCall` 那条一样：纯 cortexd（Web 端）
  /// 与旧版本的本地 agent 都没有这几条路由，而那不是故障 —— 调用方按
  /// 「这台机器上没有 MCP」处理。
  Future<Object?> _mcpRaw(
    String label,
    Future<http.Response> Function() send,
  ) async {
    final http.Response response;
    try {
      response = await send();
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode == 404 || response.statusCode == 405) {
      throw _failure(
        response.statusCode,
        '这个后端没有本机 MCP。',
        headers: response.headers,
      );
    }
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    return jsonDecode(utf8.decode(response.bodyBytes)) as Object?;
  }

  @override
  Future<List<McpParsedServer>> parseMcpPaste(String text) async {
    final body = await _mcpRaw(
      'POST /local/mcp/parse',
      () => _client.post(
        _uri('/local/mcp/parse'),
        headers: _headers(const {
          'content-type': 'application/json',
          'accept': 'application/json',
        }),
        body: jsonEncode({'text': text}),
      ),
    );
    return (body as List? ?? const [])
        .whereType<Map<String, dynamic>>()
        .map(McpParsedServer.fromJson)
        .toList();
  }

  @override
  Future<List<McpRegistryEntry>> searchMcpRegistry(String query) async {
    final body = await _mcpRaw(
      'GET /local/mcp/registry',
      () => _client.get(
        _uri('/local/mcp/registry', {'q': query}),
        headers: _headers(const {'accept': 'application/json'}),
      ),
    );
    return (body as List? ?? const [])
        .whereType<Map<String, dynamic>>()
        .map(McpRegistryEntry.fromJson)
        .toList();
  }
}
