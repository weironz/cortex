import 'dart:async';
import 'dart:math' as math;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../models/sync_event.dart';
import '../models/sync_record.dart';
import 'app_providers.dart';
import 'chat_controller.dart';
import 'auth_controller.dart';
import 'confirm_controller.dart';

/// State of the realtime link to `cortexd`.
enum SyncLinkStatus {
  /// Mock data source — there is no daemon to talk to, so the whole indicator
  /// is hidden rather than shown as "broken".
  disabled,

  /// First attempt in flight; nothing has failed yet.
  connecting,

  /// `hello` received; bumps are flowing.
  live,

  /// The socket dropped and a retry is scheduled.
  reconnecting,
}

class SyncState {
  const SyncState({
    this.status = SyncLinkStatus.connecting,
    this.cursor = 0,
    this.serverCursor = 0,
    this.version,
    this.bumps = 0,
    this.resyncs = 0,
    this.attempt = 0,
    this.nextRetryAt,
    this.error,
    this.catchingUp = false,
    this.lastPulledAt,
  });

  final SyncLinkStatus status;

  /// **Our** cursor — how far `GET /sync` has actually carried us.
  final int cursor;

  /// The daemon's cursor, as last advertised over the socket. Display only.
  final int serverCursor;

  /// Daemon version from `hello`.
  final String? version;

  /// Normal increments received.
  final int bumps;

  /// Gap-suspected increments received. Broken out from [bumps] because the
  /// daemon only sends these when its Postgres notification chain hiccuped or
  /// this connection fell behind — a climbing number is an ops signal, and
  /// folding it into [bumps] would hide exactly the thing worth noticing.
  final int resyncs;

  /// Consecutive failed connection attempts. Reset by a successful `hello`.
  final int attempt;

  final DateTime? nextRetryAt;
  final String? error;

  /// A `GET /sync` drain is in flight.
  final bool catchingUp;

  final DateTime? lastPulledAt;

  /// The daemon has committed things we have not pulled yet.
  bool get isBehind => serverCursor > cursor;

  SyncState copyWith({
    SyncLinkStatus? status,
    int? cursor,
    int? serverCursor,
    Object? version = _sentinel,
    int? bumps,
    int? resyncs,
    int? attempt,
    Object? nextRetryAt = _sentinel,
    Object? error = _sentinel,
    bool? catchingUp,
    Object? lastPulledAt = _sentinel,
  }) => SyncState(
    status: status ?? this.status,
    cursor: cursor ?? this.cursor,
    serverCursor: serverCursor ?? this.serverCursor,
    version: version == _sentinel ? this.version : version as String?,
    bumps: bumps ?? this.bumps,
    resyncs: resyncs ?? this.resyncs,
    attempt: attempt ?? this.attempt,
    nextRetryAt: nextRetryAt == _sentinel
        ? this.nextRetryAt
        : nextRetryAt as DateTime?,
    error: error == _sentinel ? this.error : error as String?,
    catchingUp: catchingUp ?? this.catchingUp,
    lastPulledAt: lastPulledAt == _sentinel
        ? this.lastPulledAt
        : lastPulledAt as DateTime?,
  );

  static const Object _sentinel = Object();
}

/// Owns the `GET /ws` connection: reconnect policy, the client cursor, and the
/// fan-out of "something changed" into pane refreshes.
///
/// ## Why the socket carries no data
///
/// `cortexd` pushes signals only (see `SyncEvent`). That is not a limitation to
/// work around — it is what makes this controller correct across a disconnect:
/// there is exactly **one** catch-up path (`GET /sync?since=<our cursor>`), and
/// it is exercised on every single bump, not only after an outage. A push-the-
/// rows design would have a fast path that works and a rare slow path that
/// nobody tests.
///
/// The consequence is a rule with teeth: [_cursor] is only ever assigned from a
/// `GET /sync` response. The cursor on a `bump` is a *lag indicator*. Assigning
/// it would silently skip everything between our position and the server's.
class SyncController extends Notifier<SyncState> {
  static const _baseDelay = Duration(milliseconds: 500);
  static const _maxDelay = Duration(seconds: 20);

  /// Bumps arrive per committed transaction, and one chat turn commits several
  /// times (user episode, assistant episode, extracted facts). Refreshing a
  /// pane per commit would re-run the memory search three times in a second for
  /// no visible benefit.
  static const _refreshQuiet = Duration(milliseconds: 300);

  StreamSubscription<SyncEvent>? _subscription;
  Timer? _retryTimer;
  Timer? _refreshTimer;

  /// Invalidated on every [build]; every async continuation checks it so a
  /// backend switch cannot let the old link write into the new state.
  int _generation = 0;

  int _cursor = 0;
  bool _cursorSeeded = false;
  bool _draining = false;
  bool _drainQueued = false;
  final Set<String> _touched = {};
  final math.Random _jitter = math.Random();

  @override
  SyncState build() {
    final generation = ++_generation;
    ref.onDispose(_teardown);

    final api = ref.watch(cortexApiProvider);
    if (ref.read(appConfigProvider).useMock) {
      return const SyncState(status: SyncLinkStatus.disabled);
    }

    // ── 没登录就不连。──
    //
    // 这个 controller 是 App 一起动就被 watch 的，而它以前**无条件**开始
    // 连 WS、换 ticket —— 于是登录页阶段就有一串打向认证接口的请求，全部
    // 401。后果不只是 console 刷屏：那些 401 会敲响 `onUnauthorized`，把
    // 「凭据已失效」的红字压在一张用户还没输过密码的登录表单上 ——
    // 「无法登录」的错觉正是它造出来的。
    //
    // watch 而不是 read：登录成功（或登出）时这里要跟着重建 ——
    // 登录后自动连上，登出后自动断开，两个方向都不需要谁记得手动通知。
    if (!ref.watch(authControllerProvider.select((s) => s.isReady))) {
      return const SyncState(status: SyncLinkStatus.disabled);
    }

    // A fresh backend means a fresh log; carrying the old cursor over would
    // mean asking daemon B about positions that only exist in daemon A.
    _cursor = 0;
    _cursorSeeded = false;
    _touched.clear();

    // Cannot mutate state during build, so the first connect is deferred by a
    // microtask rather than run inline.
    Future.microtask(() {
      if (!_alive(generation)) return;
      _connect(api, generation);
    });

    return const SyncState(status: SyncLinkStatus.connecting);
  }

  bool _alive(int generation) => ref.mounted && generation == _generation;

  // ------------------------------------------------------------- connection

  void _connect(CortexApi api, int generation) {
    _subscription?.cancel();
    _subscription = api.watchSync().listen(
      (event) {
        if (_alive(generation)) _onEvent(api, event, generation);
      },
      onError: (Object error, StackTrace _) {
        if (_alive(generation)) {
          _onDropped(api, generation, _describe(error));
        }
      },
      // A clean close is still a disconnect: the daemon shutting down or a
      // proxy trimming an idle connection both land here.
      onDone: () {
        if (_alive(generation)) _onDropped(api, generation, null);
      },
      cancelOnError: true,
    );
  }

  void _onEvent(CortexApi api, SyncEvent event, int generation) {
    switch (event) {
      case SyncHello(:final cursor, :final version):
        // The client has no local store, so "catching up" means re-fetching
        // over REST, not replaying the log. That is why adopting the server
        // cursor as our starting point is safe *here* and only here.
        //
        // The moment a local SQLite cache exists this must become "read the
        // persisted cursor, 0 for a new device" — otherwise a first launch
        // would silently skip the entire history.
        if (!_cursorSeeded) {
          _cursor = cursor;
          _cursorSeeded = true;
        }
        _retryTimer?.cancel();
        _retryTimer = null;
        state = state.copyWith(
          status: SyncLinkStatus.live,
          serverCursor: cursor,
          cursor: _cursor,
          version: version,
          attempt: 0,
          nextRetryAt: null,
          error: null,
        );
        // A reconnect after downtime lands here already behind; drain the gap
        // instead of waiting for the next write to happen to produce a bump.
        if (cursor > _cursor) _drain(api, generation);

        // `hello` is the one moment the client reliably knows it has just
        // (re)established contact, which makes it the right trigger for
        // recovering tool confirmations. They are deliberately **not** pushed
        // over this socket — its contract is "signals, never data" — and the
        // SSE stream that carried the original request does not replay. So a
        // poll on reconnect is the only thing standing between a dropped
        // connection and a turn that sits suspended until it times out.
        unawaited(ref.read(confirmControllerProvider.notifier).recover());

      case SyncBump(:final cursor):
        state = state.copyWith(
          serverCursor: math.max(cursor, state.serverCursor),
          bumps: state.bumps + 1,
        );
        _drain(api, generation);

      case SyncResync(:final cursor):
        state = state.copyWith(
          serverCursor: math.max(cursor, state.serverCursor),
          resyncs: state.resyncs + 1,
        );
        _drain(api, generation);

      case SyncUnknown():
        // Forward compatible: a newer daemon event must not drop the link.
        break;
    }
  }

  void _onDropped(CortexApi api, int generation, String? error) {
    _subscription?.cancel();
    _subscription = null;

    final attempt = state.attempt + 1;
    final delay = _backoff(attempt);
    state = state.copyWith(
      status: SyncLinkStatus.reconnecting,
      attempt: attempt,
      nextRetryAt: DateTime.now().add(delay),
      error: error,
    );

    _retryTimer?.cancel();
    _retryTimer = Timer(delay, () {
      if (!_alive(generation)) return;
      state = state.copyWith(nextRetryAt: null);
      _connect(api, generation);
    });
  }

  /// Exponential backoff, capped, with ±25% jitter.
  ///
  /// The cap matters because a daemon restart takes seconds, not minutes —
  /// waiting five minutes to notice it came back is worse than a few extra
  /// probes. The jitter matters because every client (and every browser tab)
  /// disconnects at the *same instant* when the daemon goes down; without it
  /// they would all come back in lockstep and hammer the first accepting
  /// socket.
  Duration _backoff(int attempt) {
    final exponent = math.min(attempt - 1, 8);
    final raw = _baseDelay.inMilliseconds * math.pow(2, exponent).toInt();
    final capped = math.min(raw, _maxDelay.inMilliseconds);
    final spread = (capped * 0.25).round();
    final offset = spread == 0 ? 0 : _jitter.nextInt(spread * 2 + 1) - spread;
    return Duration(milliseconds: math.max(200, capped + offset));
  }

  /// Manual "try now", for the status indicator's retry affordance.
  void reconnectNow() {
    if (state.status == SyncLinkStatus.disabled) return;
    final generation = _generation;
    _retryTimer?.cancel();
    _retryTimer = null;
    state = state.copyWith(
      status: SyncLinkStatus.connecting,
      nextRetryAt: null,
      error: null,
    );
    _connect(ref.read(cortexApiProvider), generation);
  }

  // ---------------------------------------------------------------- catch-up

  /// Pulls `GET /sync` from **our** cursor until the server reports no more.
  ///
  /// Re-entrancy is collapsed rather than queued: five bumps arriving during
  /// one drain need one more drain afterwards, not five.
  Future<void> _drain(CortexApi api, int generation) async {
    if (_draining) {
      _drainQueued = true;
      return;
    }
    _draining = true;
    state = state.copyWith(catchingUp: true);

    try {
      // A daemon that reported `has_more` without advancing the cursor would
      // spin us forever; the loop exits on any non-advance instead of trusting
      // the flag.
      while (true) {
        final before = _cursor;
        final page = await api.sync(since: _cursor);
        if (!_alive(generation)) return;

        for (final record in page.records) {
          _touched.add(record.table);
        }
        if (page.cursor > _cursor) _cursor = page.cursor;

        state = state.copyWith(cursor: _cursor, lastPulledAt: DateTime.now());
        if (!page.hasMore || _cursor <= before) break;
      }
      state = state.copyWith(error: null);
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(error: e.message);
    } on Object catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(error: '$e');
    } finally {
      _draining = false;
      if (_alive(generation)) {
        state = state.copyWith(catchingUp: false);
        _scheduleRefresh(generation);
        if (_drainQueued) {
          _drainQueued = false;
          unawaited(_drain(api, generation));
        }
      }
    }
  }

  void _scheduleRefresh(int generation) {
    if (_touched.isEmpty) return;
    _refreshTimer?.cancel();
    _refreshTimer = Timer(_refreshQuiet, () {
      if (!_alive(generation)) return;
      _applyRefresh();
    });
  }

  void _applyRefresh() {
    final touched = Set<String>.from(_touched);
    _touched.clear();
    if (touched.isEmpty) return;

    if (touched.any(SyncTables.conversation.contains)) {
      final chat = ref.read(chatControllerProvider.notifier);
      unawaited(chat.loadSessions());

      // ── 正在看的那一段也要重载 ──
      //
      // `loadSessions` 里**有**一个 `_ensureTranscript(active)`，看起来这件事
      // 已经办了。它没有：那个函数的语义是「没加载过才加载」，而打开着的会话
      // 必然已经在 transcripts 里，于是它每次都提前 return。
      //
      // 症状是「同步只同步了一半」：在浏览器里聊完切到桌面端，侧栏那条已经
      // 变成「刚刚」、排到了最前面，对话却停在几分钟前的最后一句。用户看到的
      // 结论是「不是实时同步的吧」—— 而链路（WS bump → /sync）其实全程是通的，
      // 只差最后这一步没人做。2026-08-15 真机上确认。
      //
      // **本机正在流的那一轮不重载**：那条流是此刻最新的来源，中途拿服务端
      // 的快照去覆盖只会闪一下（而且助手那条 episode 还没提交，覆盖等于把
      // 刚打出来的字抹掉）。它跑完自然会有下一次 bump。
      final snapshot = ref.read(chatControllerProvider);
      final active = snapshot.activeSessionId;
      if (active != null && snapshot.streaming?.sessionId != active) {
        unawaited(chat.loadTranscript(active));
      }
    }
    // 记忆那几张表**不再触发任何客户端刷新** —— 这一侧没有记忆界面了。
    // `SyncTables.memory` 留着：它描述的是服务端会下发哪些表，
    // 与这个客户端渲不渲染它们无关。
  }

  // ------------------------------------------------------------------ teardown

  void _teardown() {
    _retryTimer?.cancel();
    _retryTimer = null;
    _refreshTimer?.cancel();
    _refreshTimer = null;
    _subscription?.cancel();
    _subscription = null;
    _draining = false;
    _drainQueued = false;
  }

  static String _describe(Object error) =>
      error is CortexApiException ? error.message : '$error';
}

final syncControllerProvider = NotifierProvider<SyncController, SyncState>(
  SyncController.new,
);
