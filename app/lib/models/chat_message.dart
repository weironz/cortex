import 'attachment.dart';
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
    this.toolCalls = const [],
    this.attachments = const [],
    this.episodeId,
    this.error,
    this.models = const [],
  });

  /// Client-local id. Stable across the streaming lifetime of the message.
  final String id;
  final MessageRole role;
  final String text;
  final DateTime createdAt;

  /// The memory injected into this turn.
  ///
  /// Normally carried by the assistant message, because that is where the
  /// drawer is drawn. It lands on the *user* message only when the turn has no
  /// answer to hang it on — a turn the model failed still injected memory, and
  /// dropping the record of that would erase exactly the case worth auditing.

  /// Tools the agent invoked during this turn. Same placement rule as [facts].
  final List<ToolCall> toolCalls;

  /// Blobs carried by this message. Only user messages have them today, but the
  /// field is on the base type because `episode_blobs` is not role-scoped.
  final List<Attachment> attachments;

  /// 这条回复**先后**是谁写的，按发生顺序。
  ///
  /// 一轮里可能不止一个：「自动」档按请求挑模型，而一次回复有几次工具
  /// 调用就发几次请求 —— 于是可能先用便宜的跑工具、再用贵的写答案。
  ///
  /// 空 = 不知道（迁移之前的历史、导入的记录、老服务端）。
  /// 界面**什么都不画** —— 猜一个「默认模型」填上去等于对历史撒谎。
  final List<String> models;

  /// Server-assigned archival id, available once the turn completes.
  final String? episodeId;

  /// Non-null when the turn failed; rendered inline instead of a bubble body.
  final String? error;

  ChatMessage copyWith({
    String? text,
    List<ToolCall>? toolCalls,
    List<Attachment>? attachments,
    String? episodeId,
    String? error,
  }) => ChatMessage(
    id: id,
    role: role,
    text: text ?? this.text,
    createdAt: createdAt,
    toolCalls: toolCalls ?? this.toolCalls,
    attachments: attachments ?? this.attachments,
    episodeId: episodeId ?? this.episodeId,
    error: error ?? this.error,
  );
}
