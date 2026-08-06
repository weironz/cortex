import 'memory_fact.dart';
import 'tool_call.dart';

enum MessageRole { user, assistant }

/// A message as rendered in the conversation view.
///
/// Distinct from [Episode]: an episode is the server's archival record, a
/// [ChatMessage] is the client's view-model (it can exist before the server has
/// acknowledged it, and it carries the per-turn injected memory).
class ChatMessage {
  const ChatMessage({
    required this.id,
    required this.role,
    required this.text,
    required this.createdAt,
    this.facts = const [],
    this.toolCalls = const [],
    this.episodeId,
    this.error,
  });

  /// Client-local id. Stable across the streaming lifetime of the message.
  final String id;
  final MessageRole role;
  final String text;
  final DateTime createdAt;

  /// For assistant messages: the memory injected into this turn.
  final List<MemoryFact> facts;

  /// For assistant messages: tools the agent invoked during this turn.
  final List<ToolCall> toolCalls;

  /// Server-assigned archival id, available once the turn completes.
  final String? episodeId;

  /// Non-null when the turn failed; rendered inline instead of a bubble body.
  final String? error;

  ChatMessage copyWith({
    String? text,
    List<MemoryFact>? facts,
    List<ToolCall>? toolCalls,
    String? episodeId,
    String? error,
  }) => ChatMessage(
    id: id,
    role: role,
    text: text ?? this.text,
    createdAt: createdAt,
    facts: facts ?? this.facts,
    toolCalls: toolCalls ?? this.toolCalls,
    episodeId: episodeId ?? this.episodeId,
    error: error ?? this.error,
  );
}
