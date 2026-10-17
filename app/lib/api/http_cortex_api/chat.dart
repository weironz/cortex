part of '../http_cortex_api.dart';

mixin _ChatEndpoints on HttpCortexApiBase {
  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    Assistant? assistant,
    List<Skill> skills = const [],
    bool computerUse = false,
    ImagePrefs? imagePrefs,
  }) async* {
    final request = http.Request('POST', _uri('/chat'))
      // A header, not a ticket: `POST /chat` is issued by `package:http`, which
      // can set one. The ticket exists for `WebSocket` and `<img>`, which
      // cannot — using it here would put a credential in a URL for no reason.
      ..headers.addAll(
        _headers(const {
          'content-type': 'application/json',
          'accept': 'text/event-stream',
          // Defeats any proxy that would otherwise buffer the whole response
          // and collapse the stream into a single chunk.
          'cache-control': 'no-cache',
        }),
      )
      ..body = jsonEncode({
        'session_id': sessionId,
        'message': message,
        // `#[serde(default)]` on the server, so an empty list is equivalent to
        // omitting it — sent unconditionally to keep the shape uniform.
        //
        // No workspace here on purpose: the daemon reads the binding off the
        // session row, so the fence's key never rides on the same request as
        // the message.
        'attachments': [for (final a in attachments) a.toWireJson()],
        // 逐轮带，不存在会话上：用户在输入框底部随时能改，改完**下一句**
        // 就该按新档位走。存服务端要多一次同步，而那次同步失败时用户看到的是
        // 「我明明切了档」。老服务端不认识这个字段会忽略它（serde default）
        'permission_mode': permissionMode.wire,
        // 只在真的选过时才带这个字段：不带 = 部署默认，而带一个 null
        // 与不带在服务端是同一个意思，少发一个字段更省事
        if (model != null && model.isNotEmpty) 'model': model,
        // 来源是独立字段，不编进 model —— ollama 的型号名本身带冒号，
        // 而且同一家可以配两条来源
        if (source != null && source.isNotEmpty) 'source': source,
        // 空人设的智能体**不发** —— 它等于没有智能体，而发过去会让服务端
        // 拿一段没有身份描述的提示词去替换默认那句（比默认那句更糟）
        if (assistant != null && assistant.isMeaningful)
          'assistant': assistant.toBrief(),
        // 目录带上，**正文不带** —— 分层的意义就在这一点上。
        // 关掉的、没名字的都进不了目录（`isListable`），与服务端那条
        // `SkillBrief::is_listable` 是同一个判据
        // ⚠️ 只在**真的开着**时才发。恒发一个 false 也能跑，但那样老服务端
        // 的日志里会多一个它不认识的字段，而且这个字段的缺席本身就是
        // 「没开」——不发比发 false 更不容易出错
        if (computerUse) 'computer_use': true,
        if (skills.any((s) => s.isListable))
          'skills': [
            for (final s in skills)
              if (s.isListable) s.toBrief(),
          ],
        // 什么都没设时整个字段不发 —— 发一个全默认的对象与不发在服务端
        // 是同一个意思（`resolve_image_spec` 两边都走「听模型的」）
        if (imagePrefs != null && !imagePrefs.isDefault)
          'image_prefs': imagePrefs.toJson(),
      });

    final http.StreamedResponse response;
    try {
      response = await _client.send(request);
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }

    yield* _events(response, 'POST /chat');
  }

  @override
  Future<void> stopRun(String sessionId) async {
    // 与 `attachChat` 打同一条路径，只是方法不同 —— 桌面端落到本机 agent，
    // Web 落到 agentd 那条同名别名，再由它转进容器
    final http.Response response;
    try {
      response = await _client.delete(
        _uri('/runs/${Uri.encodeComponent(sessionId)}'),
        headers: _headers(const {'accept': 'application/json'}),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    // 404 = 那一轮刚好自己结束了。**不是错误**，也不该弹给用户看 ——
    // 他按下停止时它已经停了，那正是他要的结果
    if (response.statusCode == 404) return;
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
  }

  @override
  Stream<ChatEvent> attachChat(String sessionId) async* {
    // GET，没有 body。**与 `chat` 走同一个解析**（`_events`）：重挂拿到的
    // 是「重放 + 后续」拼成的一条流，形状与新发那条一模一样 ——
    // 两边各写一份解析的话，其中一份必然少处理一种事件
    final request =
        http.Request('GET', _uri('/runs/${Uri.encodeComponent(sessionId)}'))
          ..headers.addAll(
            _headers(const {
              'accept': 'text/event-stream',
              'cache-control': 'no-cache',
            }),
          );

    final http.StreamedResponse response;
    try {
      response = await _client.send(request);
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }

    // 404 = 这个会话此刻没有轮次在跑。**不是错误** —— 绝大多数会话都是
    // 这样。调用方按「照常拉历史」处理，所以这里给它一个认得出来的状态码
    if (response.statusCode == 404 || response.statusCode == 405) {
      await response.stream.drain<void>().catchError((_) {});
      throw _failure(404, '这个会话现在没有正在跑的轮次。');
    }
    yield* _events(response, 'GET /runs/{id}');
  }

  /// 把一条 SSE 响应解析成事件流。`chat` 与 `attachChat` 共用。
  Stream<ChatEvent> _events(
    http.StreamedResponse response,
    String label,
  ) async* {
    if (response.statusCode != 200) {
      final body = await response.stream.bytesToString().catchError((_) => '');
      // 剥掉 `{"error": …}` 的信封再给人看 —— 见 [_unwrapError]
      throw _failure(
        response.statusCode,
        _unwrapError(body) ??
            HttpCortexApiBase._statusFallback(response.statusCode, label),
        headers: response.headers,
        retryable: _unwrapRetryable(body),
      );
    }

    // `keepAlive: true` —— 心跳要一路送到 `ChatController`，见
    // [ChatHeartbeatEvent]。丢在这一层的话，上面那条空转看门狗就只剩
    // 「多久没有 delta」可判，而那会把跑长命令的那一轮当场误杀。
    await for (final frame in decodeSse(response.stream, keepAlive: true)) {
      if (frame.isKeepAlive) {
        yield const ChatHeartbeatEvent();
        continue;
      }
      if (frame.data.isEmpty) continue;
      // Some servers terminate SSE with the literal sentinel `[DONE]`.
      if (frame.data == '[DONE]') {
        yield const ChatDoneEvent(null);
        continue;
      }
      final Object? decoded;
      try {
        decoded = jsonDecode(frame.data);
      } on FormatException {
        // A malformed frame should not kill an otherwise healthy stream.
        continue;
      }
      if (decoded is Map<String, dynamic>) {
        yield ChatEvent.fromJson(decoded);
      }
    }
  }

  // ----------------------------------------------------------- confirmations

  @override
  Future<List<PendingConfirmation>> pendingConfirmations({
    String? sessionId,
  }) async {
    final json = await _getJson('/confirmations', {'session_id': ?sessionId});
    // One `now` for the whole page so every entry's countdown is anchored to
    // the same instant; decoding entry-by-entry would skew a long list by the
    // time it took to parse.
    final now = DateTime.now();
    return asObjectList(json['pending'])
        .map((e) => PendingConfirmation.fromJson(e, now: now))
        .toList(growable: false);
  }

  @override
  Future<bool> answerConfirmation({
    required String token,
    required bool allow,
    String? sessionId,
  }) async {
    try {
      // 会话走**查询串**而不是 body：服务端要在解析 body 之前就知道该把这条
      // 转进哪个容器，而 body 是原样流式送进去的、它一个字节都不看
      await _postJson(
        '/confirmations',
        {
          // In the body, never the path. `POST /confirmations/{token}` would be
          // tidier REST and would write a credential that approves shell
          // execution into every access log on the way.
          'token': token,
          // A two-value enum rather than a bool: `{"approve":"false"}` decodes as
          // `true` in a surprising number of client libraries, and the direction
          // that mistake fails in is "ran a command nobody approved".
          'decision': allow ? 'allow' : 'deny',
        },
        {'session_id': ?sessionId},
      );
      return true;
    } on CortexApiException catch (e) {
      // The ordinary outcome of losing the race. Not routed through
      // `isUnsupported` even though that getter also matches 404 — here the
      // meaning is "this token is spent", not "this daemon lacks the route".
      if (e.statusCode == 404) return false;
      rethrow;
    }
  }
}
