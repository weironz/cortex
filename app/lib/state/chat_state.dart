import '../models/chat_message.dart';
import '../models/chat_session.dart';
import '../models/injected_memory.dart';
import '../models/tool_call.dart';

/// How many episodes one `GET /sessions/{id}` page holds.
///
/// This is now a *transfer* budget as well as a render one. It used to be
/// neither: the server sent up to 500 episodes with no cursor and the client
/// revealed them 40 at a time, so scrolling up cost nothing but the whole
/// conversation had already crossed the wire — and anything past 500 never
/// arrived at all. The server pages properly now, so asking for less actually
/// means receiving less.
///
/// Kept at the old window size on purpose: it is a known-comfortable first
/// paint (~20 turns), and each "load earlier" is one round trip for one screen
/// of markdown rather than a spinner over hundreds of bubbles.
const int kEpisodePage = 40;

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
  final List<InjectedMemory> facts;

  /// Tools the agent invoked (from `tool` events).
  final List<ToolCall> toolCalls;

  /// True until the first `delta` lands — drives the thinking indicator.
  final bool awaitingFirstToken;

  StreamingTurn copyWith({
    String? text,
    List<InjectedMemory>? facts,
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

/// One session's messages plus the state of paging them in.
///
/// ## [messages] must keep its identity between reads
///
/// `ConversationView` watches it through `ref.watch(select(...))`, and `select`
/// compares with `==`. A getter that computed a fresh list per read would
/// therefore report "changed" on every read, and every SSE delta would rebuild
/// the entire `ListView` — precisely the cost the streaming design exists to
/// avoid. So it is a plain field on an immutable object, rebuilt only when the
/// messages really change.
///
/// This used to be enforced on a separate `visible` window that materialised a
/// `sublist` in the constructor. The window is gone — paging is real now, so
/// the client holds only what it asked for — but the invariant it was
/// protecting is the same one, and it moved onto [messages].
class Transcript {
  const Transcript({
    this.messages = const [],
    this.loading = false,
    this.loadingEarlier = false,
    this.error,
    this.loadedFromServer = false,
    this.hasEarlier = false,
    this.cursor,
  });

  /// Everything paged in so far, oldest first.
  final List<ChatMessage> messages;

  /// The first page is in flight.
  final bool loading;

  /// An older page is in flight. Separate from [loading] because the two
  /// render differently: one is a blank screen with a spinner, the other is a
  /// spinner above a conversation that stays readable.
  final bool loadingEarlier;

  final String? error;

  /// `GET /sessions/{id}` completed for this session. Distinguishes "empty
  /// because it is new" from "empty because we never asked".
  final bool loadedFromServer;

  /// The server says older messages exist. Taken from the response's
  /// `has_more` rather than inferred from the page size — the two disagree on
  /// an exactly-divisible history, and the inference loses.
  final bool hasEarlier;

  /// Opaque `before` value for the next page up. Null once [hasEarlier] is
  /// false.
  final String? cursor;

  Transcript copyWith({
    List<ChatMessage>? messages,
    bool? loading,
    bool? loadingEarlier,
    Object? error = _sentinel,
    bool? loadedFromServer,
    bool? hasEarlier,
    Object? cursor = _sentinel,
  }) => Transcript(
    messages: messages ?? this.messages,
    loading: loading ?? this.loading,
    loadingEarlier: loadingEarlier ?? this.loadingEarlier,
    error: error == _sentinel ? this.error : error as String?,
    loadedFromServer: loadedFromServer ?? this.loadedFromServer,
    hasEarlier: hasEarlier ?? this.hasEarlier,
    cursor: cursor == _sentinel ? this.cursor : cursor as String?,
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

  /// What the conversation view renders. Identity-stable across streaming
  /// deltas — see [Transcript].
  List<ChatMessage> get activeTranscript =>
      transcripts[activeSessionId]?.messages ?? _emptyTranscript;

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
