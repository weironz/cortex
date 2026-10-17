part of '../http_cortex_api.dart';

mixin _SessionsEndpoints on HttpCortexApiBase {
  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async {
    final json = await _getJson('/sessions', {
      if (includeArchived) 'include_archived': 'true',
      // 缺省即「全部」。传空串会被服务端当成一个叫 "" 的项目 id 去查，
      // 结果是一个空列表 —— 而调用方要的是「不过滤」
      'project_id': ?projectId,
    });
    return asObjectList(
      json['sessions'],
    ).map(ChatSession.fromJson).toList(growable: false);
  }

  @override
  Future<List<SessionSearchHit>> searchSessions(
    String query, {
    bool includeArchived = false,
  }) async {
    final json = await _getJson('/sessions/search', {
      'q': query,
      if (includeArchived) 'include_archived': 'true',
    });
    return asObjectList(
      json['hits'],
    ).map(SessionSearchHit.fromJson).toList(growable: false);
  }

  @override
  Future<ChatSession> moveSessionToProject(
    String sessionId,
    String? projectId,
  ) async => ChatSession.fromJson(
    // 显式的 `null` 就是「移出」。这里不能用 `?projectId` 的省略语法：
    // 省略掉字段等于「别动分组」，而调用方明确要求了移出
    await _patchJson('/sessions/${Uri.encodeComponent(sessionId)}', {
      'project_id': projectId,
    }),
  );

  @override
  Future<ChatSession> setContainerWorkspace(
    String sessionId,
    String? name,
  ) async => ChatSession.fromJson(
    // 与上面同理：显式的 `null` 才是「回到卷根」，省略字段等于「别动」
    await _patchJson('/sessions/${Uri.encodeComponent(sessionId)}', {
      'container_workspace': name,
    }),
  );

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async => SessionDetail.fromJson(
    await _getJson('/sessions/${Uri.encodeComponent(id)}', {
      if (limit != null) 'limit': '$limit',
      // Sent back verbatim. `Uri` percent-encodes it for us, and the daemon's
      // `|` separator survives that round trip.
      'before': ?before,
    }),
  );

  @override
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    bool? pinned,
    String? workspace,
    bool clearWorkspace = false,
  }) async {
    // The tri-state lives here: an *absent* key means "leave it", an explicit
    // `null` means "unbind". `jsonEncode` emits both correctly, but only if the
    // key is added conditionally rather than always with a nullable value.
    final body = <String, dynamic>{
      'title': ?title,
      'archived': ?archived,
      'pinned': ?pinned,
      if (clearWorkspace) 'workspace': null,
      if (!clearWorkspace && workspace != null) 'workspace': workspace,
    };

    final http.Response response;
    try {
      response = await _client.patch(
        _uri('/sessions/${Uri.encodeComponent(id)}'),
        headers: _headers(const {
          'content-type': 'application/json',
          'accept': 'application/json',
        }),
        body: jsonEncode(body),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }

    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        // A daemon that predates the route answers 405 (the path exists, but
        // only for GET) with an empty body. Anything else — notably the
        // workspace validator's 400 — carries a message written for the user,
        // so it is passed through untouched.
        response.statusCode == 405 || response.statusCode == 404
            ? 'cortexd 还没有 PATCH /sessions/{id}，改动只在本地生效。'
            : _errorMessage(response),
        headers: response.headers,
      );
    }
    return ChatSession.fromJson(
      _decodeObject('PATCH /sessions', utf8.decode(response.bodyBytes)),
    );
  }

  @override
  Future<ChatSession> forkSession(String id, {String? upToEpisodeId}) async {
    final http.Response response;
    try {
      response = await _client.post(
        _uri('/sessions/${Uri.encodeComponent(id)}/fork'),
        headers: _headers(const {
          'content-type': 'application/json',
          'accept': 'application/json',
        }),
        // 截断点是消息 id，不是「前 N 条」：序号要两端对口径，数错是
        // 静默截错；id 指错服务端会明确回 400
        body: jsonEncode({'up_to_episode_id': ?upToEpisodeId}),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      // 404 在这条路上有两个来源：老服务端没这条路由（axum 兜底，空 body），
      // 与「会话不存在」（服务端写了给人看的话）。有话就原样透出，
      // 没话才翻成「老版本」—— 反过来会把真 404 也说成版本问题
      final served = _unwrapError(utf8.decode(response.bodyBytes));
      final oldServer =
          (response.statusCode == 404 && served == null) ||
          response.statusCode == 405;
      throw _failure(
        response.statusCode,
        oldServer ? '这个服务端还不支持分叉会话 —— 它是个老版本。' : _errorMessage(response),
        headers: response.headers,
      );
    }
    return ChatSession.fromJson(
      _decodeObject('POST /sessions/fork', utf8.decode(response.bodyBytes)),
    );
  }

  @override
  Future<Episode> episode(String id) async =>
      Episode.fromJson(await _getJson('/episodes/${Uri.encodeComponent(id)}'));

  @override
  Future<Uint8List> exportSessions() => _sandboxBytes('/sessions/export');
}
