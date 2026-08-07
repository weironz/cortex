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

  /// The daemon understood the request and answered "I do not offer this".
  ///
  /// Kept distinct from a transport failure because the two demand opposite
  /// reactions: an unreachable daemon should be **retried**, an unsupported
  /// endpoint must **not** be — retrying a 501 is a loop on a road that will
  /// never open. Callers use it to fall back (presign → relay upload) or to
  /// degrade to a local-only change (rename with no `PATCH /sessions/{id}`).
  ///
  /// 404 counts: a route that was never registered answers 404, and from the
  /// client's side that is indistinguishable from — and has the same remedy as
  /// — an explicit 501.
  bool get isUnsupported =>
      statusCode == 501 || statusCode == 405 || statusCode == 404;

  @override
  String toString() =>
      statusCode == null ? message : 'HTTP $statusCode · $message';
}
