import '../core/permission_mode.dart';
import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/foundation.dart' show visibleForTesting;
import 'package:http/http.dart' as http;
import 'package:web_socket_channel/web_socket_channel.dart';

import '../import/import_source.dart';
import '../models/account.dart';
import '../models/auth_tokens.dart';
import '../models/library_item.dart';
import '../models/model_source.dart';
import '../models/search_prefs.dart';
import '../models/model_role.dart';
import '../models/mcp.dart';
import '../models/assistant.dart';
import '../models/agent_presence.dart';
import '../models/attachment.dart';
import '../models/blob.dart';
import '../models/image_prefs.dart';
import '../models/generated_image.dart';
import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/import_plan.dart';
import '../models/json.dart';
import '../models/pending_confirmation.dart';
import '../models/project.dart';
import '../models/skill.dart';
import '../models/sandbox_health.dart';
import '../models/session_detail.dart';
import '../models/model_option.dart';
import '../models/session_search_hit.dart';
import '../models/usage_report.dart';
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
    this.onLocalAgentRejected,
    this.frontsDeployment,
  }) : _base = _normalise(baseUrl),
       _token = (token != null && token.trim().isEmpty) ? null : token?.trim(),
       _client = client ?? createHttpClient();

  final Uri _base;
  final http.Client _client;

  /// `baseUrl` 是桌面端**自己拉起的本机 agent** 时，它前面挡着的那个部署。
  ///
  /// `null` = 直接打用户配的地址（Web、或者这台机器没有 agent）。
  ///
  /// # 它只为一句错误信息存在，而那句话此前一直在骗人
  ///
  /// 本机 agent 绑的是**内核随机分的端口**，用户从没配过它。连不上时
  /// 原来那句「连不上 cortexd（http://127.0.0.1:9826）。确认 daemon 已启动，
  /// 或在设置里切到 Mock 数据源」有三处不对：那不是 cortexd、用户没有
  /// 「daemon」可启动、切 Mock 也解决不了。
  ///
  /// 2026-08-20 实测的现场：设置页显示 `https://…/api`，而报错说
  /// `127.0.0.1:9826` —— 用户读出来的是「串台了，我配的地址没生效」。
  final String? frontsDeployment;

  /// The long-lived bearer credential, or null against a daemon running with
  /// `CORTEX_AUTH=disabled`.
  ///
  /// Immutable for the life of the instance: a token change means a *different*
  /// identity, and `cortexApiProvider` rebuilds the whole client for it. Making
  /// it settable would leave in-flight requests straddling two identities and
  /// would keep a stale token alive inside closures.
  final String? _token;

  /// 本机 agent **自己**拒了请求（401 且带 `x-cortex-denied-by: local-agent`）。
  ///
  /// 与 [onUnauthorized] 是两种截然不同的处境：这个说的是**入站凭据错位**
  /// （重启 agent 能治），那个说的是**远端不认用户凭据**（重启 agent 永远
  /// 治不了）。2026-08-21 之前两者共用一个回调，客户端把远端 401 也当成
  /// 本机错位去重启 agent —— 凭据在服务端失效后，1 秒一次的轮询把 agent
  /// 杀了 639+ 次，用户看到的是「总是连不上 agent、看不到会话」。
  final void Function()? onLocalAgentRejected;

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

  /// 空地址 = **同源**（浏览器里），不是「没配」。
  ///
  /// # 这里踩过一次
  ///
  /// 上一版是 `if (s.isEmpty) s = 'http://127.0.0.1:8080'`。于是
  /// `just dev` 那套拓扑（浏览器 → nginx:5173 → cortexd，同源反代）整个
  /// 走不通：`--dart-define=CORTEX_BASE_URL=` 传的空串**是有意义的配置**，
  /// 却在这里被换成了另一个源，浏览器随即因为 CORS 把响应丢掉，报的是
  /// 「Failed to fetch」—— 一句看起来像 daemon 没起来的错。
  ///
  /// 这是本仓库第 7 次「空串顶掉默认值」，但方向反过来：前六次是空串被当成
  /// 「配过了」，这次是一个**刻意的空串**被当成「没配过」。
  ///
  /// # 判据是「有没有页面源」，不是 `kIsWeb`
  ///
  /// `Uri.base` 在浏览器里是当前页面的 URL，在桌面端是进程工作目录
  /// （`file:` 协议）。桌面端确实没有同源可言，那儿的空串才真是「没配」，
  /// 回落到本机 daemon 的默认端口是对的。用协议判**正好**分开这两件事，
  /// 而且不用把平台判断散到 API 层（见 `core/local_agent.dart` 的同一立场）。
  static Uri _normalise(String raw) => resolveBase(raw, Uri.base);

  /// [_normalise] 的纯函数版。`page` 就是 `Uri.base`。
  ///
  /// 提出来只为一件事：**能测同源那一支**。`Uri.base` 在测试进程里恒为
  /// `file:`，直接测 `_normalise` 只能验到桌面端的回落 —— 而出问题的
  /// 恰恰是另一支。
  @visibleForTesting
  static Uri resolveBase(String raw, Uri page) {
    var s = raw.trim();
    if (s.isEmpty) {
      if (page.scheme == 'http' || page.scheme == 'https') {
        // 只保留 scheme + host + port。页面路径与查询串不属于 API 根。
        //
        // **重新构造，不能用 `replace(query: null, fragment: null)`** ——
        // `Uri.replace` 里的 null 是「这一项不动」而不是「清空」，于是
        // `…/app/?x=1#frag` 的查询串与锚点会原样留在 API 根上，
        // 之后每个请求都带着它们。
        return Uri(
          scheme: page.scheme,
          host: page.host,
          port: page.hasPort ? page.port : null,
        );
      }
      s = 'http://127.0.0.1:8080';
    }
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
  CortexApiException _failure(
    int status,
    String message, {
    Map<String, String>? headers,
    bool? retryable,
  }) {
    if (status == 401) {
      // 按「谁拒的」分铃。头是 agent 侧打的（routes.rs 的 require_auth），
      // package:http 会把响应头的键统一成小写。没带头 = 远端拒的
      // （反代回来的响应不会自称 local-agent）
      final bell = headers?['x-cortex-denied-by'] == 'local-agent'
          ? onLocalAgentRejected
          : onUnauthorized;
      if (bell != null) {
        // Deferred: the listener drops the credential, which rebuilds
        // `cortexApiProvider` and disposes *this* instance — including the
        // HTTP client whose response we are still holding. Letting that happen
        // a microtask later keeps the unwind on this call stack ordinary.
        scheduleMicrotask(bell);
      }
    }
    return CortexApiException(
      message,
      statusCode: status,
      retryable: retryable,
    );
  }

  @override
  Future<HealthStatus> health() async =>
      HealthStatus.fromJson(await _getJson('/health'));

  /// `GET /sandbox/health`。**不抛** —— 见接口上那段文档。
  ///
  /// 刻意不走 [_getJson]：那条路把每个 >= 400 都翻成异常，而这里
  /// 「404」与「502」恰恰是两个**要显示出来的不同答案**。
  @override
  Future<SandboxHealth> sandboxHealth() async {
    final http.Response response;
    try {
      response = await _client.get(
        _uri('/sandbox/health'),
        headers: _headers(const {'accept': 'application/json'}),
      );
    } on Object catch (e) {
      // 连不上这个地址本身。`/health` 会同时红，而那一条说得清楚得多 ——
      // 这里只说「问不出来」，不再复述一遍「连不上 cortexd」
      return SandboxHealth.blocked('探测不到 /sandbox/health：$e');
    }

    // 404/405 = 这个地址上没有 agent 编排服务。**不是故障**：自托管与
    // 纯本机部署本来就没有它。
    //
    // 401 也走这里而不是敲 [onUnauthorized] 的门：这条路由在服务端是
    // 公开的，收到 401 只说明前面挡着一层我们不认识的东西 —— 拿它去
    // 判定「凭据过期」会把用户从一次健康探测里踢出登录态。
    if (response.statusCode == 404 ||
        response.statusCode == 405 ||
        response.statusCode == 401) {
      return SandboxHealth.absent;
    }
    if (response.statusCode >= 400) {
      // 502/503/504 是网关在说「我认得这条路，但后面那个进程没起来」。
      // 那与「压根没有沙箱」是两件事，一起吞成 absent 会让一次真正的
      // 宕机看起来像一个正常的自托管形态
      return SandboxHealth.blocked(_errorMessage(response));
    }

    final Object? decoded;
    try {
      decoded = jsonDecode(utf8.decode(response.bodyBytes));
    } on FormatException {
      // 200 + 一张网页：nginx 对认不出的路径做 SPA 回落。CLAUDE.md 专门
      // 记了这个假信号 —— 「回 200 + index.html，看起来像成功」
      return SandboxHealth.absent;
    }
    if (decoded is! Map<String, dynamic>) return SandboxHealth.absent;
    return SandboxHealth.fromJson(decoded);
  }

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
  Future<ModelCatalog> llmModels() async =>
      ModelCatalog.fromJson(await _getJson('/llm/models'));

  @override
  Future<UsageReport> usage() async =>
      UsageReport.fromJson(await _getJson('/auth/usage'));

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
  Future<List<Assistant>> assistants() async {
    final body = await _getJson('/assistants');
    return asObjectList(
      body['assistants'],
    ).map(Assistant.fromJson).toList(growable: false);
  }

  @override
  Future<Assistant> createAssistant(Assistant draft) async =>
      Assistant.fromJson(
        await _postJson('/assistants', {
          'name': draft.name,
          'description': draft.description,
          'instructions': draft.instructions,
          'icon': draft.icon,
          'model': ?draft.model,
          'source': ?draft.source,
        }),
      );

  @override
  Future<Assistant> updateAssistant(
    String id, {
    String? name,
    String? description,
    String? instructions,
    String? icon,
    List<String>? disabledTools,
  }) async => Assistant.fromJson(
    await _patchJson('/assistants/${Uri.encodeComponent(id)}', {
      'name': ?name,
      'description': ?description,
      'instructions': ?instructions,
      'icon': ?icon,
      'disabled_tools': ?disabledTools,
    }),
  );

  @override
  Future<void> deleteAssistant(String id) =>
      _noContent('DELETE', '/assistants/${Uri.encodeComponent(id)}');

  @override
  Future<List<Skill>> skills() async {
    final body = await _getJson('/skills');
    return asObjectList(
      body['skills'],
    ).map(Skill.fromJson).toList(growable: false);
  }

  @override
  Future<Skill> createSkill(Skill draft) async => Skill.fromJson(
    await _postJson('/skills', {
      'name': draft.name,
      'description': draft.description,
      'instructions': draft.instructions,
    }),
  );

  @override
  Future<Skill> updateSkill(
    String id, {
    String? name,
    String? description,
    String? instructions,
    bool? enabled,
  }) async => Skill.fromJson(
    await _patchJson('/skills/${Uri.encodeComponent(id)}', {
      'name': ?name,
      'description': ?description,
      'instructions': ?instructions,
      'enabled': ?enabled,
    }),
  );

  @override
  Future<void> deleteSkill(String id) =>
      _noContent('DELETE', '/skills/${Uri.encodeComponent(id)}');

  @override
  Future<Project> patchProject(String id, {String? name, bool? pinned}) async =>
      Project.fromJson(
        await _patchJson('/projects/${Uri.encodeComponent(id)}', {
          'name': ?name,
          'pinned': ?pinned,
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
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
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
  Future<AuthTokens> login(String username, String password) =>
      _postAuth('/auth/login', {'username': username, 'password': password});

  @override
  Future<AuthTokens> register(String username, String password) =>
      _postAuth('/auth/register', {'username': username, 'password': password});

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
  Future<Profile> profile() async =>
      Profile.fromJson(await _getJson('/auth/profile'));

  @override
  Future<Profile> updateProfile({Patch<String?>? nickname}) async {
    // **只放进这次真的要改的字段。** 字段缺席 = 服务端不动它；
    // 无脑把所有字段都发出去的话，一次「只改昵称」会把别的字段一起重写成
    // 界面上那份可能已经过期的副本
    final body = <String, dynamic>{
      if (nickname != null) 'nickname': nickname.value,
    };
    return Profile.fromJson(await _patchJson('/auth/profile', body));
  }

  @override
  Future<Profile> putAvatar(Uint8List bytes) async {
    // body 是原始字节，不是 multipart：这条路只上传一个文件、没有别的字段，
    // multipart 会多一层双方都要维护的解析
    final req = http.Request('PUT', _uri('/auth/avatar'))
      ..headers.addAll(_headers({'content-type': 'application/octet-stream'}))
      ..bodyBytes = bytes;
    return Profile.fromJson(await _sendJson(req, '上传头像'));
  }

  @override
  Future<Profile> deleteAvatar() async =>
      Profile.fromJson(await _deleteJson('/auth/avatar'));

  @override
  Future<Uint8List> avatarBytes(String userId) async {
    // 与 `blobBytes` 同一条路：自己取字节、自己渲染。不用 `Image.network`
    // ——它带不了 Authorization，而 mock 数据源也答不出一个 URL
    final http.Response response;
    try {
      response = await _client.get(
        _uri('/auth/avatar/${Uri.encodeComponent(userId)}'),
        headers: _headers(),
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        _trim(response.body),
        headers: response.headers,
      );
    }
    return response.bodyBytes;
  }

  @override
  Future<void> startEmailBinding(String email) async {
    await _postJson('/auth/email/start', {'email': email});
  }

  @override
  Future<Profile> verifyEmail(String code) async =>
      Profile.fromJson(await _postJson('/auth/email/verify', {'code': code}));

  @override
  Future<Profile> unbindEmail(String password) async {
    // DELETE 带 body —— `_deleteJson` 不带，走 `_sendJson`
    final req = http.Request('DELETE', _uri('/auth/email'))
      ..headers.addAll(_headers({'content-type': 'application/json'}))
      ..body = jsonEncode({'password': password});
    return Profile.fromJson(await _sendJson(req, '解绑邮箱'));
  }

  @override
  Future<void> changePassword(String oldPassword, String newPassword) async {
    await _postJson('/auth/password', {
      'old_password': oldPassword,
      'new_password': newPassword,
    });
  }

  @override
  Future<Profile> deleteAccount(String password) async {
    // DELETE 带 body —— `_deleteJson` 不带，所以走 `_sendJson`
    final req = http.Request('DELETE', _uri('/auth/account'))
      ..headers.addAll(_headers({'content-type': 'application/json'}))
      ..body = jsonEncode({'password': password});
    return Profile.fromJson(await _sendJson(req, '删除账号'));
  }

  @override
  Future<Profile> restoreAccount() async =>
      Profile.fromJson(await _postJson('/auth/account/restore', const {}));

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
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    return AuthTokens.fromJson(
      _decodeObject(path, utf8.decode(response.bodyBytes)),
    );
  }

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
            _statusFallback(response.statusCode, 'import/run'),
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

  /// Unwraps `ErrorBody { error }` so the daemon's own wording reaches the user
  /// instead of a JSON blob. The workspace validator in particular writes its
  /// rejections to be read ("整台机器不是工作区"), and re-phrasing them here
  /// would throw that away.
  /// 一次失败该给用户看的话。
  ///
  /// # 空 body 必须自己编一句，不能就这么回空串
  ///
  /// 有些失败**根本没有 body**：axum 的路由 fallback 回的 404 就是
  /// `content-length: 0`。回空串的后果是界面画出一个**只有图标、没有一个字**
  /// 的红框 —— 用户知道出错了，但没有任何线索。真机上撞到过：地址填成
  /// `http://127.0.0.1:8080/api`（cortexd 的路由在根上，没有 `/api` 前缀），
  /// 于是 `/api/health` 404、body 为空、红框全空。
  ///
  /// 编的那句话要**带上状态码与路径**：这两样正好是「地址填错了」与
  /// 「服务端挂了」之间唯一的区别。
  String _errorMessage(http.Response response) {
    final unwrapped = _unwrapError(utf8.decode(response.bodyBytes));
    if (unwrapped != null) return unwrapped;

    return _statusFallback(response.statusCode, response.request?.url.path);
  }

  /// 没有可用 body 时，按状态码编一句**给人看**的话。
  ///
  /// 只有状态码和路径可用时，这两样正好能分开几种完全不同的处境 ——
  /// 而把它们并成一句「请求失败」，用户就只能来问我们。
  static String _statusFallback(int status, String? path) {
    final where = (path == null || path.isEmpty) ? '' : '（$path）';
    return switch (status) {
      // 404 且没有 body，几乎总是「这个地址上没有服务」而不是
      // 「这个资源不存在」—— 真正的资源 404 由服务端带上 {"error": …}
      404 =>
        '$path 在这个地址上不存在（HTTP 404）。'
            '服务端的路由挂在根上，地址里不要带 /api 之类的前缀。',
      // 502/503/504 是**网关**在说话：它自己活着，但它要转发的那个上游没起来。
      // 这三个码到不了应用层，所以永远不会有 {"error": …} —— 由这里兜底。
      //
      // 点名说「上游」而不是笼统的「服务端错误」：真机上这一条对应的
      // 就是记忆服务没起，而那时用户能做的事很具体（去把它起起来）
      502 || 503 || 504 =>
        '网关连不上它要转发的服务（HTTP $status$where）。'
            '多半是那个上游没起来 —— 部署入口本身是通的，否则这里会是连接被拒。',
      final s => '服务端回了 HTTP $s，而且没有说明原因$where。',
    };
  }

  /// 从响应体里取出**给人看的那句话**：`{"error": "…"}` 就剥掉信封，
  /// 是纯文本就用原文；拿不到时回 `null`（调用方去编一句）。
  ///
  /// # 为什么非要有这一步
  ///
  /// 服务端一律用 `{"error": …}` 包一层。流式那条路上原先直接把整个 body
  /// 当成消息，于是聊天气泡里画出来的是
  ///
  /// ```
  /// {"error":"这一轮要在云端跑，但数据源 … 请把数据源改成部署入口。"}
  /// ```
  ///
  /// —— 一句本来写得清清楚楚的提示，因为多了一层信封就变成了「报错很不友好」。
  /// 2026-08-15 真机上撞到的。非流式那条一直是对的，这个函数把两条并回一处，
  /// 免得下一个人只修其中一条。
  ///
  /// # HTML 一律扔掉
  ///
  /// 这里原先写着「非 JSON 的 body 原样保留 —— nginx 的 502 页面之类，
  /// 那些原文同样是线索」。**那句话是错的**，同一天就被打脸：记忆服务停掉
  /// 之后，会话列表那一栏里画出来的是一整张
  ///
  /// ```
  /// <html><head><title>502 Bad Gateway</title></head><body>…
  /// <!-- a padding to disable MSIE and Chrome friendly error page -->
  /// ```
  ///
  /// —— 连那几行给 IE 凑字数的注释都照单全收。**一个网关的 HTML 错误页对
  /// 用户不是线索，是噪音**：它唯一有用的信息（502）已经在状态码里了，
  /// 而调用方按状态码编的那句话比它清楚得多。
  /// 错误体里那一位「重发有没有用」（`ErrorBody.retryable`）。
  ///
  /// 没有这一位就是 `null` —— 绝大多数错误都是，界面照旧给出路。
  /// **只认服务端说的**：客户端自己按状态码或文案猜是那种「改一个字就
  /// 静默失效」的判据，见 [CortexApiException.retryable]。
  bool? _unwrapRetryable(String body) {
    try {
      final decoded = jsonDecode(body);
      if (decoded is Map<String, dynamic>) return decoded['retryable'] as bool?;
    } on Object {
      // 不是 JSON —— 那就没有这一位
    }
    return null;
  }

  String? _unwrapError(String body) {
    try {
      final decoded = jsonDecode(body);
      if (decoded is Map<String, dynamic>) {
        final message = asStringOrNull(decoded['error']);
        if (message != null && message.trim().isNotEmpty) return message;
      }
    } on Object {
      // 不是 JSON。可能是纯文本（那往往就是给人看的），也可能是一张 HTML
      // 错误页 —— 下面那一行把后者挡掉
    }
    // 判 `<` 而不判 content-type：反代与网关在这条路上未必给对头部，
    // 而「body 以尖括号开头」对 HTML / XML 错误页是够用且不会误伤的判据
    // （真正给人看的错误文案不会以 `<` 开头）
    if (body.trimLeft().startsWith('<')) return null;
    final trimmed = _trim(body);
    return trimmed.isEmpty ? null : trimmed;
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
        _unwrapError(body) ?? _statusFallback(response.statusCode, label),
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
      throw _failure(
        response.statusCode,
        _trim(response.body),
        headers: response.headers,
      );
    }
    return response.bodyBytes;
  }

  @override
  Future<BlobUrl> blobUrl(String hash) async => BlobUrl.fromJson(
    await _getJson('/blobs/${Uri.encodeComponent(hash)}/url'),
  );

  @override
  Future<List<String>> generateImages({
    required String prompt,
    String? model,
    String? source,
    String? size,
    int n = 1,
  }) async {
    final body = await _postJson('/llm/image', {
      'prompt': prompt,
      // 三个都用 `?`：null 就整个键不出现，而服务端那侧「键不出现」
      // 正是「你替我挑」—— 传 null 进去会被当成一个显式的空值
      'model': ?model,
      'source': ?source,
      'size': ?size,
      'n': n,
      // `session_id` 刻意**不传**：从图片页画的图不属于任何一条会话，
      // 画廊里那一列为 NULL 就是这个意思。对话里画的由 agent 那条路带
    });
    return [
      for (final m in (body['images'] as List? ?? const []))
        if (m is Map && m['hash'] is String) m['hash'] as String,
    ];
  }

  @override
  Future<Gallery> gallery({
    int limit = 30,
    String? before,
    String? folder,
    String? hash,
  }) async => Gallery.fromJson(
    await _getJson('/images', {
      'limit': '$limit',
      'before': ?before,
      'folder': ?folder,
      'hash': ?hash,
    }),
  );

  @override
  Future<String> shareImage(String id) async => asString(
    (await _postJson('/images/${Uri.encodeComponent(id)}/share', null))['url'],
  );

  @override
  Future<void> unshareImage(String id) =>
      _noContent('DELETE', '/images/${Uri.encodeComponent(id)}/share');

  @override
  Future<void> removeImage(String id) =>
      _noContent('DELETE', '/images/${Uri.encodeComponent(id)}');

  @override
  Future<Folders> folders() async =>
      Folders.fromJson(await _getJson('/folders'));

  @override
  Future<LibraryPage> library({
    int limit = 60,
    String? before,
    String? folder,
    String? tab,
    String? sort,
  }) async => LibraryPage.fromJson(
    await _getJson('/library', {
      'limit': '$limit',
      'before': ?before,
      'folder': ?folder,
      'tab': ?tab,
      'sort': ?sort,
    }),
  );

  @override
  Future<LibraryItem> addToLibrary({
    required String blobHash,
    required String name,
    String? origin,
    String? folderId,
  }) async => LibraryItem.fromJson(
    await _postJson('/library', {
      'blob_hash': blobHash,
      'name': name,
      'origin': ?origin,
      'folder_id': ?folderId,
    }),
  );

  @override
  Future<LibraryItem> updateLibraryItem(
    String id, {
    String? name,
    String? folderId,
    bool moveFolder = false,
  }) async => LibraryItem.fromJson(
    await _patchJson('/library/${Uri.encodeComponent(id)}', {
      'name': ?name,
      // ⚠️ 只有真要动归属时才带这个键。带一个 null 与不带是两回事：
      // 服务端把「带了 null」读成「移出文件夹」，而一次只改名字的请求
      // 顺手把归档清掉，用户看到的是文件自己从文件夹里跑出来了
      if (moveFolder) 'folder_id': folderId,
    }),
  );

  @override
  Future<void> removeFromLibrary(String id) =>
      _noContent('DELETE', '/library/${Uri.encodeComponent(id)}');

  @override
  Future<Folders> createFolder(String name) async =>
      Folders.fromJson(await _postJson('/folders', {'name': name}));

  @override
  Future<Folders> renameFolder(String id, String name) async =>
      Folders.fromJson(
        await _patchJson('/folders/${Uri.encodeComponent(id)}', {'name': name}),
      );

  @override
  Future<Folders> deleteFolder(String id) async => Folders.fromJson(
    await _deleteJson('/folders/${Uri.encodeComponent(id)}'),
  );

  @override
  Future<void> moveImage(String id, String? folderId) => _noContent(
    'PATCH',
    '/images/${Uri.encodeComponent(id)}',
    // 显式带 null 而不是省掉这个键：省掉的话服务端读成「没说要动归属」，
    // 于是「移出文件夹」这个动作静默地什么都不做
    {'folder_id': folderId},
  );

  @override
  Future<Uint8List> sandboxWorkspaceTar({String? sessionId}) =>
      _sandboxBytes('/sandbox/workspace.tar', _scoped(null, sessionId));

  @override
  Future<Uint8List> exportSessions() => _sandboxBytes('/sessions/export');

  @override
  Future<Uint8List> sandboxReadFile(String path, {String? sessionId}) =>
      _sandboxBytes('/sandbox/files/raw', _scoped(path, sessionId));

  /// 文件端点的查询串。**`session` 决定读写的是哪个项目的工作区。**
  ///
  /// 服务端拿它查这个会话属于哪个项目，再据此选容器与卷（见
  /// `SandboxScope::key`）。不传的话服务端按「未分组」算 —— 那是一个
  /// 合法但通常不是用户想看的工作区，症状是「文件树里空空如也」。
  Map<String, String> _scoped(String? path, String? sessionId) => {
    'path': ?path,
    'session': ?sessionId,
  };

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
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    return response.bodyBytes;
  }

  @override
  Future<List<FileNode>> sandboxListFiles(
    String path, {
    String? sessionId,
  }) async {
    final json = await _getJson('/sandbox/files', _scoped(path, sessionId));
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
    String? sessionId,
  }) async {
    // 复用 `uploadBlob` 那套分块请求：一个 `http.Request` 只有 0% 与 done
    // 两个状态，而工作区里塞进来的常常是几十 MB 的数据集。
    // Web 上进度的语义见 `_ProgressRequest` 的文档（会先冲到 100% 再等）
    final request =
        _ProgressRequest(
            'PUT',
            // 路径在 query，字节在 body。反过来（多段表单）要额外一层编码，
            // 而这条路由两边都只认原始字节
            _uri('/sandbox/files', _scoped(path, sessionId)),
            bytes,
            onProgress,
          )
          ..headers.addAll(
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
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
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
  /// DELETE 且**要回体** —— 删文件夹会把整份新清单带回来，
  /// 客户端不必自己从本地列表里抠掉一条（抠错的症状是界面与服务端不一致）。
  Future<Map<String, dynamic>> _deleteJson(String path) async {
    final http.Response response;
    try {
      response = await _client.delete(
        _uri(path),
        headers: _headers(const {'accept': 'application/json'}),
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
    return _decodeObject(path, utf8.decode(response.bodyBytes));
  }

  Future<Map<String, dynamic>> _postJson(
    String path,
    Map<String, dynamic>? body, [
    Map<String, String>? query,
  ]) async {
    final http.Response response;
    try {
      response = await _client.post(
        _uri(path, query),
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
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
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
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
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
        headers: response.headers,
      );
    }
    return _decodeObject(what, body);
  }

  /// 一条**不回内容**的请求（204）。
  ///
  /// 单开一个而不是复用 `_postJson`：那几个都要 `_decodeObject`，
  /// 而 204 的正文是空的 —— 解它只会抛一个与真正问题无关的 JSON 错误。
  Future<void> _noContent(
    String method,
    String path, [
    Map<String, dynamic>? body,
  ]) async {
    final request = http.Request(method, _uri(path))
      ..headers.addAll(
        _headers(
          body == null ? const {} : const {'content-type': 'application/json'},
        ),
      );
    if (body != null) request.body = jsonEncode(body);
    final http.StreamedResponse response;
    try {
      response = await _client.send(request);
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    final text = await response.stream.bytesToString().catchError((_) => '');
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        text.isEmpty ? '$method $path 失败' : _trim(text),
        headers: response.headers,
      );
    }
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
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    return _decodeObject(path, utf8.decode(response.bodyBytes));
  }

  /// The ticket is stripped from the reported URL — this string reaches logs
  /// and the status indicator's tooltip, and keeping a credential out of URLs
  /// is pointless if the client then prints it.
  String _wsUnreachableMessage(Object e) => '连不上实时同步通道（${_wsUri(null)}）。$e';

  /// 连不上时那句话。**先说清楚这个地址是谁。**
  ///
  /// 打的是本机 agent 时，那个端口是桌面端自己拉起的进程、内核随机分的，
  /// 用户从没配过它。照搬「确认 daemon 已启动」会让他去找一个根本不存在
  /// 的东西 —— 而他看到一个陌生的 127.0.0.1 端口，第一反应是
  /// 「串台了，我配的地址没生效」。2026-08-20 就是这么被问的。
  String _unreachableMessage(Object e) {
    if (frontsDeployment case final remote?) {
      return '连不上本机 agent（$_base）—— 那是桌面端自己拉起的进程，'
          '不是你配的地址；你配的部署是 $remote。\n'
          '它多半刚退出（会自动重启，稍等一下）。一直这样就重启一次应用。\n$e';
    }
    return '连不上 cortexd（$_base）。确认 daemon 已启动，'
        '或在设置里切到 Mock 数据源。\n$e';
  }

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
