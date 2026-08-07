import 'chat_session.dart';
import 'episode.dart';
import 'json.dart';

/// `GET /sessions/{id}` — the session overview plus **every** episode in it.
///
/// ## There is no cursor here, and that shapes the client
///
/// `SessionDetail` is flat: the server flattens `SessionDto` next to a single
/// `episodes` array and offers no `before` / `limit` / `after` parameters. So
/// the client cannot page the *request*; a 4000-turn session arrives as one
/// JSON document or not at all.
///
/// What the client can still do is page the *render*, which is what
/// `ChatController.openSession` does: it keeps the whole decoded transcript in
/// memory and reveals it from the newest end in windows, growing the window
/// when the user scrolls to the top. That fixes the cost that actually hurts —
/// markdown parsing and layout per bubble — while leaving the transfer cost
/// where the contract put it.
///
/// Worse, the server-side cap is `LIMIT 500` over `ORDER BY occurred_at ASC`,
/// so a longer session comes back **oldest-first and truncated at the end** —
/// the user would be shown the start of a conversation whose latest turns are
/// missing, with nothing on screen saying so. [truncated] exists to say so.
///
/// The missing piece is server-side: a cursor
/// (`GET /sessions/{id}?before=…&limit=N`, newest-first). Until it exists,
/// "分页" on this screen is honestly only half a feature, and the README
/// says which half.
class SessionDetail {
  const SessionDetail({required this.session, required this.episodes});

  final ChatSession session;

  /// Chronological, oldest first — the order the server returns.
  final List<Episode> episodes;

  /// The daemon returned fewer episodes than the session claims to hold, i.e.
  /// the `LIMIT 500` bit. The transcript renders a banner instead of quietly
  /// showing a conversation with its ending cut off.
  bool get truncated => session.messageCount > episodes.length;

  factory SessionDetail.fromJson(Map<String, dynamic> json) => SessionDetail(
    // The session fields are flattened into the same object.
    session: ChatSession.fromJson(json),
    episodes: asObjectList(
      json['episodes'],
    ).map(Episode.fromJson).toList(growable: false),
  );
}
