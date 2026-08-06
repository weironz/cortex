import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../core/ulid.dart';
import '../models/chat_event.dart';
import '../models/chat_message.dart';
import '../models/chat_session.dart';
import '../models/tool_call.dart';
import 'app_providers.dart';
import 'chat_state.dart';

/// Owns sessions, transcripts and the in-flight generation.
class ChatController extends Notifier<ChatState> {
  StreamSubscription<ChatEvent>? _subscription;

  /// Deltas are coalesced into this buffer and flushed on a timer.
  ///
  /// A backend emitting one event per token can fire well above the display
  /// refresh rate; publishing state per event would schedule a rebuild that
  /// the framework then discards. Batching to one publish per frame keeps the
  /// typing effect visually identical while cutting rebuild count by ~4× on a
  /// fast stream. The buffer is always flushed before any terminal event, so
  /// no token can be dropped.
  final StringBuffer _pending = StringBuffer();
  Timer? _flushTimer;
  static const _flushInterval = Duration(milliseconds: 16);

  @override
  ChatState build() {
    ref.onDispose(() {
      _flushTimer?.cancel();
      _subscription?.cancel();
    });
    // Re-hydrate whenever the data source flips (mock ↔ live).
    ref.listen(cortexApiProvider, (_, _) => _reload());
    Future.microtask(_reload);
    return const ChatState();
  }

  CortexApi get _api => ref.read(cortexApiProvider);

  // -------------------------------------------------------------- lifecycle

  Future<void> _reload() async {
    await _cancelStream();
    state = const ChatState(sessionsLoading: true);
    await loadSessions();
  }

  Future<void> loadSessions() async {
    state = state.copyWith(sessionsLoading: true, sessionsError: null);
    try {
      final sessions = await _api.sessions();
      final active =
          state.activeSessionId ??
          (sessions.isNotEmpty ? sessions.first.id : null);
      state = state.copyWith(
        sessions: sessions,
        sessionsLoading: false,
        activeSessionId: active,
      );
    } on CortexApiException catch (e) {
      state = state.copyWith(
        sessionsLoading: false,
        sessionsError: e.message,
        sessions: const [],
      );
    } on Object catch (e) {
      state = state.copyWith(sessionsLoading: false, sessionsError: '$e');
    }
  }

  // ---------------------------------------------------------------- sessions

  void selectSession(String id) {
    if (state.activeSessionId == id) return;
    // An in-flight generation belongs to the session that started it; keep it
    // running and just look away. Cancelling on every sidebar click would lose
    // work the user did not ask to discard.
    state = state.copyWith(activeSessionId: id, sendError: null);
  }

  String createSession() {
    final session = ChatSession(
      id: Ulid.generate(),
      title: '新会话',
      updatedAt: DateTime.now(),
      isLocalDraft: true,
    );
    state = state.copyWith(
      sessions: [session, ...state.sessions],
      activeSessionId: session.id,
      sendError: null,
    );
    return session.id;
  }

  // -------------------------------------------------------------------- send

  Future<void> send(String rawText) async {
    final text = rawText.trim();
    if (text.isEmpty) return;
    if (state.streaming != null) return; // one generation at a time

    final sessionId = state.activeSessionId ?? createSession();

    final userMessage = ChatMessage(
      id: Ulid.generate(),
      role: MessageRole.user,
      text: text,
      createdAt: DateTime.now(),
    );
    _appendMessage(sessionId, userMessage);
    _touchSession(sessionId, titleFrom: text);

    final turn = StreamingTurn(
      messageId: Ulid.generate(),
      sessionId: sessionId,
      startedAt: DateTime.now(),
    );
    state = state.copyWith(streaming: turn, sendError: null);

    final stream = _api.chat(sessionId: sessionId, message: text);
    _subscription = stream.listen(
      _onEvent,
      onError: _onError,
      onDone: _onDone,
      cancelOnError: true,
    );
  }

  void _onEvent(ChatEvent event) {
    final turn = state.streaming;
    if (turn == null) return;

    switch (event) {
      case ChatDeltaEvent(:final text):
        if (text.isEmpty) return;
        _pending.write(text);
        _scheduleFlush();

      case ChatMemoryEvent(:final facts):
        _flushPending();
        state = state.copyWith(
          streaming: state.streaming?.copyWith(
            facts: [...?state.streaming?.facts, ...facts],
          ),
        );

      case ChatToolEvent(:final name, :final summary):
        _flushPending();
        state = state.copyWith(
          streaming: state.streaming?.copyWith(
            toolCalls: [
              ...?state.streaming?.toolCalls,
              ToolCall(name: name, summary: summary),
            ],
          ),
        );

      case ChatDoneEvent(:final episodeId):
        _commit(episodeId: episodeId);

      case ChatErrorEvent(:final message):
        _commit(error: message);

      case ChatUnknownEvent():
        // Forward compatibility: ignore quietly rather than break the turn.
        break;
    }
  }

  void _scheduleFlush() {
    _flushTimer ??= Timer(_flushInterval, () {
      _flushTimer = null;
      _flushPending();
    });
  }

  void _flushPending() {
    if (_pending.isEmpty) return;
    final chunk = _pending.toString();
    _pending.clear();
    final turn = state.streaming;
    if (turn == null) return;
    // Append, never replace — this is what makes the bubble grow instead of
    // re-laying-out from scratch.
    state = state.copyWith(
      streaming: turn.copyWith(
        text: turn.text + chunk,
        awaitingFirstToken: false,
      ),
    );
  }

  void _onError(Object error, StackTrace _) {
    final message = error is CortexApiException ? error.message : '$error';
    _commit(error: message);
  }

  void _onDone() {
    // A stream that closes without an explicit `done` still has usable text.
    if (state.streaming != null) _commit();
  }

  /// Moves the in-flight turn into the transcript and clears streaming state.
  void _commit({String? episodeId, String? error}) {
    _flushTimer?.cancel();
    _flushTimer = null;
    _flushPending();

    final turn = state.streaming;
    if (turn == null) return;

    _subscription?.cancel();
    _subscription = null;

    final message = ChatMessage(
      id: turn.messageId,
      role: MessageRole.assistant,
      text: turn.text,
      createdAt: turn.startedAt,
      facts: turn.facts,
      toolCalls: turn.toolCalls,
      episodeId: episodeId,
      error: error,
    );

    state = state.copyWith(streaming: null, sendError: error);
    _appendMessage(turn.sessionId, message);
    _touchSession(turn.sessionId);
  }

  /// User-initiated abort. Keeps whatever text already arrived.
  Future<void> stopGeneration() async {
    if (state.streaming == null) return;
    await _subscription?.cancel();
    _subscription = null;
    _commit(error: '已停止生成');
  }

  Future<void> _cancelStream() async {
    _flushTimer?.cancel();
    _flushTimer = null;
    _pending.clear();
    await _subscription?.cancel();
    _subscription = null;
  }

  void clearSendError() {
    if (state.sendError != null) state = state.copyWith(sendError: null);
  }

  /// Drops the last assistant turn and re-sends the user message before it.
  Future<void> retryLast() async {
    final transcript = state.activeTranscript;
    if (transcript.isEmpty) return;
    final sessionId = state.activeSessionId!;

    final trimmed = [...transcript];
    if (trimmed.last.role == MessageRole.assistant) trimmed.removeLast();
    if (trimmed.isEmpty || trimmed.last.role != MessageRole.user) return;
    final prompt = trimmed.removeLast().text;

    state = state.copyWith(
      transcripts: {...state.transcripts, sessionId: trimmed},
      sendError: null,
    );
    await send(prompt);
  }

  // ----------------------------------------------------------------- helpers

  void _appendMessage(String sessionId, ChatMessage message) {
    final existing = state.transcripts[sessionId] ?? const <ChatMessage>[];
    state = state.copyWith(
      transcripts: {
        ...state.transcripts,
        sessionId: [...existing, message],
      },
    );
  }

  /// Bumps `updated_at` and, for a fresh draft, derives a title from the first
  /// user message so the sidebar is not a wall of "新会话".
  void _touchSession(String sessionId, {String? titleFrom}) {
    final sessions = [...state.sessions];
    final index = sessions.indexWhere((s) => s.id == sessionId);
    if (index == -1) return;

    final current = sessions[index];
    final shouldRename =
        titleFrom != null && (current.isLocalDraft || current.title == '新会话');

    sessions[index] = current.copyWith(
      updatedAt: DateTime.now(),
      title: shouldRename ? _deriveTitle(titleFrom) : current.title,
      isLocalDraft: current.isLocalDraft && titleFrom == null,
    );
    state = state.copyWith(sessions: sessions);
  }

  static String _deriveTitle(String message) {
    final single = message.replaceAll(RegExp(r'\s+'), ' ').trim();
    if (single.length <= 24) return single;
    return '${single.substring(0, 24)}…';
  }
}

final chatControllerProvider = NotifierProvider<ChatController, ChatState>(
  ChatController.new,
);
