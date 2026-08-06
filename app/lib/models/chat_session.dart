import 'json.dart';

/// A conversation thread (`GET /sessions`).
class ChatSession {
  const ChatSession({
    required this.id,
    required this.title,
    this.updatedAt,
    this.isLocalDraft = false,
  });

  final String id;
  final String title;
  final DateTime? updatedAt;

  /// True for a session created client-side that the server has not yet
  /// acknowledged. The first `POST /chat` with this id materialises it, so we
  /// keep it in the list optimistically and just mark it.
  final bool isLocalDraft;

  ChatSession copyWith({String? title, DateTime? updatedAt, bool? isLocalDraft}) =>
      ChatSession(
        id: id,
        title: title ?? this.title,
        updatedAt: updatedAt ?? this.updatedAt,
        isLocalDraft: isLocalDraft ?? this.isLocalDraft,
      );

  factory ChatSession.fromJson(Map<String, dynamic> json) => ChatSession(
    id: asString(json['id']),
    title: asString(json['title'], '未命名会话'),
    updatedAt: asDateOrNull(json['updated_at']),
  );
}
