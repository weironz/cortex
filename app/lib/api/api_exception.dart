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
  ///
  /// **Not usable on `POST /confirmations`.** There a 404 is the ordinary
  /// outcome of losing a race (another device answered first, or the turn timed
  /// out), and reading it as "this daemon has no confirmation endpoint" would
  /// turn the most common benign case into a scary one. That call checks
  /// [statusCode] directly; see `ConfirmController.answer`.
  bool get isUnsupported =>
      statusCode == 501 || statusCode == 405 || statusCode == 404;

  /// Plain 404 — "the thing you named is not there".
  ///
  /// Every endpoint that addresses a single resource by id has this second
  /// reading of 404, and it collides with [isUnsupported]: `DELETE
  /// /projects/{id}` answers 404 both when the daemon has no projects feature
  /// at all and when that one project was already deleted from another device.
  /// Callers on an item-scoped endpoint must tell the two apart — treating "it
  /// is already gone" as "this feature does not exist" tears down a whole
  /// section of the UI over a duplicate delete.
  ///
  /// `POST /confirmations` hit this first and checked [statusCode] inline; it
  /// is a named test now so the next one does not have to rediscover it.
  bool get isMissing => statusCode == 404;

  /// The daemon requires a credential we did not present, or rejected the one
  /// we did.
  ///
  /// `cortexd::auth::require` deliberately does not distinguish "no token" from
  /// "wrong token" — telling them apart would hand a brute-forcer a progress
  /// bar — so neither does this. The client's reaction is the same either way:
  /// drop back to the login gate rather than showing an empty pane or spinning
  /// forever.
  bool get isUnauthorized => statusCode == 401;

  @override
  String toString() =>
      statusCode == null ? message : 'HTTP $statusCode · $message';
}
