import 'json.dart';

/// An append-only archived conversation turn — the provenance target every
/// memory fact points back at (`GET /episodes/{id}`).
class Episode {
  const Episode({
    required this.id,
    required this.sessionId,
    required this.role,
    required this.text,
    this.occurredAt,
  });

  final String id;
  final String sessionId;

  /// `user` | `assistant` | `system` (kept as a string: the server owns this
  /// vocabulary and may grow it, and we only ever display it).
  final String role;

  final String text;
  final DateTime? occurredAt;

  factory Episode.fromJson(Map<String, dynamic> json) => Episode(
    id: asString(json['id']),
    sessionId: asString(json['session_id']),
    role: asString(json['role'], 'user'),
    text: asString(json['text']),
    occurredAt: asDateOrNull(json['occurred_at']),
  );
}
