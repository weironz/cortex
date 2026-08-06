/// The single platform seam in the app.
///
/// Everything above this file (API impl, controllers, widgets) is platform
/// agnostic and just asks for an `http.Client`; only the two implementations
/// behind this conditional export know what platform they are on. This is why
/// there is no `if (kIsWeb)` anywhere in `lib/features/` or `lib/widgets/`.
///
/// Why it matters: on web, `package:http`'s default `BrowserClient` is built on
/// `XMLHttpRequest` and buffers the **entire** response before completing —
/// `StreamedResponse.stream` therefore emits once, at the end. That silently
/// turns SSE into "wait for the whole answer, then show it", i.e. the streaming
/// UI dies on web only. `fetch_client` implements the same `http.Client`
/// interface on top of the browser `fetch` API, whose `ReadableStream` body is
/// genuinely incremental, so `POST /chat` streams on web exactly as it does on
/// Windows.
library;

export 'http_client_factory_io.dart'
    if (dart.library.js_interop) 'http_client_factory_web.dart';
