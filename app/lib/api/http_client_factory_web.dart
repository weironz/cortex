import 'package:fetch_client/fetch_client.dart';
import 'package:http/http.dart' as http;

/// Web implementation, built on the browser `fetch` API.
///
/// Two settings matter here:
///
/// * [RequestMode.cors] — `fetch_client` defaults to `no-cors`, which yields an
///   *opaque* response: status 0, unreadable body. Since the Flutter web app is
///   served from a different origin than `cortexd` (`:8080`) during dev, we
///   must ask for a real CORS response. `cortexd` already enables
///   `tower-http`'s CORS layer.
/// * `streamRequests: false` — streaming **request** bodies requires HTTP/2 and
///   is unsupported in Firefox/Safari. We only need a streaming *response*, and
///   that works everywhere `fetch` does.
http.Client createHttpClient() => FetchClient(
  mode: RequestMode.cors,
  credentials: RequestCredentials.omit,
  streamRequests: false,
);

/// Browsers may buffer a response until some bytes have arrived; servers
/// normally solve this by sending an SSE comment immediately. Flagged here so
/// the UI can explain a stalled first token rather than looking frozen.
const bool kNeedsSseKeepAlive = true;

const String kHttpClientKind = 'browser fetch (FetchClient)';
