import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../models/pending_confirmation.dart';
import 'app_providers.dart';
import 'notify_prefs.dart';

/// What happened to a prompt that is no longer answerable.
///
/// Kept separate from an error channel on purpose — none of these is a failure
/// the user caused or can fix, and styling them as errors would teach people to
/// ignore the one thing on screen that ever really needs attention.
enum ConfirmOutcome {
  allowed,
  denied,

  /// The daemon answered 404: another device got there first, the turn timed
  /// out, or the turn ended. `cortexd::confirm` refuses to distinguish these
  /// (doing so would let someone probe for "is a command awaiting approval
  /// right now?"), so the client states the set rather than picking one.
  superseded,

  /// The local countdown reached zero without a receipt. Silence is refusal
  /// server-side; this is the client saying so before the user wonders why the
  /// prompt vanished.
  expired,
}

/// A prompt that has left the queue, kept briefly so its disappearance is
/// explained rather than merely observed.
class ConfirmResolution {
  const ConfirmResolution({
    required this.request,
    required this.outcome,
    required this.at,
  });

  final PendingConfirmation request;
  final ConfirmOutcome outcome;
  final DateTime at;
}

class ConfirmState {
  const ConfirmState({
    this.pending = const [],
    this.resolved = const [],
    this.answering = const {},
    this.error,
  });

  /// Everything still awaiting an answer, soonest deadline first.
  ///
  /// Not filtered to the active session. A confirmation raised on one device
  /// and recovered on another arrives with a `session_id` the user may not be
  /// looking at, and hiding it would reproduce exactly the failure the recovery
  /// endpoint exists to prevent: a suspended turn with nothing on screen to
  /// explain it.
  final List<PendingConfirmation> pending;

  /// Recently retired prompts, newest first, capped at [kResolvedMemory].
  final List<ConfirmResolution> resolved;

  /// Tokens with a receipt in flight, so the buttons can disable without
  /// blocking the rest of the list.
  final Set<String> answering;

  /// A genuine transport failure while delivering a receipt. A 404 never lands
  /// here — that is [ConfirmOutcome.superseded].
  final String? error;

  bool get hasPending => pending.isNotEmpty;

  static const int kResolvedMemory = 3;

  ConfirmState copyWith({
    List<PendingConfirmation>? pending,
    List<ConfirmResolution>? resolved,
    Set<String>? answering,
    Object? error = _sentinel,
  }) => ConfirmState(
    pending: pending ?? this.pending,
    resolved: resolved ?? this.resolved,
    answering: answering ?? this.answering,
    error: error == _sentinel ? this.error : error as String?,
  );

  static const Object _sentinel = Object();
}

/// Owns the tool-confirmation queue: arrival, countdown, receipt, recovery.
///
/// ## The three ways a prompt arrives
///
/// 1. **`ChatEvent::Confirm` on the live SSE stream** — the normal case, routed
///    in by `ChatController`.
/// 2. **`GET /confirmations` after a reconnect** — the SSE stream is single-
///    shot with no replay, so a request that was in flight when the socket
///    dropped is *never* re-sent. Without this poll the turn stays suspended
///    for its full timeout with nothing on screen.
/// 3. **`GET /confirmations` on a second device** — the confirmation queue is a
///    shared book, not a property of the connection that raised it.
///
/// All three produce the same object, and [_merge] is keyed on the token, so a
/// prompt already on screen is never duplicated by a recovery poll.
///
/// ## Why the countdown is local
///
/// The daemon sends a *duration*, never an absolute time, so nothing here
/// depends on the two clocks agreeing. The ticker exists because the timeout is
/// silent: at zero the server treats the request as refused and moves on, and a
/// prompt that simply sat there looking answerable would let the user click
/// "允许" thirty seconds after the decision was already made for them.
class ConfirmController extends Notifier<ConfirmState> {
  /// One second is the coarsest tick that still renders a smooth countdown, and
  /// the timer only runs while something is pending — an idle app schedules
  /// nothing.
  static const Duration _tick = Duration(seconds: 1);

  Timer? _ticker;
  int _generation = 0;

  @override
  ConfirmState build() {
    final generation = ++_generation;
    ref.onDispose(() => _ticker?.cancel());
    // Re-hydrate on a data-source swap: a queue belonging to the previous
    // daemon is meaningless against the new one, and its tokens would all 404.
    ref.listen(cortexApiProvider, (_, _) {
      // 必须 +1，不能只清空。在飞的那个 `pendingConfirmations()` 是打给
      // **旧后端**的；它之后才回来，而 `_alive()` 只比对 generation ——
      // 不 +1 的话它会通过检查，把旧后端的确认项 merge 进这个刚清干净的
      // state。换的若是账号，用户会被问「要不要执行」，而那是别人的命令。
      _generation++;
      state = const ConfirmState();
      unawaited(recover());
    });
    // A cold start is indistinguishable from a reconnect as far as the server
    // is concerned: either way there may already be something waiting.
    Future.microtask(() {
      if (_alive(generation)) unawaited(recover());
    });
    return const ConfirmState();
  }

  bool _alive(int generation) => ref.mounted && generation == _generation;

  CortexApi get _api => ref.read(cortexApiProvider);

  // ------------------------------------------------------------- arrival

  /// Called by `ChatController` when a `confirm` frame arrives.
  ///
  /// [sessionId] comes from the stream's own context because the SSE event does
  /// not carry one — it does not need to, since a stream belongs to exactly one
  /// session, but the recovery endpoint does report it and the two paths have
  /// to produce the same object.
  void offer(PendingConfirmation request, {String? sessionId}) {
    final stamped = sessionId == null
        ? request
        : request.withSession(sessionId);
    final isNew = !state.pending.any((p) => p.token == stamped.token);
    state = state.copyWith(pending: _merge(state.pending, [stamped]));
    // 响一声。**只为新来的那一条** —— `offer` 会被重复调到（流里来一次、
    // 恢复端点再报一次），每次都响的话同一个确认框会响两下
    if (isNew && ref.read(notifyPrefsProvider).onConfirm) {
      unawaited(playAlert());
    }
    _ensureTicking();
  }

  /// Pulls whatever the daemon still has outstanding.
  ///
  /// Failures are swallowed: this runs on reconnect, which is by definition a
  /// moment when the network was just unhealthy, and a red banner for a poll
  /// the user never asked for would be noise. A prompt missed here is picked up
  /// by the next reconnect or the next bump.
  Future<void> recover() async {
    final generation = _generation;
    try {
      final remote = await _api.pendingConfirmations();
      if (!_alive(generation)) return;
      if (remote.isEmpty && state.pending.isEmpty) return;
      state = state.copyWith(pending: _merge(state.pending, remote));
      _ensureTicking();
    } on Object {
      // Intentionally silent — see above.
    }
  }

  /// Union by token, keeping the copy already on screen.
  ///
  /// Keeping the *held* deadline matters: a recovery poll that overwrote it
  /// with a freshly computed one would restart the countdown on every reconnect
  /// flap, and the number on screen would stop meaning anything.
  static List<PendingConfirmation> _merge(
    List<PendingConfirmation> held,
    List<PendingConfirmation> incoming,
  ) {
    final byToken = {for (final c in held) c.token: c};
    for (final c in incoming) {
      byToken.putIfAbsent(c.token, () => c);
    }
    final now = DateTime.now();
    return byToken.values.where((c) => !c.isExpiredAt(now)).toList()
      ..sort((a, b) => a.deadline.compareTo(b.deadline));
  }

  // -------------------------------------------------------------- answering

  /// Delivers the user's decision.
  ///
  /// The prompt is removed only once the daemon has acknowledged, not
  /// optimistically. An optimistic removal would show "已允许" for a receipt
  /// that in fact lost the race and changed nothing — and the whole reason this
  /// dialog exists is that the user's belief about what will run has to match
  /// what runs.
  Future<void> answer(String token, {required bool allow}) async {
    final generation = _generation;
    final request = state.pending.where((c) => c.token == token).firstOrNull;
    if (request == null || state.answering.contains(token)) return;

    state = state.copyWith(answering: {...state.answering, token}, error: null);

    try {
      final accepted = await _api.answerConfirmation(
        token: token,
        allow: allow,
        // 从这条待确认自己身上取 —— 它在 `offer` 时被盖过章。云端那条路
        // 要靠它找到这一轮跑在哪个容器里
        sessionId: request.sessionId,
      );
      if (!_alive(generation)) return;
      _retire(
        request,
        accepted
            ? (allow ? ConfirmOutcome.allowed : ConfirmOutcome.denied)
            // False means the daemon said 404 — normal, not an error. Reported
            // as its own outcome so the user learns their click did not decide
            // this one, rather than being told "允许" when nothing was allowed.
            : ConfirmOutcome.superseded,
      );
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return;
      // Only real transport / server faults reach here; `answerConfirmation`
      // has already converted 404 into `false`.
      state = state.copyWith(
        answering: {...state.answering}..remove(token),
        error: '回执没能送达：${e.message}',
      );
    } on Object catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(
        answering: {...state.answering}..remove(token),
        error: '回执没能送达：$e',
      );
    }
  }

  void _retire(PendingConfirmation request, ConfirmOutcome outcome) {
    state = state.copyWith(
      pending: state.pending
          .where((c) => c.token != request.token)
          .toList(growable: false),
      answering: {...state.answering}..remove(request.token),
      resolved: [
        ConfirmResolution(
          request: request,
          outcome: outcome,
          at: DateTime.now(),
        ),
        ...state.resolved,
      ].take(ConfirmState.kResolvedMemory).toList(growable: false),
    );
    _ensureTicking();
  }

  /// Dismisses the explanatory note for a retired prompt.
  void clearResolved(String token) {
    state = state.copyWith(
      resolved: state.resolved
          .where((r) => r.request.token != token)
          .toList(growable: false),
    );
  }

  void clearError() {
    if (state.error != null) state = state.copyWith(error: null);
  }

  // ---------------------------------------------------------------- ticking

  void _ensureTicking() {
    if (state.pending.isEmpty) {
      _ticker?.cancel();
      _ticker = null;
      return;
    }
    if (_ticker != null) return;
    final generation = _generation;
    _ticker = Timer.periodic(_tick, (_) {
      if (!_alive(generation)) return;
      _sweep();
    });
  }

  /// Drops anything whose deadline passed and republishes so the countdown
  /// re-renders.
  ///
  /// Republishing every second is cheap here and only here: the confirmation
  /// panel is a handful of widgets, unlike the transcript, whose whole design
  /// is about *not* rebuilding on a timer.
  void _sweep() {
    final now = DateTime.now();
    final expired = state.pending.where((c) => c.isExpiredAt(now)).toList();
    if (expired.isEmpty) {
      // Same list contents, new identity — this is what advances the visible
      // countdown.
      state = state.copyWith(pending: [...state.pending]);
      return;
    }
    state = state.copyWith(
      pending: state.pending
          .where((c) => !c.isExpiredAt(now))
          .toList(growable: false),
      answering: {...state.answering}..removeAll(expired.map((c) => c.token)),
      resolved: [
        for (final c in expired)
          ConfirmResolution(
            request: c,
            outcome: ConfirmOutcome.expired,
            at: now,
          ),
        ...state.resolved,
      ].take(ConfirmState.kResolvedMemory).toList(growable: false),
    );
    _ensureTicking();
  }
}

final confirmControllerProvider =
    NotifierProvider<ConfirmController, ConfirmState>(ConfirmController.new);

/// 正在**等用户确认**的那些会话 id —— 界面上的第四状态。
///
/// # 为什么值得单独一个状态（和一个 provider）
///
/// running / finished / failed 三态里，只有它是「不处理就永远卡住」的：
/// 别的状态时间都在替用户干活，这个状态时间在流逝而什么都没发生。
/// 它却曾与 running 共用蓝色 —— 人扫一眼分不出「在跑」和「在等我」。
///
/// 落点必须**同色**（琥珀，`tokens.warning`），目前两处：左栏会话行的
/// 状态点、顶栏的「等你确认 · N」pill。⚠️ 设计稿还有第三处（系统托盘
/// 图标），但这个应用**目前没有托盘** —— 那一处等托盘功能立项时一起做，
/// 别为一个点先引入一整个托盘。
///
/// 派生成独立 provider 而不是各处自己 `watch(confirmControllerProvider)`
/// 再算：算法（按 sessionId 去重）要写一遍就够。
///
/// ⚠️ **select 的目标是 String，不是 Set。** `_sweep` 每秒都换一次
/// pending 的列表引用（倒计时靠它重画），而 `select` 按 `==` 比较 ——
/// Set/List 是引用相等，选它们等于每秒重建整条左栏。String 是值相等，
/// 队列内容没变时 tick 被滤掉。
final awaitingConfirmSessionsProvider = Provider<Set<String>>((ref) {
  final joined = ref.watch(
    confirmControllerProvider.select((s) {
      final ids = {for (final p in s.pending) ?p.sessionId}.toList()..sort();
      return ids.join('\n');
    }),
  );
  return joined.isEmpty ? const {} : joined.split('\n').toSet();
});

extension _FirstOrNull<E> on Iterable<E> {
  E? get firstOrNull {
    final it = iterator;
    return it.moveNext() ? it.current : null;
  }
}
