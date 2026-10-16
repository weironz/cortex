part of '../http_cortex_api.dart';

mixin _WorkspaceEndpoints on HttpCortexApiBase {
  @override
  Future<List<AgentPresence>> agents({String? session}) async {
    final path = session == null || session.isEmpty
        ? '/agents'
        : '/agents?session=${Uri.encodeQueryComponent(session)}';
    final json = await _getJson(path);
    return [
      for (final a in (json['agents'] as List? ?? const []))
        AgentPresence.fromJson(a as Map<String, dynamic>),
    ];
  }

  @override
  Future<String?> bindLocalWorkspace(String id, String? path) async {
    final http.Response response;
    try {
      response = await _client.put(
        _uri('/local/workspaces/${Uri.encodeComponent(id)}'),
        headers: _headers(const {
          'content-type': 'application/json',
          'accept': 'application/json',
        }),
        body: jsonEncode({'path': path}),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }

    // A plain cortexd has no such route, and neither does a local agent from
    // before it existed. Both answer 404/405 — the caller falls back to
    // `PATCH /sessions/{id}`, which is what every client did until now.
    if (response.statusCode == 404 || response.statusCode == 405) {
      throw _failure(
        response.statusCode,
        '这个后端没有本地工作区端点，改走 PATCH /sessions/{id}。',
        headers: response.headers,
      );
    }
    if (response.statusCode >= 400) {
      // The validator's wording ("整台机器不是工作区") is written for the user.
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    final body = _decodeObject(
      'PUT /local/workspaces',
      utf8.decode(response.bodyBytes),
    );
    return asStringOrNull(body['workspace']);
  }

  @override
  Future<LocalWorkspaceRoot> localWorkspaceRoot() async => _rootCall(
    () => _client.get(
      _uri('/local/workspace-root'),
      headers: _headers(const {'accept': 'application/json'}),
    ),
  );

  @override
  Future<LocalWorkspaceRoot> setLocalWorkspaceRoot(String path) async =>
      _rootCall(
        () => _client.put(
          _uri('/local/workspace-root'),
          headers: _headers(const {
            'content-type': 'application/json',
            'accept': 'application/json',
          }),
          body: jsonEncode({'path': path}),
        ),
      );

  @override
  Future<LocalAttach> localAttach() => _attachCall(
    () => _client.get(
      _uri('/local/attach'),
      headers: _headers(const {'accept': 'application/json'}),
    ),
  );

  @override
  Future<LocalAttach> setLocalAttach(bool enabled) => _attachCall(
    () => _client.put(
      _uri('/local/attach'),
      headers: _headers(const {
        'content-type': 'application/json',
        'accept': 'application/json',
      }),
      body: jsonEncode({'enabled': enabled}),
    ),
  );

  /// 两条开关路由的共同外壳。读与写回的是同一个形状（落定之后的状态）——
  /// 写完不用再读一次，也就没有「读到的是改之前那一份」这种缝。
  Future<LocalAttach> _attachCall(Future<http.Response> Function() send) async {
    final http.Response response;
    try {
      response = await send();
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    // Web 端、纯 cortexd、以及比这条路由旧的本地 agent。调用方据此
    // **整个不画那个开关**，而不是画一个永远关着的
    if (response.statusCode == 404 || response.statusCode == 405) {
      throw _failure(
        response.statusCode,
        '这个后端没有远程接入开关。',
        headers: response.headers,
      );
    }
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
      );
    }
    final body = _decodeObject(
      '/local/attach',
      utf8.decode(response.bodyBytes),
    );
    // 缺字段当成「关着」是**错的**：那会把一台开着的机器画成关着。
    // 读不懂就当这个后端答不出来，走上面那条「整个不画」
    final enabled = body['enabled'];
    if (enabled is! bool) {
      throw const CortexApiException('远程接入开关的响应看不懂。', statusCode: 502);
    }
    return LocalAttach(
      enabled: enabled,
      // 名字缺了不是故障（老一点的 agent 没有这个字段）—— 卡片少一行字，
      // 而开关照常能用。为它把整条路判成「答不出」是不成比例的
      machineHint: asStringOrNull(body['machine_hint']) ?? '',
    );
  }

  /// 两条根目录路由的共同外壳：读、写回的是同一个形状。
  Future<LocalWorkspaceRoot> _rootCall(
    Future<http.Response> Function() send,
  ) async {
    final http.Response response;
    try {
      response = await send();
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    // 纯 cortexd 没有这条路由（Web 端就是这种情况），也包括比它旧的本地
    // agent。调用方按「这台机器上没有本机工作区」处理，而不是当成故障
    if (response.statusCode == 404 || response.statusCode == 405) {
      throw _failure(
        response.statusCode,
        '这个后端没有本地工作空间根目录。',
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
    final body = _decodeObject(
      '/local/workspace-root',
      utf8.decode(response.bodyBytes),
    );
    return LocalWorkspaceRoot(
      root: asStringOrNull(body['root']),
      folders: asStringList(body['folders']),
    );
  }

  @override
  Future<String> createLocalWorkspace({
    required String name,
    String? projectId,
  }) async {
    final http.Response response;
    try {
      response = await _client.post(
        _uri('/local/workspaces'),
        headers: _headers(const {
          'content-type': 'application/json',
          'accept': 'application/json',
        }),
        body: jsonEncode({'name': name, 'project_id': ?projectId}),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode == 404 || response.statusCode == 405) {
      throw _failure(
        response.statusCode,
        '这个后端建不了本地工作空间。',
        headers: response.headers,
      );
    }
    if (response.statusCode >= 400) {
      // 校验器的拒绝话术是写给人看的（「工作空间名里不能有 '/'」），原样上带
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    final body = _decodeObject(
      'POST /local/workspaces',
      utf8.decode(response.bodyBytes),
    );
    return asString(body['path']);
  }

  @override
  Future<String?> autoBindLocalWorkspace(String id) async {
    final http.Response response;
    try {
      response = await _client.post(
        _uri('/local/workspaces/${Uri.encodeComponent(id)}/auto'),
        headers: _headers(const {'accept': 'application/json'}),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode == 404 || response.statusCode == 405) {
      throw _failure(
        response.statusCode,
        '这个后端不会自动开工作空间目录。',
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
    final body = _decodeObject(
      'POST /local/workspaces/{id}/auto',
      utf8.decode(response.bodyBytes),
    );
    return asStringOrNull(body['workspace']);
  }

  // ── MCP ────────────────────────────────────────────
}
