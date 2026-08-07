import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:http/http.dart' as http;
import 'package:web_socket_channel/web_socket_channel.dart';

import '../models/attachment.dart';
import '../models/blob.dart';
import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/json.dart';
import '../models/memory_search_result.dart';
import '../models/session_detail.dart';
import '../models/sync_event.dart';
import '../models/sync_record.dart';
import 'api_exception.dart';
import 'cortex_api.dart';
import 'http_client_factory.dart';
import 'sse.dart';

/// Talks to a real `cortexd` over HTTP + SSE.
class HttpCortexApi implements CortexApi {
  HttpCortexApi({required String baseUrl, http.Client? client})
    : _base = _normalise(baseUrl),
      _client = client ?? createHttpClient();

  final Uri _base;
  final http.Client _client;

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
  Uri get _wsUri =>
      _uri('/ws').replace(scheme: _base.scheme == 'https' ? 'wss' : 'ws');

  @override
  String get label => '$_base · $kHttpClientKind';

  @override
  Future<HealthStatus> health() async =>
      HealthStatus.fromJson(await _getJson('/health'));

  @override
  Future<List<ChatSession>> sessions({bool includeArchived = false}) async {
    final json = await _getJson('/sessions', {
      if (includeArchived) 'include_archived': 'true',
    });
    return asObjectList(json['sessions'])
        .map(ChatSession.fromJson)
        .toList(growable: false);
  }

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
      throw CortexApiException(
        // A daemon that predates the route answers 405 (the path exists, but
        // only for GET) with an empty body. Anything else — notably the
        // workspace validator's 400 — carries a message written for the user,
        // so it is passed through untouched.
        response.statusCode == 405 || response.statusCode == 404
            ? 'cortexd 还没有 PATCH /sessions/{id}，改动只在本地生效。'
            : _errorMessage(response),
        statusCode: response.statusCode,
      );
    }
    final decoded = jsonDecode(utf8.decode(response.bodyBytes));
    if (decoded is! Map<String, dynamic>) {
      throw const CortexApiException('PATCH /sessions 返回了非对象 JSON');
    }
    return ChatSession.fromJson(decoded);
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
  }) async* {
    final request = http.Request('POST', _uri('/chat'))
      ..headers.addAll({
        'content-type': 'application/json',
        'accept': 'text/event-stream',
        // Defeats any proxy that would otherwise buffer the whole response and
        // collapse the stream into a single chunk.
        'cache-control': 'no-cache',
      })
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
      });

    final http.StreamedResponse response;
    try {
      response = await _client.send(request);
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }

    if (response.statusCode != 200) {
      final body = await response.stream.bytesToString().catchError((_) => '');
      throw CortexApiException(
        body.isEmpty ? 'POST /chat 失败' : _trim(body),
        statusCode: response.statusCode,
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

  @override
  Future<SyncPage> sync({required int since, int limit = 500}) async {
    final json = await _getJson('/sync', {
      'since': '$since',
      'limit': '$limit',
    });
    return SyncPage.fromJson(json);
  }

  @override
  Stream<SyncEvent> watchSync() async* {
    final WebSocketChannel channel;
    try {
      channel = WebSocketChannel.connect(_wsUri);
      // `connect` is lazy — without awaiting `ready` a failed handshake would
      // only surface later, as an error on the message stream, and the caller
      // could not tell "never connected" from "dropped after 3 hours".
      await channel.ready;
    } on Object catch (e) {
      throw CortexApiException(_wsUnreachableMessage(e), cause: e);
    }

    try {
      await for (final frame in channel.stream) {
        if (frame is! String) continue; // the daemon only sends text frames
        final Object? decoded;
        try {
          decoded = jsonDecode(frame);
        } on FormatException {
          // One malformed frame must not tear down a healthy link; the next
          // bump will carry us forward anyway.
          continue;
        }
        if (decoded is Map<String, dynamic>) yield SyncEvent.fromJson(decoded);
      }
    } on Object catch (e) {
      // An abnormal close arrives as an exception. Normalise it so the caller
      // only ever has to handle one error type.
      throw CortexApiException('实时同步通道断开：$e', cause: e);
    } finally {
      // Runs on consumer cancellation too, which is how the reconnect loop
      // guarantees it never leaves a half-open socket behind.
      await channel.sink.close();
    }
  }

  // ------------------------------------------------------------------- blobs

  @override
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    final request = _ProgressRequest(
      'POST',
      _uri('/blobs'),
      bytes,
      onProgress,
    )..headers.addAll({
      'accept': 'application/json',
      // The server sniffs the byte header and only falls back to this.
      'content-type': mime ?? 'application/octet-stream',
    });

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
    final request = _ProgressRequest(
      'PUT',
      Uri.parse(url),
      bytes,
      onProgress,
    );
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
      response = await _client.get(_uri('/blobs/${Uri.encodeComponent(hash)}'));
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }
    if (response.statusCode >= 400) {
      throw CortexApiException(
        _trim(response.body),
        statusCode: response.statusCode,
      );
    }
    return response.bodyBytes;
  }

  // ----------------------------------------------------------------- plumbing

  Future<Map<String, dynamic>> _postJson(
    String path,
    Map<String, dynamic> body,
  ) async {
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
      throw CortexApiException(
        _trim(response.body),
        statusCode: response.statusCode,
      );
    }
    final decoded = jsonDecode(utf8.decode(response.bodyBytes));
    if (decoded is! Map<String, dynamic>) {
      throw CortexApiException('$path 返回了非对象 JSON');
    }
    return decoded;
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
      throw CortexApiException(
        body.isEmpty ? '$what 失败' : _trim(body),
        statusCode: response.statusCode,
      );
    }
    final decoded = jsonDecode(body);
    if (decoded is! Map<String, dynamic>) {
      throw CortexApiException('$what 返回了非对象 JSON');
    }
    return decoded;
  }

  Future<Map<String, dynamic>> _getJson(
    String path, [
    Map<String, String>? query,
  ]) async {
    final http.Response response;
    try {
      response = await _client.get(
        _uri(path, query),
        headers: const {'accept': 'application/json'},
      );
    } on Object catch (e) {
      throw CortexApiException(_unreachableMessage(e), cause: e);
    }

    if (response.statusCode >= 400) {
      throw CortexApiException(
        _trim(response.body),
        statusCode: response.statusCode,
      );
    }
    final decoded = jsonDecode(utf8.decode(response.bodyBytes));
    if (decoded is! Map<String, dynamic>) {
      throw CortexApiException('$path 返回了非对象 JSON');
    }
    return decoded;
  }

  String _wsUnreachableMessage(Object e) =>
      '连不上实时同步通道（$_wsUri）。$e';

  String _unreachableMessage(Object e) =>
      '连不上 cortexd（$_base）。确认 daemon 已启动，'
      '或在设置里切到 Mock 数据源。\n$e';

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
