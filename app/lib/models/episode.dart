import 'attachment.dart';
import 'json.dart';
import 'tool_call.dart';
import 'turn_block.dart';

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
    this.toolCalls = const [],
    this.blocks = const [],
    this.models = const [],
  });

  final String id;
  final String sessionId;

  /// `user` | `assistant` | `system` (kept as a string: the server owns this
  /// vocabulary and may grow it, and we only ever display it).
  final String role;

  final String text;
  final DateTime? occurredAt;

  /// 这条回复**先后**是谁写的，按发生顺序。空 = 不知道
  /// （迁移之前的历史、导入的记录）—— 界面什么都不画。
  final List<String> models;

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

  /// Tools invoked during this turn. Same anchoring and same omission rule as
  /// [attachments]. One entry per invocation — unlike the SSE path, replay does
  /// not need pairing.
  final List<ToolCall> toolCalls;

  /// 正文与工具的先后骨架。空 = 不知道（老服务端、导入的历史、
  /// 加这一位之前落库的会话）—— 那时界面退回从前的画法。
  final List<TurnBlock> blocks;

  factory Episode.fromJson(Map<String, dynamic> json) => Episode(
    id: asString(json['id']),
    sessionId: asString(json['session_id']),
    role: asString(json['role'], 'user'),
    text: asString(json['text']),
    occurredAt: asDateOrNull(json['occurred_at']),
    attachments: asObjectList(
      json['attachments'],
    ).map(Attachment.fromJson).toList(growable: false),
    toolCalls: asObjectList(
      json['tool_calls'],
    ).map(ToolCall.replayed).toList(growable: false),
    blocks: asObjectList(
      json['blocks'],
    ).map(TurnBlock.fromJson).nonNulls.toList(growable: false),
    models: asStringList(json['models']),
  );
}
