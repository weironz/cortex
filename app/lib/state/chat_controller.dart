import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../core/ulid.dart';
import '../models/attachment.dart';
import '../models/chat_event.dart';
import '../models/chat_message.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/tool_call.dart';
import '../models/workspace.dart';
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
    if (!ref.mounted) return;
    state = ChatState(
      sessionsLoading: true,
      // The toggle is a view preference, not backend data — a source swap
      // should not silently re-hide what the user asked to see.
      showArchived: state.showArchived,
    );
    await loadSessions();
  }

  /// Every `state =` below sits behind a [Ref.mounted] check.
  ///
  /// Not defensive noise: this runs after an `await`, and the provider can be
  /// gone by then. Two ways in — the app shutting down while the first load is
  /// still in flight, and (much more likely now) `SyncController` calling this
  /// from a WebSocket bump at a moment it does not control. Writing to a
  /// disposed `Ref` throws out of a future nobody awaits, which surfaces as an
  /// unhandled async error rather than anything the UI can act on.
  Future<void> loadSessions() async {
    if (!ref.mounted) return;
    state = state.copyWith(sessionsLoading: true, sessionsError: null);
    try {
      final remote = await _api.sessions(includeArchived: state.showArchived);
      if (!ref.mounted) return;
      final merged = _mergeSessions(remote);
      final active =
          state.activeSessionId ??
          (merged.isNotEmpty ? merged.first.id : null);
      state = state.copyWith(
        sessions: merged,
        sessionsLoading: false,
        activeSessionId: active,
      );
      if (active != null) unawaited(_ensureTranscript(active));
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(
        sessionsLoading: false,
        sessionsError: e.message,
        // Drafts only exist on this device; dropping them because the daemon
        // is momentarily unreachable would destroy work the user can still see.
        sessions: state.sessions.where((s) => s.isLocalDraft).toList(),
      );
    } on Object catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(sessionsLoading: false, sessionsError: '$e');
    }
  }

  /// Reconciles the daemon's list with what is on screen.
  ///
  /// A plain replace loses three things:
  ///
  /// * **Local drafts.** [createSession] mints a ULID client-side and shows the
  ///   session immediately; the daemon only learns of it when the first turn
  ///   commits. Before that, a refresh — now triggered by any WebSocket bump,
  ///   including one caused by somebody else's device — would make the session
  ///   the user is typing in vanish.
  /// * **The "未同步" flag.** When the draft's id does come back from the
  ///   server, that *is* the write receipt: the remote row carries
  ///   `isLocalDraft == false`, so the badge clears itself.
  /// * **Local-only edits.** A rename against a daemon without
  ///   `PATCH /sessions/{id}` lives only here. Letting the next refresh
  ///   overwrite it would make the rename appear to work and then silently
  ///   undo itself a second later — worse than refusing outright.
  List<ChatSession> _mergeSessions(List<ChatSession> remote) {
    final remoteIds = remote.map((s) => s.id).toSet();
    final unconfirmed = state.sessions.where(
      (s) => s.isLocalDraft && !remoteIds.contains(s.id),
    );
    final overrides = {
      for (final s in state.sessions)
        if (s.hasLocalOverrides) s.id: s,
    };
    final reconciled = [
      for (final s in remote)
        if (overrides[s.id] case final local?)
          s.copyWith(
            title: local.title,
            titleIsCustom: local.titleIsCustom,
            archived: local.archived,
            workspace: local.workspace,
            hasLocalOverrides: true,
          )
        else
          s,
    ];
    return [...unconfirmed, ...reconciled];
  }

  // ---------------------------------------------------------------- sessions

  void selectSession(String id) {
    if (state.activeSessionId == id) return;
    // An in-flight generation belongs to the session that started it; keep it
    // running and just look away. Cancelling on every sidebar click would lose
    // work the user did not ask to discard.
    state = state.copyWith(activeSessionId: id, sendError: null);
    unawaited(_ensureTranscript(id));
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
      // Marked loaded so the transcript view does not try to fetch a session
      // the daemon has never heard of and render a 404 as a failure.
      transcripts: {
        ...state.transcripts,
        session.id: Transcript(loadedFromServer: true),
      },
      sendError: null,
    );
    return session.id;
  }

  void setShowArchived(bool value) {
    if (state.showArchived == value) return;
    state = state.copyWith(showArchived: value);
    // Refetch rather than filter locally: archived sessions were never sent,
    // so there is nothing local to reveal.
    unawaited(loadSessions());
  }

  /// Renames a session.
  ///
  /// Rethrows a genuine failure so the dialog can show it; an *unsupported*
  /// endpoint is not a failure the user can act on, so the change is kept
  /// locally and flagged instead.
  Future<void> renameSession(String id, String title) async {
    final trimmed = title.trim();
    if (trimmed.isEmpty) return;
    await _patch(
      id,
      call: () => _api.updateSession(id, title: trimmed),
      local: (s) =>
          s.copyWith(title: trimmed, titleIsCustom: true, hasLocalOverrides: true),
    );
  }

  /// Archives or restores. **Not** deletion: episodes, attachments and the
  /// memory extracted from them are untouched, which is why no confirmation
  /// dialog threatens permanence.
  Future<void> setArchived(String id, bool archived) async {
    await _patch(
      id,
      call: () => _api.updateSession(id, archived: archived),
      local: (s) => s.copyWith(archived: archived, hasLocalOverrides: true),
    );
    // An archived session that is still selected would leave the transcript on
    // screen with no matching row in the list.
    if (archived && !state.showArchived && state.activeSessionId == id) {
      final next = state.visibleSessions.where((s) => s.id != id).firstOrNull;
      state = state.copyWith(activeSessionId: next?.id);
      if (next != null) unawaited(_ensureTranscript(next.id));
    }
  }

  /// Binds a workspace. The daemon validates the path and its rejection message
  /// is written to be read by the user, so it is allowed to propagate.
  Future<void> bindWorkspace(String id, String root) async {
    final trimmed = root.trim();
    if (trimmed.isEmpty) return;
    await _patch(
      id,
      call: () => _api.updateSession(id, workspace: trimmed),
      local: (s) => s.copyWith(
        workspace: Workspace(root: trimmed),
        hasLocalOverrides: true,
      ),
    );
  }

  Future<void> unbindWorkspace(String id) async {
    await _patch(
      id,
      call: () => _api.updateSession(id, clearWorkspace: true),
      local: (s) => s.copyWith(workspace: null, hasLocalOverrides: true),
    );
  }

  /// One shape for all four mutations: try the server, fall back to a flagged
  /// local edit **only** when the endpoint does not exist.
  ///
  /// The distinction matters. A 400 from the workspace validator ("整台机器不是
  /// 工作区") must reach the user unchanged — swallowing it and applying the
  /// binding locally would show a workspace that the agent will never honour.
  Future<void> _patch(
    String id, {
    required Future<ChatSession> Function() call,
    required ChatSession Function(ChatSession) local,
  }) async {
    try {
      final updated = await call();
      if (!ref.mounted) return;
      _replaceSession(id, (s) => updated.copyWith(isLocalDraft: s.isLocalDraft));
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      if (!e.isUnsupported) rethrow;
      _replaceSession(id, local);
    }
  }

  void _replaceSession(String id, ChatSession Function(ChatSession) update) {
    final sessions = [...state.sessions];
    final index = sessions.indexWhere((s) => s.id == id);
    if (index == -1) return;
    sessions[index] = update(sessions[index]);
    state = state.copyWith(sessions: sessions);
  }

  // ------------------------------------------------------------ history replay

  /// Fetches a session's messages the first time it is opened.
  ///
  /// Only ever runs when nothing is held for the session yet. A session the
  /// user has already talked in this run has its turns in memory, and merging a
  /// server replay into them would need identity for the *user* message too —
  /// which the client does not have, because it never learns the episode id of
  /// its own prompt. Refetching would therefore duplicate every user turn.
  Future<void> _ensureTranscript(String id) async {
    if (state.transcripts.containsKey(id)) return;
    final session = state.sessions.firstWhereOrNull((s) => s.id == id);
    if (session != null && session.isLocalDraft) return;
    await loadTranscript(id);
  }

  Future<void> loadTranscript(String id) async {
    if (!ref.mounted) return;
    final current = state.transcripts[id] ?? Transcript();
    if (current.loading) return;
    _putTranscript(id, current.copyWith(loading: true, error: null));

    try {
      final detail = await _api.sessionDetail(id);
      if (!ref.mounted) return;
      _putTranscript(
        id,
        Transcript(
          messages: detail.episodes
              .map(_messageFromEpisode)
              .toList(growable: false),
          loading: false,
          loadedFromServer: true,
          serverTruncated: detail.truncated,
        ),
      );
      // The detail response carries a fresher overview than the list did.
      _replaceSession(
        id,
        (s) => s.hasLocalOverrides
            ? s
            : detail.session.copyWith(isLocalDraft: s.isLocalDraft),
      );
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      _putTranscript(
        id,
        (state.transcripts[id] ?? Transcript()).copyWith(
          loading: false,
          // A session with no episodes yet is a 404 here, not a failure.
          error: e.statusCode == 404 ? null : e.message,
          loadedFromServer: e.statusCode == 404,
        ),
      );
    } on Object catch (e) {
      if (!ref.mounted) return;
      _putTranscript(
        id,
        (state.transcripts[id] ?? Transcript()).copyWith(
          loading: false,
          error: '$e',
        ),
      );
    }
  }

  /// Grows the render window by one page, oldest-ward.
  void revealEarlier(String id) {
    final transcript = state.transcripts[id];
    if (transcript == null || !transcript.hasEarlier) return;
    _putTranscript(
      id,
      transcript.copyWith(
        visibleCount: transcript.visibleCount + kTranscriptWindow,
      ),
    );
  }

  /// An episode is the archival record; a [ChatMessage] is the view-model.
  ///
  /// Two things do not survive the round trip, both because the server does not
  /// store them per-episode: the memory injected into that turn, and the tools
  /// it called. A replayed assistant turn therefore shows no audit drawer —
  /// visibly different from a live one, and honestly so.
  static ChatMessage _messageFromEpisode(Episode e) => ChatMessage(
    id: e.id,
    role: e.role == 'assistant' ? MessageRole.assistant : MessageRole.user,
    text: e.text,
    createdAt: e.occurredAt ?? DateTime.now(),
    attachments: e.attachments,
    episodeId: e.id,
  );

  void _putTranscript(String id, Transcript transcript) {
    state = state.copyWith(
      transcripts: {...state.transcripts, id: transcript},
    );
  }

  // -------------------------------------------------------------------- send

  Future<void> send(
    String rawText, {
    List<Attachment> attachments = const [],
  }) async {
    final text = rawText.trim();
    // An attachment with no words is a legitimate message ("看这张图")…
    if (text.isEmpty && attachments.isEmpty) return;
    if (state.streaming != null) return; // one generation at a time

    final sessionId = state.activeSessionId ?? createSession();

    final userMessage = ChatMessage(
      id: Ulid.generate(),
      role: MessageRole.user,
      text: text,
      createdAt: DateTime.now(),
      attachments: attachments,
    );
    _appendMessage(sessionId, userMessage);
    _touchSession(sessionId, titleFrom: text.isEmpty ? null : text);

    final turn = StreamingTurn(
      messageId: Ulid.generate(),
      sessionId: sessionId,
      startedAt: DateTime.now(),
    );
    state = state.copyWith(streaming: turn, sendError: null);

    final stream = _api.chat(
      sessionId: sessionId,
      message: text,
      attachments: attachments,
    );
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
        // Call and result arrive as two events; [ToolCall.merge] folds them
        // into a single row instead of printing the same tool twice.
        final current = state.streaming;
        if (current == null) return;
        state = state.copyWith(
          streaming: current.copyWith(
            toolCalls: ToolCall.merge(current.toolCalls, name, summary),
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
    final last = trimmed.removeLast();

    _putTranscript(
      sessionId,
      (state.transcripts[sessionId] ?? Transcript()).copyWith(
        messages: trimmed,
      ),
    );
    state = state.copyWith(sendError: null);
    // Attachments ride along: they are already registered blobs, so resending
    // costs nothing and dropping them would change the question being asked.
    await send(last.text, attachments: last.attachments);
  }

  // ----------------------------------------------------------------- helpers

  void _appendMessage(String sessionId, ChatMessage message) {
    final existing = state.transcripts[sessionId] ?? Transcript();
    _putTranscript(
      sessionId,
      existing.copyWith(
        messages: [...existing.messages, message],
        // Keep the newest turn inside the window when history was paged in.
        visibleCount: existing.visibleCount + 1,
      ),
    );
  }

  /// Bumps `updated_at` and, for a fresh draft, derives a title from the first
  /// user message so the sidebar is not a wall of "新会话".
  void _touchSession(String sessionId, {String? titleFrom}) {
    final sessions = [...state.sessions];
    final index = sessions.indexWhere((s) => s.id == sessionId);
    if (index == -1) return;

    final current = sessions[index];
    // A title the user set explicitly is never overwritten by a derived one.
    final shouldRename =
        titleFrom != null &&
        !current.titleIsCustom &&
        (current.isLocalDraft || current.title == '新会话');

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

extension _FirstWhereOrNull<E> on Iterable<E> {
  E? firstWhereOrNull(bool Function(E) test) {
    for (final e in this) {
      if (test(e)) return e;
    }
    return null;
  }
}

final chatControllerProvider = NotifierProvider<ChatController, ChatState>(
  ChatController.new,
);
