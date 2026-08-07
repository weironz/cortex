import 'attachment.dart';
import 'json.dart';

/// An append-only archived conversation turn — the provenance target every
/// memory fact points back at (`GET /episodes/{id}`), and the unit
/// `GET /sessions/{id}` replays a transcript from.
class Episode {
  const Episode({
    required this.id,
    required this.sessionId,
    required this.role,
    required this.text,
    this.occurredAt,
    this.attachments = const [],
  });

  final String id;
  final String sessionId;

  /// `user` | `assistant` | `system` (kept as a string: the server owns this
  /// vocabulary and may grow it, and we only ever display it).
  final String role;

  final String text;
  final DateTime? occurredAt;

  /// Blobs hanging off this turn. The server sends `[]` rather than omitting
  /// the key, so the client never has to tell "no attachments" apart from
  /// "this server build does not report attachments".
  final List<Attachment> attachments;

  factory Episode.fromJson(Map<String, dynamic> json) => Episode(
    id: asString(json['id']),
    sessionId: asString(json['session_id']),
    role: asString(json['role'], 'user'),
    text: asString(json['text']),
    occurredAt: asDateOrNull(json['occurred_at']),
    attachments: asObjectList(
      json['attachments'],
    ).map(Attachment.fromJson).toList(growable: false),
  );
}
