import 'json.dart';

/// One frame of the `GET /ws` down-stream.
///
/// Wire contract (`crates/cortexd/src/dto.rs::SyncEvent`):
///
/// * `{"type":"hello","cursor":13,"version":"0.0.1"}`
/// * `{"type":"bump","cursor":14}`
/// * `{"type":"resync","cursor":14}`
///
/// ## The `cursor` here is a signal, not a fetch offset
///
/// The daemon deliberately pushes *notifications only*, never rows: two
/// serialisation paths for the same data would drift, and a disconnected client
/// has to catch up from its own cursor anyway. So `cursor` on these events only
/// answers "how far behind am I" — the actual catch-up must be
/// `GET /sync?since=<our own cursor>`.
///
/// Using the event's cursor as the next `since` would silently skip the whole
/// range between what we have already pulled and what the server just reached.
/// That is why `SyncController` keeps its own cursor and never assigns from
/// here after the first hello.
sealed class SyncEvent {
  const SyncEvent();

  /// The server's cursor as of this event. Display / lag only.
  int get cursor;

  factory SyncEvent.fromJson(Map<String, dynamic> json) {
    return switch (asString(json['type'])) {
      'hello' => SyncHello(
        cursor: asInt(json['cursor']),
        version: asStringOrNull(json['version']),
      ),
      'bump' => SyncBump(asInt(json['cursor'])),
      'resync' => SyncResync(asInt(json['cursor'])),
      final other => SyncUnknown(other, asInt(json['cursor'])),
    };
  }
}

/// Connection established. Carries the server's current cursor so a client can
/// tell immediately how far behind it is.
final class SyncHello extends SyncEvent {
  const SyncHello({required this.cursor, this.version});

  @override
  final int cursor;

  /// `cortex_core::VERSION` of the daemon on the other end.
  final String? version;
}

/// Normal increment: something was committed, go pull from your own cursor.
final class SyncBump extends SyncEvent {
  const SyncBump(this.cursor);

  @override
  final int cursor;
}

/// Same action as [SyncBump], but the daemon admits a bump may have been lost
/// (its Postgres notification channel dropped, or this connection fell behind
/// the broadcast buffer).
///
/// Counted separately on purpose — a healthy deployment shows ~0 of these, so a
/// rising count is an operations signal rather than a client bug.
final class SyncResync extends SyncEvent {
  const SyncResync(this.cursor);

  @override
  final int cursor;
}

/// Forward compatibility: a newer daemon event type must not kill the link.
final class SyncUnknown extends SyncEvent {
  const SyncUnknown(this.type, this.cursor);

  final String type;

  @override
  final int cursor;
}
