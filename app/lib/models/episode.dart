import 'attachment.dart';
import 'injected_memory.dart';
import 'json.dart';
import 'tool_call.dart';

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
    this.memories = const [],
    this.toolCalls = const [],
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

  /// What was injected into this turn's prompt — the replayed contents of the
  /// "why do you remember that" drawer.
  ///
  /// Anchored on the **user** episode server-side (`episode_memories.episode_id`
  /// is the user turn), because the assistant episode does not exist when the
  /// model errors. The client moves it onto the answer for display — see
  /// `ChatController`.
  ///
  /// Omitted entirely rather than sent as `[]` when empty: most messages have
  /// none, and a few hundred `"memories":[]` per session is pure waste.
  final List<InjectedMemory> memories;

  /// Tools invoked during this turn. Same anchoring and same omission rule as
  /// [memories]. One entry per invocation — unlike the SSE path, replay does
  /// not need pairing.
  final List<ToolCall> toolCalls;

  factory Episode.fromJson(Map<String, dynamic> json) => Episode(
    id: asString(json['id']),
    sessionId: asString(json['session_id']),
    role: asString(json['role'], 'user'),
    text: asString(json['text']),
    occurredAt: asDateOrNull(json['occurred_at']),
    attachments: asObjectList(
      json['attachments'],
    ).map(Attachment.fromJson).toList(growable: false),
    memories: asObjectList(
      json['memories'],
    ).map(InjectedMemory.fromJson).toList(growable: false),
    toolCalls: asObjectList(
      json['tool_calls'],
    ).map(ToolCall.replayed).toList(growable: false),
  );
}
