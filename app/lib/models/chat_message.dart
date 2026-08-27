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
    this.errorIsDeterministic = false,
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

  /// 这次失败是**确定性**的吗 —— 重发这一模一样的请求必定同样的结果。
  ///
  /// 由服务端说（`ErrorBody.retryable == false`），界面据此把「重试」与
  /// 「换模型」两个按钮收掉：它们是给「这个模型这一次不行」准备的出路，
  /// 而确定性失败里它们只是两堵一样的墙。
  ///
  /// 典型场景：会话钉在一台关着的电脑上。真正的出路（把它唤醒 / 在它上面
  /// 开 agent）没有一件做得到在这个屏幕上，而**一个摆在那儿的按钮本身就在
  /// 说「点我可能有用」** —— 用户点完开始怀疑自己网不好，真正该做的事
  /// 反被挡住了视线。
  final bool errorIsDeterministic;

  ChatMessage copyWith({
    String? text,
    List<ToolCall>? toolCalls,
    List<Attachment>? attachments,
    String? episodeId,
    String? error,
    bool? errorIsDeterministic,
  }) => ChatMessage(
    id: id,
    role: role,
    text: text ?? this.text,
    createdAt: createdAt,
    toolCalls: toolCalls ?? this.toolCalls,
    attachments: attachments ?? this.attachments,
    episodeId: episodeId ?? this.episodeId,
    error: error ?? this.error,
    errorIsDeterministic: errorIsDeterministic ?? this.errorIsDeterministic,
  );
}
