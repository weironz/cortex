import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../api/http_cortex_api.dart';
import '../../../api/mock_cortex_api.dart';
import '../../../core/computer_trace_api.dart';
import '../../../core/theme.dart';
import '../../../models/chat_message.dart';
import '../../../models/pending_confirmation.dart';
import '../../../models/tool_call.dart';
import '../../../state/app_providers.dart';
import '../../../state/auth_controller.dart';
import '../../../state/chat_controller.dart';
import '../../../state/confirm_controller.dart';

/// 缺口不是一次请求的错误：503、刷新或离开会话都不能证明它消失。
/// 状态留在当前 ProviderContainer，并以 API 对象身份和会话共同隔离。
final _computerTraceRegistryProvider = Provider<_ComputerTraceRegistry>((ref) {
  final registry = _ComputerTraceRegistry();
  ref.onDispose(registry.dispose);
  return registry;
});

typedef _ComputerTraceIdentity = ({
  Uri endpoint,
  Uri? deployment,
  String userId,
});

/// 身份查询直连部署，避免本机 agent 的出站凭据热更新尚未完成时误认账号。
/// 每个 API 实例重新验证；只保存无凭据的地址和服务端确认的 userId。
final _computerTraceIdentityProvider = FutureProvider.autoDispose
    .family<_ComputerTraceIdentity?, HttpCortexApi>((ref, api) async {
      final backend = api.computerTraceBackendIdentity;
      final currentApi = ref.watch(cortexApiProvider);
      final phase = ref.watch(
        authControllerProvider.select((state) => state.phase),
      );
      final config = ref.watch(appConfigProvider);
      if (backend == null ||
          !identical(currentApi, api) ||
          phase != AuthPhase.ready ||
          config.offline ||
          !api.computerTraceMatchesDeployment(config.baseUrl)) {
        return null;
      }
      final probe = ref.watch(authProbeApiProvider)(
        ref.read(authControllerProvider).token,
      );
      ref.onDispose(probe.dispose);
      try {
        if (probe is! HttpCortexApi ||
            probe.frontsDeployment != null ||
            !probe.computerTraceMatchesDeployment(config.baseUrl)) {
          return null;
        }
        final userId = await probe.computerTraceVerifiedUserId().timeout(
          const Duration(seconds: 8),
        );
        if (!ref.mounted ||
            !identical(ref.read(cortexApiProvider), api) ||
            userId == null) {
          return null;
        }
        return (
          endpoint: backend.endpoint,
          deployment: backend.deployment,
          userId: userId,
        );
      } on Object {
        return null;
      } finally {
        probe.dispose();
      }
    }, retry: (_, _) => null);

final _computerTraceHistoryProvider = StreamProvider.autoDispose
    .family<_ComputerTraceSnapshot, String>(
      (ref, sessionId) {
        final api = ref.watch(cortexApiProvider);
        _ComputerTraceIdentity? identity;
        if (api is HttpCortexApi) {
          final verification = ref.watch(_computerTraceIdentityProvider(api));
          if (verification.isLoading ||
              verification.hasError ||
              verification.value == null) {
            return Stream.value(
              _ComputerTraceSnapshot(
                api: api,
                history: null,
                error: null,
                gap: null,
                uncertain: true,
                loading: false,
              ),
            );
          }
          identity = verification.value;
        } else if (api is! MockCortexApi) {
          return Stream.value(
            _ComputerTraceSnapshot(
              api: api,
              history: null,
              error: null,
              gap: null,
              uncertain: true,
              loading: false,
            ),
          );
        }
        return ref
            .watch(_computerTraceRegistryProvider)
            .entry(api, sessionId, identity: identity)
            .watch();
      },
      // 重试由每个会话的条目负责，不能复用 Riverpod 跨错误类型的计数。
      retry: (_, _) => null,
    );

class _ComputerTraceSnapshot {
  const _ComputerTraceSnapshot({
    required this.api,
    required this.history,
    required this.error,
    required this.gap,
    required this.uncertain,
    required this.loading,
  });

  final Object api;
  final ComputerUseTraceHistory? history;
  final Object? error;
  final CortexApiException? gap;
  final bool uncertain;
  final bool loading;

  String? get displayError {
    final gap = this.gap;
    if (gap == null) {
      if (!uncertain) return error?.toString();
      return '历史完整性尚未重新确认${error == null ? '' : '：$error'}';
    }
    if (error == null || identical(error, gap)) return '$gap';
    return '$gap；最近一次复查：$error';
  }
}

class _ComputerTraceRegistry {
  // 不能因为旧 API 留有缺口就一直强持有它及其凭据。API 仍可用时，Expando
  // 按对象身份保留缺口；API 不再可达时，关联状态也可以被回收。
  final _entries = Expando<Map<String, _ComputerTraceEntry>>();
  final _verifiedEntries =
      <_ComputerTraceIdentity, Map<String, _ComputerTraceEntry>>{};
  final _active = <_ComputerTraceEntry>{};

  _ComputerTraceEntry entry(
    Object api,
    String sessionId, {
    _ComputerTraceIdentity? identity,
  }) {
    final sessions = identity == null
        ? (_entries[api] ??= {})
        : _verifiedEntries.putIfAbsent(identity, () => {});
    final entry = sessions.putIfAbsent(
      sessionId,
      () => _ComputerTraceEntry(api, sessionId, identity, _prune),
    );
    entry.bind(api);
    _active.add(entry);
    return entry;
  }

  void _prune(_ComputerTraceEntry entry) {
    if (!entry.isIdle) return;
    _active.remove(entry);
    if (!entry.canDiscard) {
      if (entry.identity != null) entry.releaseApi();
      return;
    }
    final identity = entry.identity;
    final sessions = identity == null
        ? _entries[entry.api]
        : _verifiedEntries[identity];
    if (!identical(sessions?[entry.sessionId], entry)) return;
    sessions!.remove(entry.sessionId);
    if (sessions.isEmpty) {
      if (identity == null) {
        _entries[entry.api] = null;
      } else {
        _verifiedEntries.remove(identity);
      }
    }
    entry.dispose();
  }

  void dispose() {
    for (final entry in _active.toList()) {
      entry.dispose();
    }
    _active.clear();
    _verifiedEntries.clear();
  }
}

class _ComputerTraceCandidate {
  const _ComputerTraceCandidate(this.request, this.gapRevision);

  final int request;
  final int gapRevision;
}

class _UnsupportedComputerTraceApi implements Exception {
  @override
  String toString() => '当前后端没有本机 Computer Use trace 历史端点';
}

class _ComputerTraceEntry {
  _ComputerTraceEntry(Object api, this.sessionId, this.identity, this._prune)
    : _api = api;

  Object? _api;
  Object get api => _api!;
  final String sessionId;
  final _ComputerTraceIdentity? identity;
  final void Function(_ComputerTraceEntry) _prune;
  final _changes = StreamController<_ComputerTraceSnapshot>.broadcast();
  final _inFlight = <int>{};
  // 不支持取消的传输可能永久不结束；保留观察以接收迟到的 409，不能假装已释放。
  final _underlying = <int>{};
  final _abortSignals = <int, Completer<void>>{};
  final _deadlines = <int, Timer>{};
  int _listeners = 0;
  int _latestRequest = 0;
  int _gapRevision = 0;
  int _gapRetries = 0;
  int _networkRetries = 0;
  bool _disposed = false;
  bool _loading = false;
  bool _terminal = false;
  bool _uncertain = false;
  bool _timerIsGap = false;
  Timer? _timer;
  ComputerUseTraceHistory? _history;
  Object? _error;
  CortexApiException? _gap;
  _ComputerTraceCandidate? _candidate;

  bool get isIdle => _listeners == 0 && _underlying.isEmpty;
  bool get canDiscard => isIdle && _gap == null && !_uncertain;

  void bind(Object api) => _api = api;

  void releaseApi() {
    _api = null;
    _error = null;
  }

  _ComputerTraceSnapshot get _snapshot => _ComputerTraceSnapshot(
    api: api,
    history: _history,
    error: _error,
    gap: _gap,
    uncertain: _uncertain,
    loading: _loading,
  );

  Stream<_ComputerTraceSnapshot> watch() => Stream.multi((controller) {
    _listeners++;
    // 每次打开或显式刷新都重新查询，但保留已经观察到的缺口。
    _refresh();
    final subscription = _changes.stream.listen(controller.add);
    controller.add(_snapshot);
    controller.onCancel = () {
      _listeners--;
      if (_listeners == 0) {
        _timer?.cancel();
        _timer = null;
        _history = null;
        _candidate = null;
        for (final signal in _abortSignals.values) {
          if (!signal.isCompleted) signal.complete();
        }
      }
      _prune(this);
      return subscription.cancel();
    };
  });

  void _refresh() {
    _timer?.cancel();
    _timer = null;
    _gapRetries = 0;
    _networkRetries = 0;
    _terminal = false;
    _startRequest();
  }

  void _emit() {
    if (!_disposed) _changes.add(_snapshot);
  }

  void _startRequest() {
    if (_disposed) return;
    final request = ++_latestRequest;
    final gapRevision = _gapRevision;
    _candidate = null;
    _inFlight.add(request);
    _underlying.add(request);
    final abort = Completer<void>();
    _abortSignals[request] = abort;
    _deadlines[request] = Timer(const Duration(seconds: 15), () {
      _deadlines.remove(request);
      if (_disposed || !_inFlight.remove(request)) return;
      _abortSignals.remove(request);
      if (!abort.isCompleted) abort.complete();
      // 超时不能当作旧请求的成功；后续证明必须在超时之后重新发起。
      _gapRevision++;
      _candidate = null;
      _uncertain = true;
      _history = null;
      if (request == _latestRequest) {
        _loading = false;
        _error = const CortexApiException('Computer Use trace 查询超时');
        _scheduleRetry(gap: false);
      } else {
        _scheduleRetry(gap: _gap != null);
      }
      _emit();
    });
    _loading = true;
    _emit();
    unawaited(_query(request, gapRevision, abort.future));
  }

  Future<void> _query(int request, int gapRevision, Future<void> abort) async {
    try {
      final api = this.api;
      if (api is! ComputerTraceApi) {
        throw _UnsupportedComputerTraceApi();
      }
      final history = await (api is AbortableComputerTraceApi
          ? (api as AbortableComputerTraceApi).computerUseTracesAbortable(
              sessionId,
              abortTrigger: abort,
            )
          : api.computerUseTraces(sessionId));
      if (_disposed) return;
      _underlying.remove(request);
      if (!_inFlight.remove(request)) {
        _acceptCandidateIfSafe();
        _emit();
        return;
      }
      if (request == _latestRequest) {
        _loading = false;
        _error = null;
        if (_gap == null && !_uncertain) {
          _history = _listeners == 0 ? null : history;
          _gapRetries = 0;
          _networkRetries = 0;
        } else if (gapRevision == _gapRevision) {
          if (_underlying.isEmpty) {
            _acceptHistory(history);
          } else {
            // 缺口期间不持有隐藏历史。旧请求结算后重查，不能复用这里的结果。
            _candidate = _ComputerTraceCandidate(request, gapRevision);
          }
        } else {
          // 这个成功请求发出后又观察到了 409，不能用它清除新缺口。
          _scheduleRetry(gap: _gap != null);
        }
      } else {
        _acceptCandidateIfSafe();
      }
      _emit();
    } catch (error) {
      if (_disposed) return;
      _underlying.remove(request);
      final wasInFlight = _inFlight.remove(request);
      final isGap = error is CortexApiException && error.statusCode == 409;
      if (!wasInFlight && !isGap) {
        _acceptCandidateIfSafe();
        _emit();
        return;
      }
      if (isGap) {
        // 即使请求已经被刷新取代，迟到的 409 也是尚未消除的证据。
        _gap = CortexApiException(error.message, statusCode: 409);
        _history = null;
        _gapRevision++;
        _candidate = null;
      }
      if (wasInFlight && request == _latestRequest) {
        _loading = false;
        _error = isGap ? _gap : error;
        if (isGap) {
          _scheduleRetry(gap: true);
        } else if (_isTerminal(error)) {
          _terminal = true;
          _timer?.cancel();
          _timer = null;
        } else {
          _scheduleRetry(gap: false);
        }
      } else if (isGap) {
        _scheduleRetry(gap: true);
      } else {
        _acceptCandidateIfSafe();
      }
      _emit();
    } finally {
      _deadlines.remove(request)?.cancel();
      _abortSignals.remove(request);
      _underlying.remove(request);
      if (!_disposed) _prune(this);
    }
  }

  void _acceptCandidateIfSafe() {
    final candidate = _candidate;
    if (candidate == null ||
        _underlying.isNotEmpty ||
        candidate.request != _latestRequest ||
        candidate.gapRevision != _gapRevision) {
      return;
    }
    _candidate = null;
    if (_listeners > 0 && !_terminal) _startRequest();
  }

  void _acceptHistory(ComputerUseTraceHistory history) {
    _history = _listeners == 0 ? null : history;
    _gap = null;
    _uncertain = false;
    _candidate = null;
    _gapRetries = 0;
    _networkRetries = 0;
    _timer?.cancel();
    _timer = null;
  }

  // 本机 history 的 5xx 目前也返回 retryable=false；遵从服务端声明，需手动刷新。
  bool _isTerminal(Object error) =>
      error is _UnsupportedComputerTraceApi ||
      error is StateError ||
      (error is CortexApiException &&
          (error.isUnsupported ||
              error.isUnauthorized ||
              error.isDeterministic));

  void _scheduleRetry({required bool gap}) {
    if (_listeners == 0 || _disposed || _terminal) return;
    const gapDelays = [1, 3, 8];
    const networkDelays = [1, 3, 8, 20, 30];
    // 预算耗尽时不能取消已经排好的网络重试。
    if (gap && _gapRetries >= gapDelays.length) return;
    if (_timer != null) {
      if (!gap || _timerIsGap) return;
      _timer!.cancel();
      _timer = null;
    }
    final seconds = gap
        ? gapDelays[_gapRetries++]
        : networkDelays[(_networkRetries++).clamp(0, networkDelays.length - 1)];
    _timerIsGap = gap;
    _timer = Timer(Duration(seconds: seconds), () {
      _timer = null;
      if (_listeners > 0) _startRequest();
    });
  }

  void dispose() {
    if (_disposed) return;
    _disposed = true;
    _timer?.cancel();
    for (final deadline in _deadlines.values) {
      deadline.cancel();
    }
    for (final signal in _abortSignals.values) {
      if (!signal.isCompleted) signal.complete();
    }
    unawaited(_changes.close());
  }
}

/// Computer Use 工具名的唯一客户端白名单。
///
/// 旧坐标工具也收进来：它们仍可能出现在升级前未结束的会话里，审计视图不能
/// 因为工具目录换代就把已经发生过的电脑操作从历史里抹掉。
bool isComputerUseToolName(String name) =>
    name.startsWith('computer_') ||
    name == 'screenshot' ||
    name == 'click' ||
    name == 'type_text' ||
    name == 'key' ||
    name == 'scroll';

enum _TraceIdentityStatus { checking, verified, unverified }

_TraceIdentityStatus _traceIdentityStatus(WidgetRef ref, Object api) {
  if (api is HttpCortexApi) {
    final identity = ref.watch(_computerTraceIdentityProvider(api));
    if (identity.isLoading) return _TraceIdentityStatus.checking;
    return identity.hasError || identity.value == null
        ? _TraceIdentityStatus.unverified
        : _TraceIdentityStatus.verified;
  }
  return api is MockCortexApi
      ? _TraceIdentityStatus.verified
      : _TraceIdentityStatus.unverified;
}

bool _isVisibleHistoryError(Object? error) =>
    error != null &&
    error is! _UnsupportedComputerTraceApi &&
    !(error is CortexApiException && error.isUnsupported);

/// 聊天页顶栏里的 Computer Use 轨迹入口。
///
/// 有电脑工具、确认记录或本机历史时才占位置。派发前拒绝可能没有持久化的
/// 工具事件，因此重开会话后也要查询历史，才能找到那条拒绝记录。
class ComputerUseTraceButton extends ConsumerWidget {
  const ComputerUseTraceButton({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    ref.listen(chatControllerProvider.select((state) => state.streamingTurns), (
      previous,
      next,
    ) {
      for (final sessionId in previous?.keys ?? const <String>[]) {
        if (!next.containsKey(sessionId)) {
          ref.invalidate(
            _computerTraceHistoryProvider(sessionId),
            asReload: true,
          );
        }
      }
    });
    final hasToolTrace = ref.watch(
      chatControllerProvider.select(
        (state) =>
            state.activeTranscript.any(
              (message) => message.toolCalls.any(
                (call) => isComputerUseToolName(call.name),
              ),
            ) ||
            (state.streaming?.toolCalls.any(
                  (call) => isComputerUseToolName(call.name),
                ) ??
                false),
      ),
    );
    final sessionId = ref.watch(
      chatControllerProvider.select((state) => state.activeSessionId),
    );
    final hasConfirmation = ref.watch(
      confirmControllerProvider.select(
        (state) =>
            state.pending.any(
              (request) =>
                  isComputerUseToolName(request.tool) &&
                  (sessionId == null || request.sessionId == sessionId),
            ) ||
            state.resolved.any(
              (resolution) =>
                  isComputerUseToolName(resolution.request.tool) &&
                  (sessionId == null ||
                      resolution.request.sessionId == sessionId),
            ),
      ),
    );
    final history = !hasToolTrace && !hasConfirmation && sessionId != null
        ? ref.watch(_computerTraceHistoryProvider(sessionId))
        : null;
    final api = ref.watch(cortexApiProvider);
    final identityStatus = _traceIdentityStatus(ref, api);
    final identityUnverified =
        identityStatus == _TraceIdentityStatus.unverified;
    final snapshot = history?.value;
    // Riverpod 重建时可能暂留旧值，不能把上一个 API 的缺口带进新后端。
    final current = snapshot != null && identical(snapshot.api, api)
        ? snapshot
        : null;
    final providerError =
        history?.hasError == true && history?.isLoading == false
        ? history?.error
        : null;
    final hasHistory =
        (current?.history?.traces.isNotEmpty ?? false) ||
        current?.gap != null ||
        (current?.uncertain ?? false) ||
        _isVisibleHistoryError(current?.error) ||
        _isVisibleHistoryError(providerError) ||
        (sessionId != null && identityStatus != _TraceIdentityStatus.verified);
    if (!hasToolTrace && !hasConfirmation && !hasHistory) {
      return const SizedBox.shrink();
    }

    return IconButton(
      key: const ValueKey('computer_use:trace'),
      onPressed: () {
        if (api is HttpCortexApi && identityUnverified) {
          ref.invalidate(_computerTraceIdentityProvider(api));
        }
        if (sessionId != null) {
          ref.invalidate(
            _computerTraceHistoryProvider(sessionId),
            asReload: true,
          );
        }
        showDialog<void>(
          context: context,
          builder: (_) => const _ComputerUseTraceDialog(),
        );
      },
      iconSize: 18,
      tooltip: 'Computer Use 轨迹',
      icon: const Icon(Icons.account_tree_outlined),
    );
  }
}

class _ComputerUseTraceDialog extends ConsumerWidget {
  const _ComputerUseTraceDialog();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final transcript = ref.watch(
      chatControllerProvider.select((state) => state.activeTranscript),
    );
    final sessionId = ref.watch(
      chatControllerProvider.select((state) => state.activeSessionId),
    );
    final liveTools = ref.watch(
      chatControllerProvider.select(
        (state) => state.streaming?.toolCalls ?? const <ToolCall>[],
      ),
    );
    final liveStartedAt = ref.watch(
      chatControllerProvider.select((state) => state.streaming?.startedAt),
    );
    final liveMessageId = ref.watch(
      chatControllerProvider.select((state) => state.streaming?.messageId),
    );
    final pending = ref.watch(
      confirmControllerProvider.select((state) => state.pending),
    );
    final resolved = ref.watch(
      confirmControllerProvider.select((state) => state.resolved),
    );
    final history = sessionId == null
        ? null
        : ref.watch(_computerTraceHistoryProvider(sessionId));
    final api = ref.watch(cortexApiProvider);
    final identityStatus = _traceIdentityStatus(ref, api);
    final identityChecking = identityStatus == _TraceIdentityStatus.checking;
    final identityUnverified =
        identityStatus == _TraceIdentityStatus.unverified;
    final snapshot = history?.value;
    final current = snapshot != null && identical(snapshot.api, api)
        ? snapshot
        : null;
    final providerError =
        history?.hasError == true && history?.isLoading == false
        ? history?.error
        : null;

    return AlertDialog(
      title: Row(
        children: [
          const Icon(Icons.account_tree_outlined, size: 20),
          const SizedBox(width: 9),
          const Expanded(child: Text('Computer Use 轨迹')),
          DecoratedBox(
            decoration: BoxDecoration(
              color: Theme.of(context).colorScheme.surfaceContainerHighest,
              // 不走圆角五阶：只读标记是胶囊，两端保持全圆。
              borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
            ),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
              child: Text(
                '只读',
                style: Theme.of(
                  context,
                ).textTheme.labelSmall?.copyWith(fontWeight: FontWeight.w700),
              ),
            ),
          ),
        ],
      ),
      contentPadding: const EdgeInsets.fromLTRB(24, 8, 24, 0),
      content: SizedBox(
        width: 760,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxHeight: 560),
          child: identityChecking
              ? const _HistoryLoading(message: '正在验证审计账号与后端…')
              : ComputerUseTraceView(
                  messages: transcript,
                  streamingTools: liveTools,
                  streamingStartedAt: liveStartedAt,
                  streamingMessageId: liveMessageId,
                  history: identityUnverified ? null : current?.history,
                  historyLoading:
                      !identityUnverified &&
                      sessionId != null &&
                      ((history?.isLoading ?? true) ||
                          (current?.loading ?? true)),
                  historyError: identityUnverified
                      ? '无法确认当前审计账号与后端，历史完整性需要人工核对'
                            '${current?.displayError == null ? '' : '：${current!.displayError}'}'
                      : current?.displayError ?? providerError?.toString(),
                  pendingConfirmations: pending
                      .where(
                        (request) =>
                            sessionId == null || request.sessionId == sessionId,
                      )
                      .toList(growable: false),
                  resolvedConfirmations: resolved
                      .where(
                        (resolution) =>
                            sessionId == null ||
                            resolution.request.sessionId == sessionId,
                      )
                      .toList(growable: false),
                ),
        ),
      ),
      actions: [
        if (sessionId != null)
          TextButton.icon(
            key: const ValueKey('computer_use:trace-refresh'),
            onPressed: () {
              if (api is HttpCortexApi && identityUnverified) {
                ref.invalidate(_computerTraceIdentityProvider(api));
              }
              ref.invalidate(
                _computerTraceHistoryProvider(sessionId),
                asReload: true,
              );
            },
            icon: const Icon(Icons.refresh, size: 17),
            label: const Text('刷新'),
          ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('关闭'),
        ),
      ],
    );
  }
}

/// 当前会话的只读审计视图。
///
/// 输入全部来自现有 [ChatMessage.toolCalls]、流式 [ToolCall] 和确认队列。
/// 不显示工具 `output` 原文：同一个 JSON 里既有结构化观察，也可能夹着窗口
/// 标题、元素值或输入框内容。这里只按白名单提取数量、布尔状态和稳定枚举。
class ComputerUseTraceView extends StatelessWidget {
  const ComputerUseTraceView({
    super.key,
    required this.messages,
    this.streamingTools = const [],
    this.streamingStartedAt,
    this.streamingMessageId,
    this.history,
    this.historyLoading = false,
    this.historyError,
    this.pendingConfirmations = const [],
    this.resolvedConfirmations = const [],
  });

  final List<ChatMessage> messages;
  final List<ToolCall> streamingTools;
  final DateTime? streamingStartedAt;
  final String? streamingMessageId;
  final ComputerUseTraceHistory? history;
  final bool historyLoading;
  final String? historyError;
  final List<PendingConfirmation> pendingConfirmations;
  final List<ConfirmResolution> resolvedConfirmations;

  @override
  Widget build(BuildContext context) {
    final backendHistory = history;
    if (historyLoading && historyError == null) {
      return const _HistoryLoading();
    }
    if (historyError == null &&
        backendHistory != null &&
        backendHistory.traces.isNotEmpty) {
      return _BackendTraceHistoryView(
        history: backendHistory,
        refreshing: historyLoading,
      );
    }
    final turns = _collectTurns();
    final allCalls = [for (final turn in turns) ...turn.calls];
    final readonly = allCalls.where(_isReadonly).length;
    final mutating = allCalls.length - readonly;
    final verified = allCalls
        .where((call) => _verification(call).passed)
        .length;
    final aborted = [
      for (final turn in turns)
        for (final call in turn.calls)
          if (call.failed || (!turn.live && call.pending)) call,
    ].length;
    final confirmations = <_ConfirmationView>[
      for (final request in pendingConfirmations)
        if (isComputerUseToolName(request.tool))
          _ConfirmationView(
            tool: request.tool,
            risk: request.risk,
            decision: '等待用户确认',
            pending: true,
          ),
      for (final resolution in resolvedConfirmations)
        if (isComputerUseToolName(resolution.request.tool))
          _ConfirmationView(
            tool: resolution.request.tool,
            risk: resolution.request.risk,
            decision: _confirmOutcomeLabel(resolution.outcome),
            pending: false,
          ),
    ];

    if (allCalls.isEmpty && confirmations.isEmpty) {
      if (historyError == null) return const _EmptyTrace();
      return Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _BoundaryNote(text: '本机历史 trace 不可用：$historyError。请人工核对。'),
          const Padding(
            padding: EdgeInsets.symmetric(vertical: 24, horizontal: 4),
            child: Text('当前客户端没有可用的工具摘要，无法据此判断操作是否发生。'),
          ),
        ],
      );
    }

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Scrollbar(
      child: SingleChildScrollView(
        key: const ValueKey('computer_use:trace-scroll'),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _BoundaryNote(
              text:
                  '本机历史 trace 尚不可用'
                  '${historyError == null ? '或为空' : '：$historyError'}。以下回退为'
                  '当前客户端保留的工具摘要；旧摘要不含策略版本、驱动回执、'
                  '完整验证和 abort 原因。原始截图、输入文字与 cookie/token '
                  '始终不展示。',
            ),
            const SizedBox(height: 12),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                _CountChip(label: '观察/只读', value: readonly),
                _CountChip(label: '动作', value: mutating),
                _CountChip(label: '已验证', value: verified),
                _CountChip(label: '失败/中止', value: aborted),
              ],
            ),
            const SizedBox(height: 16),
            _SectionTitle(
              icon: Icons.gavel_outlined,
              title: '策略与确认',
              trailing: '${confirmations.length} 条客户端记录',
            ),
            const SizedBox(height: 7),
            Container(
              width: double.infinity,
              padding: const EdgeInsets.all(12),
              decoration: BoxDecoration(
                color: scheme.surfaceContainerLow,
                borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
                border: Border.all(
                  color: scheme.outlineVariant.withValues(alpha: 0.75),
                ),
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    '只读调用 $readonly 次；会改变状态的调用 $mutating 次。'
                    '策略版本和 allowlist 规则版本未随 UI 事件下发。',
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                      height: 1.45,
                    ),
                  ),
                  if (confirmations.isEmpty) ...[
                    const SizedBox(height: 7),
                    Text(
                      '本客户端没有保留可显示的确认记录。这不等于工具没有经过策略检查。',
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                        height: 1.45,
                      ),
                    ),
                  ] else ...[
                    const SizedBox(height: 10),
                    for (final confirmation in confirmations)
                      _ConfirmationRow(view: confirmation),
                  ],
                ],
              ),
            ),
            const SizedBox(height: 18),
            if (turns.isEmpty)
              Text(
                '只有确认记录，还没有对应的工具调用到达。',
                style: theme.textTheme.bodyMedium?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              )
            else
              for (var i = turns.length - 1; i >= 0; i--)
                _TurnSection(index: i + 1, turn: turns[i], total: turns.length),
          ],
        ),
      ),
    );
  }

  List<_TurnTrace> _collectTurns() {
    final turns = <_TurnTrace>[];
    for (final message in messages) {
      final calls = message.toolCalls
          .where((call) => isComputerUseToolName(call.name))
          .toList(growable: false);
      if (calls.isEmpty) continue;
      turns.add(
        _TurnTrace(
          id: message.id,
          startedAt: message.createdAt,
          calls: calls,
          live: false,
        ),
      );
    }
    final live = streamingTools
        .where((call) => isComputerUseToolName(call.name))
        .toList(growable: false);
    if (live.isNotEmpty) {
      turns.add(
        _TurnTrace(
          id: streamingMessageId ?? 'streaming',
          startedAt: streamingStartedAt ?? DateTime.now(),
          calls: live,
          live: true,
        ),
      );
    }
    return turns;
  }
}

class _HistoryLoading extends StatelessWidget {
  const _HistoryLoading({this.message = '正在读取本机脱敏 trace…'});

  final String message;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 32),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          const SizedBox(
            width: 18,
            height: 18,
            child: CircularProgressIndicator(strokeWidth: 2),
          ),
          const SizedBox(width: 10),
          Text(message),
        ],
      ),
    );
  }
}

class _BackendTraceHistoryView extends StatelessWidget {
  const _BackendTraceHistoryView({
    required this.history,
    required this.refreshing,
  });

  final ComputerUseTraceHistory history;
  final bool refreshing;

  @override
  Widget build(BuildContext context) {
    final eventCount = history.traces.fold<int>(
      0,
      (total, trace) => total + trace.events.length,
    );
    return Scrollbar(
      child: SingleChildScrollView(
        key: const ValueKey('computer_use:backend-history'),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _BoundaryNote(
              text:
                  '本视图来自本机 agent 写入的脱敏 JSONL，并按只读协议白名单显示。'
                  '原始截图、输入全文、安全字段、cookie/token 与剪贴板内容不会'
                  '进入响应；截图 hash 与动作目标引用仍可用于审计。'
                  '新生成的确认预览、验证条件和 URL 指纹不能跨进程比较。',
            ),
            const SizedBox(height: 12),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                _CountChip(label: '历史 trace', value: history.traces.length),
                _CountChip(label: '事件', value: eventCount),
              ],
            ),
            if (refreshing) ...[
              const SizedBox(height: 9),
              const LinearProgressIndicator(minHeight: 2),
            ],
            const SizedBox(height: 15),
            for (var index = history.traces.length - 1; index >= 0; index--)
              _BackendTraceCard(
                index: index + 1,
                total: history.traces.length,
                trace: history.traces[index],
              ),
          ],
        ),
      ),
    );
  }
}

class _BackendTraceCard extends StatelessWidget {
  const _BackendTraceCard({
    required this.index,
    required this.total,
    required this.trace,
  });

  final int index;
  final int total;
  final ComputerUseTrace trace;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      key: ValueKey('computer_use:backend-trace:${trace.runId}:$index'),
      width: double.infinity,
      margin: const EdgeInsets.only(bottom: 12),
      padding: const EdgeInsets.all(13),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerLow,
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  'trace $index / $total · ${trace.runId}',
                  style: theme.textTheme.labelLarge?.copyWith(
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ),
              Text(
                trace.outcome,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: trace.outcome == 'completed'
                      ? scheme.primary
                      : scheme.onSurfaceVariant,
                  fontWeight: FontWeight.w700,
                ),
              ),
            ],
          ),
          const SizedBox(height: 8),
          _TraceLine(label: '策略版本', value: trace.policyVersion),
          _TraceLine(label: '会话', value: trace.sessionId),
          for (final field in trace.fields)
            _TraceLine(label: field.label, value: field.value),
          const SizedBox(height: 8),
          for (final event in trace.events) _BackendEventCard(event: event),
        ],
      ),
    );
  }
}

class _BackendEventCard extends StatelessWidget {
  const _BackendEventCard({required this.event});

  final ComputerUseTraceEvent event;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      key: ValueKey(
        'computer_use:backend-event:${event.sequence}:${event.type}',
      ),
      width: double.infinity,
      margin: const EdgeInsets.only(top: 6),
      padding: const EdgeInsets.fromLTRB(10, 8, 10, 4),
      decoration: BoxDecoration(
        color: scheme.surface,
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        border: Border.all(
          color: scheme.outlineVariant.withValues(alpha: 0.75),
        ),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            '${event.sequence} · ${event.title}',
            style: theme.textTheme.labelSmall?.copyWith(
              fontWeight: FontWeight.w700,
              color: scheme.onSurfaceVariant,
            ),
          ),
          const SizedBox(height: 5),
          for (final field in event.fields)
            _TraceLine(label: field.label, value: field.value),
        ],
      ),
    );
  }
}

class _TurnTrace {
  const _TurnTrace({
    required this.id,
    required this.startedAt,
    required this.calls,
    required this.live,
  });

  final String id;
  final DateTime startedAt;
  final List<ToolCall> calls;
  final bool live;
}

class _ConfirmationView {
  const _ConfirmationView({
    required this.tool,
    required this.risk,
    required this.decision,
    required this.pending,
  });

  final String tool;
  final String risk;
  final String decision;
  final bool pending;
}

class _BoundaryNote extends StatelessWidget {
  const _BoundaryNote({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      key: const ValueKey('computer_use:trace-boundary'),
      width: double.infinity,
      padding: const EdgeInsets.all(13),
      decoration: BoxDecoration(
        color: scheme.secondaryContainer.withValues(alpha: 0.5),
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        border: Border.all(color: scheme.secondary.withValues(alpha: 0.35)),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(
            Icons.privacy_tip_outlined,
            size: 18,
            color: scheme.onSecondaryContainer,
          ),
          const SizedBox(width: 9),
          Expanded(
            child: Text(
              text,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSecondaryContainer,
                height: 1.5,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _CountChip extends StatelessWidget {
  const _CountChip({required this.label, required this.value});

  final String label;
  final int value;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return DecoratedBox(
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest,
        // 不走圆角五阶：数量标签是胶囊，两端保持全圆。
        borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
      ),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
        child: Text(
          '$label $value',
          style: theme.textTheme.labelSmall?.copyWith(
            color: scheme.onSurfaceVariant,
            fontWeight: FontWeight.w600,
          ),
        ),
      ),
    );
  }
}

class _SectionTitle extends StatelessWidget {
  const _SectionTitle({required this.icon, required this.title, this.trailing});

  final IconData icon;
  final String title;
  final String? trailing;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Row(
      children: [
        Icon(icon, size: 16, color: scheme.onSurfaceVariant),
        const SizedBox(width: 7),
        Text(
          title,
          style: theme.textTheme.labelLarge?.copyWith(
            fontWeight: FontWeight.w700,
          ),
        ),
        if (trailing != null) ...[
          const Spacer(),
          Text(
            trailing!,
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ],
    );
  }
}

class _ConfirmationRow extends StatelessWidget {
  const _ConfirmationRow({required this.view});

  final _ConfirmationView view;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Padding(
      padding: const EdgeInsets.only(top: 6),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Container(
            width: 7,
            height: 7,
            margin: const EdgeInsets.only(top: 5),
            decoration: BoxDecoration(
              color: view.pending ? scheme.primary : scheme.outline,
              shape: BoxShape.circle,
            ),
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              '${_toolLabel(view.tool)} · ${_riskLabel(view.risk)} · ${view.decision}',
              style: theme.textTheme.bodySmall?.copyWith(height: 1.4),
            ),
          ),
        ],
      ),
    );
  }
}

class _TurnSection extends StatelessWidget {
  const _TurnSection({
    required this.index,
    required this.turn,
    required this.total,
  });

  final int index;
  final _TurnTrace turn;
  final int total;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final detailed = turn.calls.any((call) => call.output != null);
    final badge = turn.live
        ? '当前轮'
        : detailed
        ? '本机内存明细'
        : '历史摘要';

    return Padding(
      padding: const EdgeInsets.only(bottom: 18),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Text(
                '第 $index / $total 轮',
                style: theme.textTheme.labelMedium?.copyWith(
                  fontWeight: FontWeight.w700,
                ),
              ),
              const Spacer(),
              DecoratedBox(
                decoration: BoxDecoration(
                  color: scheme.surfaceContainerHighest,
                  // 不走圆角五阶：轮次状态是胶囊，两端保持全圆。
                  borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
                ),
                child: Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 8,
                    vertical: 3,
                  ),
                  child: Text(
                    badge,
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: 7),
          for (var i = 0; i < turn.calls.length; i++)
            _TraceCallCard(
              index: i + 1,
              call: turn.calls[i],
              turnEnded: !turn.live,
            ),
        ],
      ),
    );
  }
}

class _TraceCallCard extends StatelessWidget {
  const _TraceCallCard({
    required this.index,
    required this.call,
    required this.turnEnded,
  });

  final int index;
  final ToolCall call;
  final bool turnEnded;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final verification = _verification(call);
    final mutating = !_isReadonly(call);

    return Container(
      key: ValueKey('computer_use:trace-call:${call.name}:$index'),
      width: double.infinity,
      margin: const EdgeInsets.only(bottom: 8),
      padding: const EdgeInsets.fromLTRB(13, 12, 13, 11),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerLow,
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        border: Border.all(
          color: (mutating ? scheme.secondary : scheme.outlineVariant)
              .withValues(alpha: 0.7),
        ),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Container(
                width: 24,
                height: 24,
                alignment: Alignment.center,
                decoration: BoxDecoration(
                  color: scheme.surface,
                  borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
                  border: Border.all(color: scheme.outlineVariant),
                ),
                child: Text(
                  '$index',
                  style: theme.textTheme.labelSmall?.copyWith(
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ),
              const SizedBox(width: 9),
              Expanded(
                child: Text(
                  _toolLabel(call.name),
                  style: theme.textTheme.labelLarge?.copyWith(
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ),
              Text(
                mutating ? '会改变状态' : '只读',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: mutating ? scheme.secondary : scheme.onSurfaceVariant,
                  fontWeight: FontWeight.w700,
                ),
              ),
            ],
          ),
          const SizedBox(height: 9),
          _TraceLine(label: '观察', value: _observationSummary(call)),
          _TraceLine(label: '策略', value: _boundarySummary(call)),
          _TraceLine(label: '动作', value: _actionSummary(call)),
          _TraceLine(label: '回执', value: _receiptSummary(call)),
          if (_needsVerification(call))
            _TraceLine(
              label: '验证',
              value: verification.summary,
              warning: verification.failed,
            ),
          if (call.failed)
            const _TraceLine(
              label: '中止',
              value: '工具返回失败；具体 abort 原因未随 UI 事件下发。',
              warning: true,
            ),
          if (!call.failed && turnEnded && call.pending)
            const _TraceLine(
              label: '中止',
              value:
                  '客户端的这一轮已结束，但没有收到工具结果；'
                  '服务端是否仍在运行及 abort 原因无法从此事件确定。',
              warning: true,
            ),
          const SizedBox(height: 5),
          Text(
            '输入文字、原始截图与凭据值不在此视图中；仅保留脱敏状态和摘要。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
              height: 1.4,
            ),
          ),
        ],
      ),
    );
  }
}

class _TraceLine extends StatelessWidget {
  const _TraceLine({
    required this.label,
    required this.value,
    this.warning = false,
  });

  final String label;
  final String value;
  final bool warning;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Padding(
      padding: const EdgeInsets.only(bottom: 5),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 42,
            child: Text(
              label,
              style: theme.textTheme.labelSmall?.copyWith(
                color: warning ? scheme.error : scheme.onSurfaceVariant,
                fontWeight: FontWeight.w700,
              ),
            ),
          ),
          Expanded(
            child: Text(
              value,
              style: theme.textTheme.bodySmall?.copyWith(
                color: warning ? scheme.error : scheme.onSurface,
                height: 1.45,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _EmptyTrace extends StatelessWidget {
  const _EmptyTrace();

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 24, horizontal: 4),
      child: Text(
        '这个会话还没有 Computer Use 工具事件。',
        style: Theme.of(context).textTheme.bodyMedium?.copyWith(
          color: Theme.of(context).colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

class _VerificationSummary {
  const _VerificationSummary({
    required this.passed,
    required this.failed,
    required this.summary,
  });

  final bool passed;
  final bool failed;
  final String summary;
}

_VerificationSummary _verification(ToolCall call) {
  final payload = _safePayload(call.output);
  final passed = payload?['verification_passed'];
  final waited = payload?['wait_satisfied'];
  if (passed is bool) {
    return _VerificationSummary(
      passed: passed,
      failed: !passed,
      summary: passed ? '通过；宿主动作后用新 observation 复核。' : '未通过；宿主动作后复核没有满足条件。',
    );
  }
  if (waited is bool) {
    return _VerificationSummary(
      passed: waited,
      failed: !waited,
      summary: waited ? '条件已在最新 observation 上成立。' : '等待结束但条件没有成立；不能视为完成。',
    );
  }
  if (call.output != null) {
    return const _VerificationSummary(
      passed: false,
      failed: false,
      summary: '工具返回了输出，但事件没有携带可解析的验证结论。',
    );
  }
  return const _VerificationSummary(
    passed: false,
    failed: false,
    summary: '验证明细未持久化；刷新或重启后只能看到调用摘要。',
  );
}

String _observationSummary(ToolCall call) {
  if (call.name == 'screenshot') {
    return call.pending ? '等待读取屏幕。' : '屏幕已读取；原始截图不在审计视图中展示。';
  }
  if (call.name == 'computer_list') {
    final payload = _safePayload(call.output);
    final windows = payload?['windows'];
    if (windows is List) return '允许窗口列表：${windows.length} 个。';
    if (call.output != null) return '允许窗口列表已返回；结构化原文已省略。';
    if (!call.pending) return '仅保留窗口列表调用摘要；观察明细未持久化。';
    return '等待读取允许窗口列表。';
  }

  final payload = _safePayload(call.output);
  if (payload == null) {
    if (!call.pending) {
      return '仅保留调用摘要；观察输出、截图和元素明细未持久化。';
    }
    return '等待观察返回。';
  }

  final parts = <String>[];
  final app = _safeLabel(payload['app']);
  if (app != null) parts.add('应用 $app');
  final total = _safeInt(payload['total_elements']);
  final returned = _safeInt(payload['returned_elements']);
  if (total != null && returned != null) {
    parts.add('返回 $returned/$total 个元素');
  } else if (returned != null) {
    parts.add('返回 $returned 个元素');
  }
  if (payload['elements_complete'] == true) parts.add('元素完整');
  if (payload['truncated'] == true) parts.add('元素已截断');
  if (payload['secure_input'] == true) parts.add('安全输入已检测到');
  if (payload['capture_available'] == true) parts.add('截图已捕获（不展示）');
  if (parts.isEmpty) parts.add('观察已返回；仅展示脱敏摘要');
  return '${parts.join(' · ')}。';
}

String _boundarySummary(ToolCall call) {
  if (_isReadonly(call)) {
    return '只读路径；限定在 allowlist 内。策略版本未随 UI 事件下发。';
  }
  if (call.name == 'computer_act' || call.name == 'computer_browser_act') {
    return '会改变目标状态，必须经过逐次确认；参数预览不会复制到轨迹中。';
  }
  return '会改变目标状态；具体策略版本和确认结果以本视图上方记录为准。';
}

String _actionSummary(ToolCall call) {
  final payload = _safePayload(call.output);
  final actionClass = _safeActionClass(payload?['action_class']);
  final suffix = actionClass == null ? '' : ' · $actionClass';
  return switch (call.name) {
    'computer_list' => '列出 allowlist 内窗口；不读取窗口内容。',
    'computer_observe' => '观察一个明确窗口；不发送输入。',
    'computer_wait' => '轮询并重新观察，不把超时当作完成。',
    'computer_act' => '执行一批窗口动作$suffix。',
    'computer_browser_open' => '创建一次性隔离浏览器会话。',
    'computer_browser_observe' => '读取允许站点的结构化快照。',
    'computer_browser_act' => '执行页面动作$suffix；输入值不展示。',
    'computer_browser_wait' => '轮询允许站点，不把超时当作完成。',
    'screenshot' => '读取屏幕；不保存或展示原始截图。',
    'click' => '点击屏幕；坐标和窗口内容不展示。',
    'type_text' => '输入文字；文字内容已从审计视图省略。',
    'key' => '发送按键；组合内容已从审计视图省略。',
    'scroll' => '滚动目标窗口；只保留动作类别。',
    _ => 'Computer Use 动作。',
  };
}

String _receiptSummary(ToolCall call) {
  if (call.pending) return '等待工具返回。';
  if (call.failed) return '工具返回失败；动作是否部分落地仍需结合状态核对。';
  if (call.output != null) {
    return '工具输出已返回；驱动 receipt_id 与耗时未随 UI 事件下发。';
  }
  return '摘要已保留；输出与驱动回执明细未持久化。';
}

bool _isReadonly(ToolCall call) =>
    call.name == 'computer_list' ||
    call.name == 'computer_observe' ||
    call.name == 'computer_wait' ||
    call.name == 'computer_browser_observe' ||
    call.name == 'computer_browser_wait' ||
    call.name == 'screenshot';

bool _needsVerification(ToolCall call) =>
    call.name == 'computer_act' ||
    call.name == 'computer_wait' ||
    call.name == 'computer_browser_act' ||
    call.name == 'computer_browser_wait' ||
    call.name == 'click' ||
    call.name == 'type_text' ||
    call.name == 'key' ||
    call.name == 'scroll';

Map<String, dynamic>? _safePayload(String? raw) {
  if (raw == null || raw.isEmpty) return null;
  try {
    final decoded = jsonDecode(raw);
    return decoded is Map<String, dynamic> ? decoded : null;
  } on FormatException {
    return null;
  }
}

int? _safeInt(Object? raw) {
  if (raw is int && raw >= 0) return raw;
  if (raw is num && raw >= 0) return raw.toInt();
  return null;
}

String? _safeLabel(Object? raw) {
  if (raw is! String) return null;
  var value = raw.replaceAll(RegExp(r'[\r\n\t]+'), ' ').trim();
  if (value.isEmpty) return null;
  if (value.length > 42) value = '${value.substring(0, 39)}...';
  return value;
}

String? _safeActionClass(Object? raw) {
  if (raw is! String) return null;
  return switch (raw) {
    'Observe' => '只读',
    'Navigate' => '导航',
    'Commit' => '提交',
    'Transmit' => '发送',
    _ => null,
  };
}

String _toolLabel(String name) => switch (name) {
  'computer_list' => '窗口列表',
  'computer_observe' => '窗口观察',
  'computer_wait' => '窗口等待',
  'computer_act' => '窗口动作',
  'computer_browser_open' => '打开隔离浏览器',
  'computer_browser_observe' => '隔离网页观察',
  'computer_browser_act' => '隔离网页动作',
  'computer_browser_wait' => '隔离网页等待',
  'screenshot' => '旧版截图',
  'click' => '旧版点击',
  'type_text' => '旧版文字输入',
  'key' => '旧版按键',
  'scroll' => '旧版滚动',
  _ => name,
};

String _riskLabel(String risk) => switch (risk) {
  'execute' => '执行风险',
  'write' => '写入风险',
  'safe' => '只读风险',
  _ => '风险级别未识别',
};

String _confirmOutcomeLabel(ConfirmOutcome outcome) => switch (outcome) {
  ConfirmOutcome.allowed => '用户已允许',
  ConfirmOutcome.denied => '用户已拒绝',
  ConfirmOutcome.superseded => '确认已失效或被其他设备处理',
  ConfirmOutcome.expired => '确认超时',
};
