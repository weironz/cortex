/// Every failure surfaced by [CortexApi] is normalised into this type so the
/// UI never has to pattern-match on platform-specific socket/JS exceptions
/// (which differ between dart:io and the browser fetch stack).
class CortexApiException implements Exception {
  const CortexApiException(this.message, {this.statusCode, this.cause});

  final String message;
  final int? statusCode;
  final Object? cause;

  /// True when the request never reached the server (daemon down, wrong port,
  /// CORS refusal). Worth a distinct hint in the UI: "start cortexd or switch
  /// to mock".
  bool get isUnreachable => statusCode == null;

  @override
  String toString() =>
      statusCode == null ? message : 'HTTP $statusCode · $message';
}
