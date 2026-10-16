part of '../http_cortex_api.dart';

mixin _ImportEndpoints on HttpCortexApiBase {
  @override
  Future<ImportTarget> prepareImport(ImportSource source) async {
    // Desktop: nothing moves. The local agent opens this exact path, so the
    // bytes are read once by the process that parses them.
    if (source is ImportPath) return ImportTargetPath(source.path);

    final bytes = (source as ImportBytes).bytes;
    final http.Response response;
    try {
      response = await _client.post(
        _uri('/import/upload'),
        headers: _headers(const {
          'content-type': 'application/octet-stream',
          'accept': 'application/json',
        }),
        body: bytes,
      );
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
    final body = _decodeObject(
      'POST /import/upload',
      utf8.decode(response.bodyBytes),
    );
    final handle = asStringOrNull(body['handle']);
    if (handle == null) {
      throw const CortexApiException('上传成功但没拿到句柄，无法继续');
    }
    return ImportTargetHandle(
      handle,
      expiresInSecs: asInt(body['expires_in_secs'], 3600),
    );
  }

  @override
  Future<ImportEstimate> importPreview(
    ImportTarget target, {
    int? maxConversations,
  }) async {
    final http.Response response;
    try {
      response = await _client.post(
        _uri(_importPath(target, 'preview')),
        headers: _headers(const {
          'content-type': 'application/json',
          'accept': 'application/json',
        }),
        body: jsonEncode(_importBody(target, maxConversations)),
      );
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
    return ImportEstimate.fromJson(
      _decodeObject('import/preview', utf8.decode(response.bodyBytes)),
    );
  }

  @override
  Stream<ImportEvent> runImport(
    ImportTarget target, {
    int? maxConversations,
  }) async* {
    final request = http.Request('POST', _uri(_importPath(target, 'run')))
      ..headers.addAll(
        _headers(const {
          'content-type': 'application/json',
          'accept': 'text/event-stream',
          'cache-control': 'no-cache',
        }),
      )
      ..body = jsonEncode(_importBody(target, maxConversations));

    final http.StreamedResponse response;
    try {
      response = await _client.send(request);
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode != 200) {
      final body = await response.stream.bytesToString().catchError((_) => '');
      throw _failure(
        response.statusCode,
        _unwrapError(body) ??
            HttpCortexApiBase._statusFallback(
              response.statusCode,
              'import/run',
            ),
        headers: response.headers,
      );
    }

    await for (final frame in decodeSse(response.stream)) {
      if (frame.data.isEmpty) continue;
      final Object? decoded;
      try {
        decoded = jsonDecode(frame.data);
      } on FormatException {
        continue;
      }
      if (decoded is! Map<String, Object?>) continue;
      final event = _importEvent(decoded);
      if (event != null) yield event;
    }
  }

  /// Desktop and Web hit different routes; the *shape* on the wire is the same.
  ///
  /// `/local/…` is answered by the local agent and never leaves the machine;
  /// the unprefixed one is cortexd's, reached with a spool handle.
  String _importPath(ImportTarget target, String verb) =>
      target is ImportTargetPath ? '/local/import/$verb' : '/import/$verb';

  Map<String, Object?> _importBody(ImportTarget target, int? maxConversations) {
    return {
      ...target.locator,
      // Omitted rather than sent as null: the server treats an absent field as
      // "no limit", and an explicit null would have to mean the same thing —
      // two spellings for one meaning is how they end up disagreeing.
      'max_conversations': ?maxConversations,
    };
  }

  /// Unknown event types are **dropped, not guessed**.
  ///
  /// A newer daemon may emit a frame this build has never heard of. Rendering
  /// it as a generic error would turn a forward-compatible addition into a
  /// visible failure.
  ImportEvent? _importEvent(Map<String, Object?> json) {
    switch (asStringOrNull(json['type'])) {
      case 'started':
        final e = json['estimate'];
        if (e is! Map<String, Object?>) return null;
        return ImportStartedEvent(ImportEstimate.fromJson(e));
      case 'progress':
        return ImportProgressEvent(
          conversationsDone: asInt(json['conversations_done']),
          conversationsTotal: asInt(json['conversations_total']),
          pairsDone: asInt(json['pairs_done']),
          skipped: asInt(json['skipped']),
          failures: asInt(json['failures']),
        );
      case 'done':
        return ImportDoneEvent(
          pairsDone: asInt(json['pairs_done']),
          skipped: asInt(json['skipped']),
          failures: asInt(json['failures']),
        );
      case 'error':
        return ImportErrorEvent(
          asStringOrNull(json['message']) ?? '导入失败，但服务端没说原因',
        );
      default:
        return null;
    }
  }
}
