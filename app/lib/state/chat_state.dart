import '../models/chat_message.dart';
import '../models/chat_session.dart';
import '../models/memory_fact.dart';
import '../models/tool_call.dart';

/// How many messages a transcript reveals before the user asks for more.
///
/// The number is about *rendering*, not transfer — see [Transcript].
const int kTranscriptWindow = 40;

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

/// One session's messages plus the state of replaying them.
///
/// ## Why the window is materialised in the constructor
///
/// [visible] could just as well be a getter that slices [messages] on demand.
/// It must not be. `ConversationView` watches the visible list through
/// `ref.watch(select(...))`, and `select` compares with `==` — a getter that
/// returns a fresh `sublist` every call yields a new object identity on every
/// read, so the comparison always reports "changed" and the entire `ListView`
/// rebuilds on every SSE delta. That is precisely the cost the streaming design
/// goes out of its way to avoid.
///
/// Computing it once here, in an immutable object that is only rebuilt when the
/// messages or the window actually change, keeps the identity stable for the
/// whole duration of a stream.
class Transcript {
  Transcript({
    this.messages = const [],
    this.visibleCount = kTranscriptWindow,
    this.loading = false,
    this.error,
    this.loadedFromServer = false,
    this.serverTruncated = false,
  }) : visible = messages.length <= visibleCount
           ? messages
           : messages.sublist(messages.length - visibleCount);

  /// Everything we hold, oldest first.
  final List<ChatMessage> messages;

  /// The newest [visibleCount] of [messages]. Identity-stable — see class doc.
  final List<ChatMessage> visible;

  /// Size of the render window, grown by [ChatController.revealEarlier].
  final int visibleCount;

  final bool loading;
  final String? error;

  /// `GET /sessions/{id}` completed for this session. Distinguishes "empty
  /// because it is new" from "empty because we never asked".
  final bool loadedFromServer;

  /// The daemon returned fewer episodes than the session claims to have. Its
  /// `LIMIT 500` has no cursor, so the *end* of a long conversation is what
  /// goes missing — worth saying out loud.
  final bool serverTruncated;

  /// There is older history held but not yet rendered.
  bool get hasEarlier => messages.length > visible.length;

  Transcript copyWith({
    List<ChatMessage>? messages,
    int? visibleCount,
    bool? loading,
    Object? error = _sentinel,
    bool? loadedFromServer,
    bool? serverTruncated,
  }) => Transcript(
    messages: messages ?? this.messages,
    visibleCount: visibleCount ?? this.visibleCount,
    loading: loading ?? this.loading,
    error: error == _sentinel ? this.error : error as String?,
    loadedFromServer: loadedFromServer ?? this.loadedFromServer,
    serverTruncated: serverTruncated ?? this.serverTruncated,
  );

  static const Object _sentinel = Object();
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
    this.showArchived = false,
  });

  final List<ChatSession> sessions;
  final bool sessionsLoading;
  final String? sessionsError;
  final String? activeSessionId;

  /// Committed messages, keyed by session id.
  final Map<String, Transcript> transcripts;

  final StreamingTurn? streaming;
  final String? sendError;

  /// Whether archived sessions appear in the sidebar. Also decides the
  /// `include_archived` query parameter, so flipping it refetches.
  final bool showArchived;

  static const _emptyTranscript = <ChatMessage>[];

  Transcript? get activeTranscriptState => transcripts[activeSessionId];

  /// Every message we hold for the active session — used by `retryLast` and by
  /// tests, which care about what happened rather than what is on screen.
  List<ChatMessage> get activeTranscript =>
      transcripts[activeSessionId]?.messages ?? _emptyTranscript;

  /// What the conversation view renders. Stable across streaming deltas — see
  /// [Transcript].
  List<ChatMessage> get activeVisibleMessages =>
      transcripts[activeSessionId]?.visible ?? _emptyTranscript;

  ChatSession? get activeSession {
    final id = activeSessionId;
    if (id == null) return null;
    for (final s in sessions) {
      if (s.id == id) return s;
    }
    return null;
  }

  /// Sessions the sidebar should list, honouring [showArchived].
  ///
  /// Filtered here as well as server-side because a session archived in this
  /// session of the app must disappear immediately, without waiting for a
  /// refetch — and because a daemon too old to understand `include_archived`
  /// sends everything.
  List<ChatSession> get visibleSessions => showArchived
      ? sessions
      : sessions.where((s) => !s.archived).toList(growable: false);

  /// True when a stream is running *for the session currently on screen*.
  /// Switching sessions mid-stream should not show a spinner on the new one.
  bool get isStreamingActive =>
      streaming != null && streaming!.sessionId == activeSessionId;

  ChatState copyWith({
    List<ChatSession>? sessions,
    bool? sessionsLoading,
    Object? sessionsError = _sentinel,
    Object? activeSessionId = _sentinel,
    Map<String, Transcript>? transcripts,
    Object? streaming = _sentinel,
    Object? sendError = _sentinel,
    bool? showArchived,
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
    showArchived: showArchived ?? this.showArchived,
  );

  /// Distinguishes "not passed" from "explicitly set to null" in [copyWith].
  static const Object _sentinel = Object();
}
