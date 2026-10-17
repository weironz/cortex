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

part 'http_cortex_api/core.dart';
part 'http_cortex_api/chat.dart';
part 'http_cortex_api/sessions.dart';
part 'http_cortex_api/projects.dart';
part 'http_cortex_api/assistants.dart';
part 'http_cortex_api/auth.dart';
part 'http_cortex_api/workspace.dart';
part 'http_cortex_api/mcp.dart';
part 'http_cortex_api/models.dart';
part 'http_cortex_api/import.dart';
part 'http_cortex_api/blobs.dart';
part 'http_cortex_api/images.dart';
part 'http_cortex_api/library.dart';
part 'http_cortex_api/sandbox.dart';
part 'http_cortex_api/sync.dart';

/// Talks to a real `cortexd` over HTTP + SSE.
class HttpCortexApi extends HttpCortexApiBase
    with
        _CoreEndpoints,
        _ChatEndpoints,
        _SessionsEndpoints,
        _ProjectsEndpoints,
        _AssistantsEndpoints,
        _SkillsEndpoints,
        _AuthEndpoints,
        _WorkspaceEndpoints,
        _McpEndpoints,
        _ModelsEndpoints,
        _ImportEndpoints,
        _BlobsEndpoints,
        _ImagesEndpoints,
        _LibraryEndpoints,
        _SandboxEndpoints,
        _SyncEndpoints {
  HttpCortexApi({
    required super.baseUrl,
    super.token,
    super.client,
    super.onUnauthorized,
    super.onLocalAgentRejected,
    super.frontsDeployment,
  });

  // statics are not inherited: tests grab the pure-function form here
  @visibleForTesting
  static Uri resolveBase(String raw, Uri page) =>
      HttpCortexApiBase.resolveBase(raw, page);
}

abstract class HttpCortexApiBase implements CortexApi {
  HttpCortexApiBase({
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
