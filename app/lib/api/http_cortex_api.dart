import '../core/permission_mode.dart';
import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:http/http.dart' as http;
import 'package:web_socket_channel/web_socket_channel.dart';

import '../import/import_source.dart';
import '../models/account.dart';
import '../models/auth_tokens.dart';
import '../models/llm_key_status.dart';
import '../models/attachment.dart';
import '../models/blob.dart';
import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/import_plan.dart';
import '../models/json.dart';
import '../models/memory_search_result.dart';
import '../models/pending_confirmation.dart';
import '../models/project.dart';
import '../models/session_detail.dart';
import '../models/sync_event.dart';
import '../models/sync_record.dart';
import '../models/workspace.dart';
import 'api_exception.dart';
import 'cortex_api.dart';
import 'http_client_factory.dart';
import 'sse.dart';

/// Talks to a real `cortexd` over HTTP + SSE.
class HttpCortexApi implements CortexApi {
  HttpCortexApi({
    required String baseUrl,
    String? token,
    http.Client? client,
    this.onUnauthorized,
  }) : _base = _normalise(baseUrl),
       _token = (token != null && token.trim().isEmpty) ? null : token?.trim(),
       _client = client ?? createHttpClient();

  final Uri _base;
  final http.Client _client;

  /// The long-lived bearer credential, or null against a daemon running with
  /// `CORTEX_AUTH=disabled`.
  ///
  /// Immutable for the life of the instance: a token change means a *different*
  /// identity, and `cortexApiProvider` rebuilds the whole client for it. Making
  /// it settable would leave in-flight requests straddling two identities and
  /// would keep a stale token alive inside closures.
  final String? _token;

  /// Called once whenever the daemon answers 401.
  ///
  /// A callback rather than "every caller checks `isUnauthorized`": expiry can
  /// surface on *any* of a dozen routes, and the reaction is always the same
  /// (fall back to the login gate). Handling it per call site is how you end up
  /// with the one path that forgot — and its symptom is a permanently empty
  /// pane or a spinner that never resolves, which is exactly what this must
  /// prevent.
  final void Function()? onUnauthorized;

  /// Cached short-lived ticket for connections that cannot carry a header.
  ///
  /// Cached rather than minted per use because the daemon's tickets are
  /// explicitly *reusable within their minute* — `TicketBook::valid` does not
  /// consume them, precisely so a page of images and a reconnecting socket do
  /// not each need their own round trip.
  AuthTicket? _ticket;
  Future<AuthTicket>? _ticketInFlight;

  static Uri _normalise(String raw) {
    var s = raw.trim();
    if (s.isEmpty) s = 'http://127.0.0.1:8080';
    if (!s.startsWith('http://') && !s.startsWith('https://')) {
      s = 'http://$s';
    }
    while (s.endsWith('/')) {
      s = s.substring(0, s.length - 1);
    }
    return Uri.parse(s);
  }

  Uri _uri(String path, [Map<String, String>? query]) => _base.replace(
    path: '${_base.path}$path',
    queryParameters: (query == null || query.isEmpty) ? null : query,
  );

  /// `http` → `ws`, `https` → `wss`. Same origin as everything else, so a
  /// deployment only ever has one host to configure.
  ///
  /// [ticket] rides in the query string, and **only** a ticket ever may. See
  /// [issueTicket]: the long-lived token is barred from URLs because they are
  /// logged in three places the body never reaches.
  Uri _wsUri(String? ticket) => _uri('/ws', {
    'ticket': ?ticket,
  }).replace(scheme: _base.scheme == 'https' ? 'wss' : 'ws');

  @override
  String get label => '$_base · $kHttpClientKind';

  /// Request headers with the bearer credential folded in.
  ///
  /// Never applied to [putPresigned]: those bytes go straight to the object
  /// store, which is a different party. Sending cortexd's token there would
  /// hand a third-party service a credential to the entire memory store, and
  /// S3's signature covers the header set anyway — it would break the upload
  /// while leaking the secret.
  Map<String, String> _headers([Map<String, String> extra = const {}]) => {
    ...extra,
    if (_token case final token?) 'authorization': 'Bearer $token',
  };

  /// Builds the exception for a >= 400 response and, on 401, rings the bell.
  ///
  /// Centralised so no route can forget: the whole point of [onUnauthorized] is
  /// that expiry is noticed wherever it happens, not only on the routes someone
  /// remembered to annotate.
  CortexApiException _failure(int status, String message) {
    if (status == 401 && onUnauthorized != null) {
      // Deferred: the listener drops the credential, which rebuilds
      // `cortexApiProvider` and disposes *this* instance — including the HTTP
      // client whose response we are still holding. Letting that happen a
      // microtask later keeps the unwind on this call stack ordinary.
      scheduleMicrotask(onUnauthorized!);
    }
    return CortexApiException(message, statusCode: status);
  }

  @override
  Future<HealthStatus> health() async =>
      HealthStatus.fromJson(await _getJson('/health'));

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

  // ---------------------------------------------------------------- projects

  @override
  Future<List<Project>> projects() async {
    final json = await _getJson('/projects');
    return asObjectList(
      json['projects'],
    ).map(Project.fromJson).toList(growable: false);
  }

  @override
  Future<Project> createProject(String name) async =>
      Project.fromJson(await _postJson('/projects', {'name': name}));

  @override
  Future<Project> renameProject(String id, String name) async =>
      Project.fromJson(
        await _patchJson('/projects/${Uri.encodeComponent(id)}', {
          'name': name,
        }),
      );

  @override
  Future<void> deleteProject(String id) async {
    final http.Response response;
    try {
      response = await _client.delete(
        _uri('/projects/${Uri.encodeComponent(id)}'),
        headers: _headers(const {'accept': 'application/json'}),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(response.statusCode, _errorMessage(response));
    }
    // 回的是 `{}`，没有可读的东西 —— 不解析，省得哪天服务端改成 204
    // 就要在这里炸一次
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
  Future<MemorySearchResult> searchMemory(
    String query, {
    int limit = 20,
    DateTime? asOf,
  }) async {
    final json = await _getJson('/memory/search', {
      'q': query,
      'limit': '$limit',
      // RFC 3339 in UTC — the server compares against transaction time.
      if (asOf != null) 'as_of': asOf.toUtc().toIso8601String(),
    });
    return MemorySearchResult.fromJson(json);
  }

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
  Future<AuthTokens> login(String username, String password) =>
      _postAuth('/auth/login', {'username': username, 'password': password});

  @override
  Future<AuthTokens> refreshSession(String refreshToken) =>
      _postAuth('/auth/refresh', {'refresh_token': refreshToken});

  @override
  Future<Account?> whoAmI() async {
    try {
      return Account.fromJson(await _getJson('/auth/me'));
    } on CortexApiException catch (e) {
      // 老服务端没有这个端点、或者这个部署压根没有账号体系。
      // 两种都只意味着「没有名字可显示」—— 账号栏照常在，
      // 它还挂着设置与退出登录。
      if (e.isMissing || e.statusCode == 501) return null;
      rethrow;
    }
  }

  @override
  Future<void> logout(String refreshToken) async {
    try {
      await _client.post(
        _uri('/auth/logout'),
        headers: _headers(const {'content-type': 'application/json'}),
        body: jsonEncode({'refresh_token': refreshToken}),
      );
    } on Object catch (_) {
      // 登出失败不该拦住用户离开。本地那份已经会被清掉，而服务端那条链
      // 最多再活到过期 —— 把人卡在「退不出去」的界面上更糟
    }
  }

  /// 三条账号端点形状一样：POST JSON，回 AuthTokens。
  ///
  /// **不带 Authorization 头**：它们在服务端是免认证的公开端点
  /// （登录时本来就还没有凭据），带一个过期的 access token 反而会让
  /// 请求被中间层拦下。
  Future<AuthTokens> _postAuth(String path, Map<String, Object?> body) async {
    final http.Response response;
    try {
      response = await _client.post(
        _uri(path),
        headers: const {
          'content-type': 'application/json',
          'accept': 'application/json',
        },
        body: jsonEncode(body),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(response.statusCode, _errorMessage(response));
    }
    return AuthTokens.fromJson(
      _decodeObject(path, utf8.decode(response.bodyBytes)),
    );
  }

  @override
  Future<LlmKeyStatus> llmKeyStatus() => _llmKey('GET', null);

  @override
  Future<LlmKeyStatus> setLlmKey({
    required String provider,
    required String apiKey,
    String? baseUrl,
  }) => _llmKey('PUT', {
    'provider': provider,
    'api_key': apiKey,
    if (baseUrl != null && baseUrl.trim().isNotEmpty)
      'base_url': baseUrl.trim(),
  });

  @override
  Future<LlmKeyStatus> clearLlmKey() => _llmKey('DELETE', null);

  /// 三个动作共用一条路径，只有方法与请求体不同。
  ///
  /// 合成一个是因为**错误处理必须逐字相同**：三处各写一遍的话，
  /// 迟早有一处忘了检查状态码，然后把一段错误 JSON 当成状态解析，
  /// 界面上显示「未配置」—— 而实际是存失败了。
  Future<LlmKeyStatus> _llmKey(
    String method,
    Map<String, Object?>? body,
  ) async {
    final uri = _uri('/settings/llm-key');
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
      throw _failure(response.statusCode, _errorMessage(response));
    }
    return LlmKeyStatus.fromJson(
      _decodeObject('settings/llm-key', utf8.decode(response.bodyBytes)),
    );
  }

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
      throw _failure(response.statusCode, _errorMessage(response));
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
      throw _failure(response.statusCode, _errorMessage(response));
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
        body.isEmpty ? 'import/run 失败' : _trim(body),
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
      );
    }
    if (response.statusCode >= 400) {
      // The validator's wording ("整台机器不是工作区") is written for the user.
      throw _failure(response.statusCode, _errorMessage(response));
    }
    final body = _decodeObject(
      'PUT /local/workspaces',
      utf8.decode(response.bodyBytes),
    );
    return asStringOrNull(body['workspace']);
  }

  @override
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    String? workspace,
    bool clearWorkspace = false,
  }) async {
    // The tri-state lives here: an *absent* key means "leave it", an explicit
    // `null` means "unbind". `jsonEncode` emits both correctly, but only if the
    // key is added conditionally rather than always with a nullable value.
    final body = <String, dynamic>{
      'title': ?title,
      'archived': ?archived,
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
      );
    }
    return ChatSession.fromJson(
      _decodeObject('PATCH /sessions', utf8.decode(response.bodyBytes)),
    );
  }

  /// Unwraps `ErrorBody { error }` so the daemon's own wording reaches the user
  /// instead of a JSON blob. The workspace validator in particular writes its
  /// rejections to be read ("整台机器不是工作区"), and re-phrasing them here
  /// would throw that away.
  String _errorMessage(http.Response response) {
    try {
      final decoded = jsonDecode(utf8.decode(response.bodyBytes));
      if (decoded is Map<String, dynamic>) {
        final message = asStringOrNull(decoded['error']);
        if (message != null) return message;
      }
    } on Object {
      // Not JSON. Fall through to the raw body.
    }
    return _trim(response.body);
  }

  @override
  Future<Episode> episode(String id) async =>
      Episode.fromJson(await _getJson('/episodes/${Uri.encodeComponent(id)}'));

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
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
      });

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
        body.isEmpty ? 'POST /chat 失败' : _trim(body),
      );
    }

    await for (final frame in decodeSse(response.stream)) {
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
  }) async {
    try {
      await _postJson('/confirmations', {
        // In the body, never the path. `POST /confirmations/{token}` would be
        // tidier REST and would write a credential that approves shell
        // execution into every access log on the way.
        'token': token,
        // A two-value enum rather than a bool: `{"approve":"false"}` decodes as
        // `true` in a surprising number of client libraries, and the direction
        // that mistake fails in is "ran a command nobody approved".
        'decision': allow ? 'allow' : 'deny',
      });
      return true;
    } on CortexApiException catch (e) {
      // The ordinary outcome of losing the race. Not routed through
      // `isUnsupported` even though that getter also matches 404 — here the
      // meaning is "this token is spent", not "this daemon lacks the route".
      if (e.statusCode == 404) return false;
      rethrow;
    }
  }

  @override
  Future<SyncPage> sync({required int since, int limit = 500}) async {
    final json = await _getJson('/sync', {
      'since': '$since',
      'limit': '$limit',
    });
    return SyncPage.fromJson(json);
  }

  @override
  Future<AuthTicket> issueTicket() async {
    // **No request body**, not even `{}`.
    //
    // The handler takes no input, so axum never reads the body; hyper then
    // closes the connection instead of returning it to the keep-alive pool.
    // `IOClient` does not learn of that and hands the dead socket to the next
    // request, which fails with a bare "Write failed" — observed as the *second*
    // ticket request dying while the first succeeded. Sending nothing keeps the
    // connection reusable, which matters here precisely because this endpoint is
    // called again on every reconnect.
    final json = await _postJson('/auth/ticket', null);
    return AuthTicket(
      value: asString(json['ticket']),
      expiresAt: DateTime.now().add(
        Duration(seconds: asInt(json['expires_in_secs'], 60)),
      ),
    );
  }

  /// A ticket that is valid now, minting one only when the cached one is not.
  ///
  /// Concurrent callers share one in-flight request. Without that, a reconnect
  /// storm (daemon restart → every tab retries at once) would mint a fresh
  /// ticket per attempt, and the daemon's ticket book only prunes on issue —
  /// so the wasteful path is also the one that grows the table.
  ///
  /// Returns null when there is no token to trade: against a daemon with
  /// `CORTEX_AUTH=disabled` the socket needs no credential at all, and asking
  /// for one would fail a connection that would otherwise have worked.
  Future<String?> _currentTicket() async {
    if (_token == null) return null;
    final cached = _ticket;
    if (cached != null && cached.isUsableAt(DateTime.now())) {
      return cached.value;
    }

    final pending = _ticketInFlight ??= issueTicket();
    try {
      final fresh = await pending;
      _ticket = fresh;
      return fresh.value;
    } finally {
      _ticketInFlight = null;
    }
  }

  @override
  Stream<SyncEvent> watchSync() async* {
    // Minted *before* the handshake rather than lazily on 401: a WebSocket
    // rejected at the HTTP upgrade gives back a bare "connection failed" with
    // no status code to inspect, so there would be nothing to retry *on*.
    final String? ticket;
    try {
      ticket = await _currentTicket();
    } on CortexApiException catch (e) {
      // Includes the 401 that says the long-lived token itself is bad, which
      // `_failure` has already reported upward. Surfaced as a link failure so
      // `SyncController` backs off instead of hot-looping while the login gate
      // takes over.
      throw CortexApiException('换取实时同步票据失败：${e.message}', cause: e);
    }

    final WebSocketChannel channel;
    try {
      channel = WebSocketChannel.connect(_wsUri(ticket));
      // `connect` is lazy — without awaiting `ready` a failed handshake would
      // only surface later, as an error on the message stream, and the caller
      // could not tell "never connected" from "dropped after 3 hours".
      await channel.ready;
    } on Object catch (e) {
      throw CortexApiException(_wsUnreachableMessage(e), cause: e);
    }

    // 为什么不是一句 `await for (final frame in channel.stream)`
    //
    // 那样写，**取消这条订阅会挂死**。`await for` 在被取消时要先取消它对
    // `channel.stream` 的内层订阅，而 WebSocket 的取消要走完关闭握手 ——
    // 等服务端回一帧 close。可读侧此刻正在被撤掉，那一帧永远读不到：
    // `cancel()` 等内层，内层等一帧永远不来的数据。
    //
    // 挂住的不是别的路径，正是**切后端与退登录**：两处都要先断开实时同步
    // 再往下走。症状是点一下没反应，而且没有任何报错。
    //
    // 改成自己持有内层订阅、由 `onCancel` 决定怎么收尾：两件清理都发出去
    // 但都不等 —— 关闭是纯清理，没有任何人需要它完成的那一刻。
    final events = StreamController<SyncEvent>();
    final frames = channel.stream.listen(
      (frame) {
        if (frame is! String) return; // the daemon only sends text frames
        final Object? decoded;
        try {
          decoded = jsonDecode(frame);
        } on FormatException {
          // One malformed frame must not tear down a healthy link; the next
          // bump will carry us forward anyway.
          return;
        }
        if (decoded is Map<String, dynamic>) {
          events.add(SyncEvent.fromJson(decoded));
        }
      },
      // An abnormal close arrives as an error. Normalise it so the caller only
      // ever has to handle one error type.
      onError: (Object e) =>
          events.addError(CortexApiException('实时同步通道断开：$e', cause: e)),
      onDone: events.close,
      cancelOnError: true,
    );
    events.onCancel = () {
      unawaited(frames.cancel());
      unawaited(channel.sink.close());
    };

    yield* events.stream;
  }

  // ------------------------------------------------------------------- blobs

  @override
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    final request = _ProgressRequest('POST', _uri('/blobs'), bytes, onProgress)
      ..headers.addAll(
        _headers({
          'accept': 'application/json',
          // The server sniffs the byte header and only falls back to this.
          'content-type': mime ?? 'application/octet-stream',
        }),
      );

    final json = await _sendJson(request, 'POST /blobs');
    return BlobRef.fromJson(json);
  }

  @override
  Future<BlobPresign> presignBlob(String hash) async =>
      BlobPresign.fromJson(await _postJson('/blobs/presign', {'hash': hash}));

  @override
  Future<BlobRef> commitBlob({
    required String hash,
    required int sizeBytes,
    String? mime,
  }) async => BlobRef.fromJson(
    await _postJson('/blobs/commit', {
      'hash': hash,
      'size_bytes': sizeBytes,
      'mime': ?mime,
    }),
  );

  /// Straight to the object store. Deliberately **not** routed through
  /// [_uri] — the whole point of a presigned URL is that these bytes never
  /// reach cortexd.
  ///
  /// On Web this needs the bucket's own CORS policy to allow `PUT` from the
  /// page origin; cortexd's permissive CORS layer has no say over it.
  @override
  Future<void> putPresigned({
    required String url,
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    final request = _ProgressRequest('PUT', Uri.parse(url), bytes, onProgress);
    if (mime != null) request.headers['content-type'] = mime;

    final http.StreamedResponse response;
    try {
      response = await _client.send(request);
    } on Object catch (e) {
      throw CortexApiException('直传对象存储失败：$e', cause: e);
    }
    final body = await response.stream.bytesToString().catchError((_) => '');
    if (response.statusCode >= 400) {
      throw CortexApiException(
        body.isEmpty ? '直传返回 ${response.statusCode}' : _trim(body),
        statusCode: response.statusCode,
      );
    }
  }

  @override
  Future<Uint8List> blobBytes(String hash) async {
    final http.Response response;
    try {
      // A plain authenticated GET: this client renders attachments from bytes
      // it fetched itself, never from `Image.network`. That is why the ticket
      // is not needed here even though the daemon's docs list `<img>` among the
      // things it exists for — the widget layer already goes through
      // `CortexApi.blobBytes` so the mock source has something to answer with,
      // and the header path falls out of that for free.
      response = await _client.get(
        _uri('/blobs/${Uri.encodeComponent(hash)}'),
        headers: _headers(),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(response.statusCode, _trim(response.body));
    }
    return response.bodyBytes;
  }

  @override
  Future<Uint8List> sandboxWorkspaceTar() =>
      _sandboxBytes('/sandbox/workspace.tar');

  @override
  Future<Uint8List> sandboxReadFile(String path) =>
      _sandboxBytes('/sandbox/files/raw', {'path': path});

  /// 两条「回字节」的沙箱路由共用一份错误处理。
  ///
  /// 关键在 [_errorMessage] 而不是 `_trim(response.body)`：后者按
  /// content-type 的 charset 解码，而 axum 的 `Json` 不带 charset，
  /// `package:http` 于是回退到 **latin1** —— 服务端那句「文件还在卷里，
  /// 先发条消息把它拉起来」会变成一串乱码，恰好是最需要看清的那一句。
  /// 顺带把 `{"error": …}` 的外壳剥掉，用户不该读到 JSON。
  Future<Uint8List> _sandboxBytes(
    String path, [
    Map<String, String>? query,
  ]) async {
    final http.Response response;
    try {
      response = await _client.get(_uri(path, query), headers: _headers());
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(response.statusCode, _errorMessage(response));
    }
    return response.bodyBytes;
  }

  @override
  Future<List<FileNode>> sandboxListFiles(String path) async {
    final json = await _getJson('/sandbox/files', {'path': path});
    // 子节点的绝对路径拿**服务端回的** path 去拼，不是请求里那个：服务端会
    // 规范化（消掉多余的斜杠、结尾的 `/`），用请求里那份拼出来的路径，
    // 下一次展开送回去的就是一条服务端没见过的字符串
    final dir = asString(json['path'], path);
    return asObjectList(json['entries'])
        .map((e) {
          final name = asString(e['name']);
          final isDir = e['is_dir'] == true;
          return FileNode(
            name: name,
            path: posixJoin(dir, name),
            isDirectory: isDir,
            // 目录的 size 服务端固定给 0，那不是「空目录」的意思 —— 传 null，
            // 界面因此不会在目录行后面画一个 0 B
            sizeBytes: isDir ? null : asInt(e['size']),
            // Unix 秒。服务端给 null 表示 tar 里没有这个字段 ——
            // **不要**回落到 0，那会显示成 1970 年
            modifiedAt: switch (e['mtime']) {
              final int s when s > 0 => DateTime.fromMillisecondsSinceEpoch(
                s * 1000,
                isUtc: true,
              ).toLocal(),
              _ => null,
            },
          );
        })
        .toList(growable: false);
  }

  @override
  Future<SandboxWriteReceipt> sandboxWriteFile({
    required String path,
    required Uint8List bytes,
    UploadProgress? onProgress,
  }) async {
    // 复用 `uploadBlob` 那套分块请求：一个 `http.Request` 只有 0% 与 done
    // 两个状态，而工作区里塞进来的常常是几十 MB 的数据集。
    // Web 上进度的语义见 `_ProgressRequest` 的文档（会先冲到 100% 再等）
    final request = _ProgressRequest(
      'PUT',
      // 路径在 query，字节在 body。反过来（多段表单）要额外一层编码，
      // 而这条路由两边都只认原始字节
      _uri('/sandbox/files', {'path': path}),
      bytes,
      onProgress,
    )..headers.addAll(
      _headers(const {
        'content-type': 'application/octet-stream',
        'accept': 'application/json',
      }),
    );

    final http.Response response;
    try {
      response = await http.Response.fromStream(await _client.send(request));
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(response.statusCode, _errorMessage(response));
    }
    final json = _decodeObject(
      'PUT /sandbox/files',
      utf8.decode(response.bodyBytes),
    );
    return (path: asString(json['path'], path), size: asInt(json['size']));
  }

  // ----------------------------------------------------------------- plumbing

  /// A JSON POST. [body] null means "send nothing at all" — see [issueTicket]
  /// for why that is not the same as sending `{}`.
  Future<Map<String, dynamic>> _postJson(
    String path,
    Map<String, dynamic>? body,
  ) async {
    final http.Response response;
    try {
      response = await _client.post(
        _uri(path),
        headers: _headers({
          if (body != null) 'content-type': 'application/json',
          'accept': 'application/json',
        }),
        body: body == null ? null : jsonEncode(body),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(response.statusCode, _errorMessage(response));
    }
    return _decodeObject(path, utf8.decode(response.bodyBytes));
  }

  /// A JSON PATCH.
  ///
  /// [updateSession] deliberately does **not** go through here: it rewrites the
  /// message for 404/405 to name the missing route, and that wording is what the
  /// session sidebar shows when it degrades to a local-only edit. Folding the two
  /// together would either lose that message or force it onto every caller.
  Future<Map<String, dynamic>> _patchJson(
    String path,
    Map<String, dynamic> body,
  ) async {
    final http.Response response;
    try {
      response = await _client.patch(
        _uri(path),
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
      throw _failure(response.statusCode, _errorMessage(response));
    }
    return _decodeObject(path, utf8.decode(response.bodyBytes));
  }

  Future<Map<String, dynamic>> _sendJson(
    http.BaseRequest request,
    String what,
  ) async {
    final http.StreamedResponse response;
    try {
      response = await _client.send(request);
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    final body = await response.stream.bytesToString().catchError((_) => '');
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        body.isEmpty ? '$what 失败' : _trim(body),
      );
    }
    return _decodeObject(what, body);
  }

  Future<Map<String, dynamic>> _getJson(
    String path, [
    Map<String, String>? query,
  ]) async {
    final http.Response response;
    try {
      response = await _client.get(
        _uri(path, query),
        headers: _headers(const {'accept': 'application/json'}),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }

    if (response.statusCode >= 400) {
      throw _failure(response.statusCode, _errorMessage(response));
    }
    return _decodeObject(path, utf8.decode(response.bodyBytes));
  }

  /// The ticket is stripped from the reported URL — this string reaches logs
  /// and the status indicator's tooltip, and keeping a credential out of URLs
  /// is pointless if the client then prints it.
  String _wsUnreachableMessage(Object e) => '连不上实时同步通道（${_wsUri(null)}）。$e';

  String _unreachableMessage(Object e) =>
      '连不上 cortexd（$_base）。确认 daemon 已启动，'
      '或在设置里切到 Mock 数据源。\n$e';

  /// Decodes a JSON object body, turning "this is not JSON at all" into a
  /// message that names the likely cause.
  ///
  /// ## Why this exists
  ///
  /// `jsonDecode` on an HTML body throws a bare
  /// `FormatException: Unexpected character (at character 1) <!DOCTYPE html>`.
  /// That reached the login screen verbatim — and it is the **single most
  /// likely first-run failure**, because the obvious thing to type is the
  /// address you visit in a browser, while the API lives one path segment
  /// deeper (`https://host` → the web UI, `https://host/api` → cortexd).
  ///
  /// The exception was technically accurate and told the user nothing about
  /// what to do.
  Map<String, dynamic> _decodeObject(String what, String body) {
    final Object? decoded;
    try {
      decoded = jsonDecode(body);
    } on FormatException {
      throw CortexApiException(_notJsonMessage(what, body));
    }
    if (decoded is! Map<String, dynamic>) {
      throw CortexApiException('$what 返回了非对象 JSON');
    }
    return decoded;
  }

  String _notJsonMessage(String what, String body) {
    final head = body.trimLeft();
    if (head.startsWith('<')) {
      // An origin with no path is the classic "I typed the browser URL" case;
      // suggest the concrete address rather than describing the shape of the
      // problem. With a path already present the guess would be worse than
      // useless, so say what was observed and stop.
      final bare = _base.path.replaceAll('/', '').isEmpty;
      final hint = bare
          ? '自托管部署通常把 cortexd 挂在 `/api` 下 —— 试试 $_base/api。'
          : '确认这个地址指向 cortexd 本身，而不是 Web 界面或反代的默认站点。';
      return '$what：这个地址返回的是**网页**，不是 API。$hint';
    }
    return '$what：返回的不是 JSON。\n${_trim(body)}';
  }

  static String _trim(String s) =>
      s.length <= 400 ? s : '${s.substring(0, 400)}…';

  @override
  void dispose() => _client.close();
}

/// A request whose body is fed out in chunks so upload progress is observable.
///
/// `http.Request` hands its body over as one `Uint8List`, which gives exactly
/// two progress states: 0% and done. That is fine for a JSON POST and useless
/// for a 20 MB image, so the body is emitted here as 64 KiB chunks with a
/// callback after each one.
///
/// ## What the number means on each platform
///
/// * **Desktop** — `IOClient` writes each chunk to the socket as it is pulled,
///   so progress tracks bytes actually handed to the OS. Close enough to "on
///   the wire" to be worth showing.
/// * **Web** — `FetchClient` is configured with `streamRequests: false`
///   (streaming request bodies need HTTP/2 and are unsupported in Firefox and
///   Safari), so the browser drains this stream into a buffer *first* and
///   uploads afterwards. Progress therefore races to 100% and then the request
///   sits there while the actual upload happens.
///
/// That asymmetry is why the composer shows an indeterminate bar once progress
/// reaches 100% but the future has not completed — the honest reading of
/// "handed over, outcome unknown" rather than a bar that lies at 100%.
class _ProgressRequest extends http.BaseRequest {
  _ProgressRequest(super.method, super.url, this._bytes, this._onProgress) {
    // Set rather than overridden: `BaseRequest` exposes a settable
    // `contentLength`, and overriding the getter alone would leave the
    // inherited setter with a mismatched type.
    contentLength = _bytes.length;
  }

  final Uint8List _bytes;
  final UploadProgress? _onProgress;

  static const _chunk = 64 * 1024;

  @override
  http.ByteStream finalize() {
    super.finalize();
    return http.ByteStream(_emit());
  }

  Stream<List<int>> _emit() async* {
    if (_bytes.isEmpty) {
      _onProgress?.call(0, 0);
      return;
    }
    var sent = 0;
    while (sent < _bytes.length) {
      final end = math.min(sent + _chunk, _bytes.length);
      // A view, not a copy — `sublist` here would double peak memory on a file
      // that is already large enough to be worth a progress bar.
      yield Uint8List.sublistView(_bytes, sent, end);
      sent = end;
      _onProgress?.call(sent, _bytes.length);
    }
  }
}
