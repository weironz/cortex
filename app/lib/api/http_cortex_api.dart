import 'dart:async';
import 'dart:convert';

import 'package:http/http.dart' as http;

import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/json.dart';
import '../models/memory_search_result.dart';
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

  @override
  String get label => '$_base · $kHttpClientKind';

  @override
  Future<HealthStatus> health() async =>
      HealthStatus.fromJson(await _getJson('/health'));

  @override
  Future<List<ChatSession>> sessions() async {
    final json = await _getJson('/sessions');
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
  Future<Episode> episode(String id) async =>
      Episode.fromJson(await _getJson('/episodes/${Uri.encodeComponent(id)}'));

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
  }) async* {
    final request = http.Request('POST', _uri('/chat'))
      ..headers.addAll({
        'content-type': 'application/json',
        'accept': 'text/event-stream',
        // Defeats any proxy that would otherwise buffer the whole response and
        // collapse the stream into a single chunk.
        'cache-control': 'no-cache',
      })
      ..body = jsonEncode({'session_id': sessionId, 'message': message});

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

  String _unreachableMessage(Object e) =>
      '连不上 cortexd（$_base）。确认 daemon 已启动，'
      '或在设置里切到 Mock 数据源。\n$e';

  static String _trim(String s) =>
      s.length <= 400 ? s : '${s.substring(0, 400)}…';

  @override
  void dispose() => _client.close();
}
