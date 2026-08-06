import 'package:http/http.dart' as http;

/// Native (Windows/macOS/Linux/mobile) implementation.
///
/// `http.Client()` resolves to `IOClient`, which already exposes the response
/// body as a genuinely incremental `Stream<List<int>>` — nothing special
/// needed for SSE here.
http.Client createHttpClient() => http.Client();

/// Native sockets keep an idle SSE connection open indefinitely, so no
/// keep-alive workaround is required.
const bool kNeedsSseKeepAlive = false;

const String kHttpClientKind = 'dart:io (IOClient)';
