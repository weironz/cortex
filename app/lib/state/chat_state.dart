import '../models/chat_message.dart';
import '../models/chat_session.dart';
import '../models/memory_fact.dart';
import '../models/tool_call.dart';

/// The assistant turn currently being streamed.
///
/// Kept out of the transcript list on purpose: the transcript's `List` identity
/// must stay stable while tokens arrive, otherwise every `select` watching it
/// fires and the whole conversation rebuilds 60× a second.
class StreamingTurn {
  const StreamingTurn({
    required this.messageId,
    required this.sessionId,
    required this.startedAt,
    this.text = '',
    this.facts = const [],
    this.toolCalls = const [],
    this.awaitingFirstToken = true,
  });

  final String messageId;
  final String sessionId;
  final DateTime startedAt;
  final String text;

  /// Facts injected into this turn (from the `memory` event).
  final List<MemoryFact> facts;

  /// Tools the agent invoked (from `tool` events).
  final List<ToolCall> toolCalls;

  /// True until the first `delta` lands — drives the thinking indicator.
  final bool awaitingFirstToken;

  StreamingTurn copyWith({
    String? text,
    List<MemoryFact>? facts,
    List<ToolCall>? toolCalls,
    bool? awaitingFirstToken,
  }) => StreamingTurn(
    messageId: messageId,
    sessionId: sessionId,
    startedAt: startedAt,
    text: text ?? this.text,
    facts: facts ?? this.facts,
    toolCalls: toolCalls ?? this.toolCalls,
    awaitingFirstToken: awaitingFirstToken ?? this.awaitingFirstToken,
  );
}

class ChatState {
  const ChatState({
    this.sessions = const [],
    this.sessionsLoading = true,
    this.sessionsError,
    this.activeSessionId,
    this.transcripts = const {},
    this.streaming,
    this.sendError,
  });

  final List<ChatSession> sessions;
  final bool sessionsLoading;
  final String? sessionsError;
  final String? activeSessionId;

  /// Committed messages, keyed by session id.
  final Map<String, List<ChatMessage>> transcripts;

  final StreamingTurn? streaming;
  final String? sendError;

  static const _emptyTranscript = <ChatMessage>[];

  /// Stable across streaming deltas — see [StreamingTurn].
  List<ChatMessage> get activeTranscript =>
      transcripts[activeSessionId] ?? _emptyTranscript;

  ChatSession? get activeSession {
    final id = activeSessionId;
    if (id == null) return null;
    for (final s in sessions) {
      if (s.id == id) return s;
    }
    return null;
  }

  /// True when a stream is running *for the session currently on screen*.
  /// Switching sessions mid-stream should not show a spinner on the new one.
  bool get isStreamingActive =>
      streaming != null && streaming!.sessionId == activeSessionId;

  ChatState copyWith({
    List<ChatSession>? sessions,
    bool? sessionsLoading,
    Object? sessionsError = _sentinel,
    Object? activeSessionId = _sentinel,
    Map<String, List<ChatMessage>>? transcripts,
    Object? streaming = _sentinel,
    Object? sendError = _sentinel,
  }) => ChatState(
    sessions: sessions ?? this.sessions,
    sessionsLoading: sessionsLoading ?? this.sessionsLoading,
    sessionsError: sessionsError == _sentinel
        ? this.sessionsError
        : sessionsError as String?,
    activeSessionId: activeSessionId == _sentinel
        ? this.activeSessionId
        : activeSessionId as String?,
    transcripts: transcripts ?? this.transcripts,
    streaming: streaming == _sentinel
        ? this.streaming
        : streaming as StreamingTurn?,
    sendError: sendError == _sentinel ? this.sendError : sendError as String?,
  );

  /// Distinguishes "not passed" from "explicitly set to null" in [copyWith].
  static const Object _sentinel = Object();
}
