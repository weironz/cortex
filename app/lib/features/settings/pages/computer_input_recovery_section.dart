import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/computer_input_recovery_api.dart';
import '../../../core/theme.dart';
import '../../../models/health_status.dart';
import '../../../state/app_providers.dart';
import '../../../state/auth_controller.dart';
import '../../../state/chat_controller.dart';
import '../widgets/settings_layout.dart';

/// 用户控制面：不经过工具确认簿，也不读取“完全放行”来省略任何人工确认。
class ComputerInputRecoverySection extends ConsumerWidget {
  const ComputerInputRecoverySection({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final health = ref.watch(healthProvider).value;
    if (health?.isLocalAgent != true ||
        health?.computerCapabilities?.writeReadiness !=
            ComputerWriteReadiness.authorityUnresolved) {
      return const SizedBox.shrink();
    }
    final api = ref.watch(cortexApiProvider);
    if (api is! ComputerInputRecoveryApi) {
      return const _RecoveryCard(
        child: Text(
          '当前连接不支持输入恢复，输入保持暂停。',
          key: ValueKey('computer_input_recovery:unavailable'),
        ),
      );
    }
    final recoveryApi = api as ComputerInputRecoveryApi;
    final session = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    if (session == null || session.isEmpty) {
      return const _RecoveryCard(
        child: Text(
          '请先打开对应账号下已有的会话，再返回此处核对。不会自动创建会话。',
          key: ValueKey('computer_input_recovery:no-session'),
        ),
      );
    }
    final account = ref.watch(authControllerProvider);
    return _RecoveryPanel(
      key: _RecoveryScopeKey(api, account, session),
      api: recoveryApi,
      sessionId: session,
      isCurrent: () =>
          context.mounted &&
          identical(ref.read(cortexApiProvider), api) &&
          identical(ref.read(authControllerProvider), account) &&
          ref.read(chatControllerProvider).activeSessionId == session &&
          ref.read(healthProvider).value?.isLocalAgent == true &&
          ref
                  .read(healthProvider)
                  .value
                  ?.computerCapabilities
                  ?.writeReadiness ==
              ComputerWriteReadiness.authorityUnresolved,
    );
  }
}

/// key 只按控制上下文比较身份；诊断输出不包含账号凭据或恢复凭据。
class _RecoveryScopeKey extends LocalKey {
  const _RecoveryScopeKey(this.api, this.account, this.sessionId);
  final Object api;
  final Object account;
  final String sessionId;

  @override
  bool operator ==(Object other) =>
      other is _RecoveryScopeKey &&
      identical(api, other.api) &&
      identical(account, other.account) &&
      sessionId == other.sessionId;
  @override
  int get hashCode =>
      Object.hash(identityHashCode(api), identityHashCode(account), sessionId);
  @override
  String toString() => '[ComputerInputRecoveryScope]';
}

class _RecoveryCard extends StatelessWidget {
  const _RecoveryCard({required this.child, this.complete = false});
  final Widget child;
  final bool complete;

  @override
  Widget build(BuildContext context) => SettingsCard(
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Icon(
              complete
                  ? Icons.check_circle_outline_rounded
                  : Icons.pause_circle_outline_rounded,
              size: 20,
              color: complete
                  ? Theme.of(context).cortex.success
                  : Theme.of(context).cortex.warning,
            ),
            const SizedBox(width: 8),
            Text('输入恢复', style: Theme.of(context).textTheme.titleSmall),
          ],
        ),
        const SizedBox(height: 10),
        child,
      ],
    ),
  );
}

class _RecoveryPanel extends StatefulWidget {
  const _RecoveryPanel({
    super.key,
    required this.api,
    required this.sessionId,
    required this.isCurrent,
  });
  final ComputerInputRecoveryApi api;
  final String sessionId;
  final bool Function() isCurrent;

  @override
  State<_RecoveryPanel> createState() => _RecoveryPanelState();
}

enum _RecoveryStep { inspect, retire, acknowledge }

class _RecoveryPanelState extends State<_RecoveryPanel> {
  static const _limit = Duration(seconds: 30);
  static const _pollInterval = Duration(milliseconds: 500);
  ComputerInputRecoveryState? _status;
  ComputerInputRecoveryState? _details;
  String? _error;
  bool _busy = true;
  bool _confirming = false;
  int _serial = 0;
  Timer? _poll;
  Timer? _deadline;
  Completer<void>? _abort;
  DialogRoute<bool>? _dialog;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted && widget.isCurrent()) _load();
    });
  }

  bool _active(int serial) =>
      mounted && serial == _serial && widget.isCurrent();

  void _cancel() {
    _serial++;
    _poll?.cancel();
    _deadline?.cancel();
    _poll = null;
    _deadline = null;
    final abort = _abort;
    _abort = null;
    if (abort != null && !abort.isCompleted) abort.complete();
  }

  void _finish() {
    _poll?.cancel();
    _deadline?.cancel();
    _poll = null;
    _deadline = null;
    final abort = _abort;
    _abort = null;
    if (abort != null && !abort.isCompleted) abort.complete();
  }

  void _load() => _begin(
    (abort) => widget.api.inputRecovery(widget.sessionId, abortTrigger: abort),
  );

  void _begin(
    Future<ComputerInputRecoveryState> Function(Future<void>) request, {
    ComputerInputRecoveryState? expected,
    _RecoveryStep step = _RecoveryStep.inspect,
  }) {
    if (!mounted || !widget.isCurrent()) return;
    _cancel();
    final serial = _serial;
    final abort = Completer<void>();
    _abort = abort;
    setState(() {
      _status = null;
      _details = expected;
      _error = null;
      _busy = true;
    });
    _deadline = Timer(_limit, () {
      if (!_active(serial)) return;
      _cancel();
      setState(() {
        _busy = false;
        _status = null;
        _error = '等待已超过30秒，无法确认恢复结果。输入仍保持暂停，请刷新状态核对。';
      });
    });
    unawaited(_receive(() => request(abort.future), serial, expected, step));
  }

  Future<void> _receive(
    Future<ComputerInputRecoveryState> Function() request,
    int serial,
    ComputerInputRecoveryState? expected,
    _RecoveryStep step,
  ) async {
    try {
      final state = await request();
      if (!_active(serial)) return;
      if (step == _RecoveryStep.retire &&
          state.phase == ComputerInputRecoveryPhase.complete) {
        throw const CortexApiException('停止步骤不能代替外部结果核对，请刷新状态后重新确认。');
      }
      if (expected != null &&
          ((state.recoveryId != null &&
                  expected.recoveryId != null &&
                  state.recoveryId != expected.recoveryId) ||
              (state.operationId != null &&
                  expected.operationId != null &&
                  state.operationId != expected.operationId) ||
              (state.epoch != null &&
                  expected.epoch != null &&
                  state.epoch != expected.epoch))) {
        throw const CortexApiException('原操作或输入代际已变化，请刷新后重新核对。');
      }
      final bound = ComputerInputRecoveryState(
        phase: state.phase,
        recoveryId: state.recoveryId ?? expected?.recoveryId,
        operationId: state.operationId ?? expected?.operationId,
        epoch: state.epoch ?? expected?.epoch,
        message: state.message,
      );
      setState(() {
        _status = bound;
        _details = bound;
        _busy = state.phase == ComputerInputRecoveryPhase.working;
      });
      if (_busy) {
        // 只轮询严格 GET，不自动重发 retire/acknowledge，更不重试旧输入动作。
        _poll = Timer(_pollInterval, () {
          if (!_active(serial)) return;
          final abort = _abort;
          if (abort == null) return;
          unawaited(
            _receive(
              () => widget.api.inputRecovery(
                widget.sessionId,
                abortTrigger: abort.future,
              ),
              serial,
              bound,
              step,
            ),
          );
        });
      } else {
        _finish();
      }
    } on Object catch (error) {
      if (!_active(serial)) return;
      _finish();
      setState(() {
        _busy = false;
        _status = null;
        if (error is CortexApiException && error.statusCode == 404) {
          _status = const ComputerInputRecoveryState();
          _error = null;
        } else {
          _error = error is CortexApiException
              ? '${error.message} 输入保持暂停。'
              : '无法确认输入恢复状态，输入保持暂停。';
        }
      });
    }
  }

  Future<bool> _confirm({
    required bool review,
    required ComputerInputRecoveryState state,
  }) async {
    if (_confirming || _busy || !widget.isCurrent()) return false;
    setState(() => _confirming = true);
    final route = DialogRoute<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(review ? '已核对外部结果？' : '停止原执行器？'),
        content: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                review
                    ? '请先在外部应用中核对这次操作是否已经提交、发送或产生其他效果。'
                          '恢复只解除后续输入暂停，不会重试原来的动作。尚未核对请取消。'
                    : '将停止这次未确认输入所用的原执行器。停止不会撤销已经发生的效果。'
                          '停止完成后，仍需你检查外部应用的实际结果。',
              ),
              const SizedBox(height: 12),
              Text('原操作：${state.operationId ?? '后端未提供'}'),
              Text('输入代际：${state.epoch ?? '后端未提供'}'),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            key: ValueKey(
              review
                  ? 'computer_input_recovery:confirm-review'
                  : 'computer_input_recovery:confirm-stop',
            ),
            onPressed: () => Navigator.of(context).pop(true),
            child: Text(review ? '已核对，恢复后续输入' : '停止原执行器'),
          ),
        ],
      ),
    );
    _dialog = route;
    final accepted = await Navigator.of(context).push(route) ?? false;
    if (!mounted) return false;
    _dialog = null;
    setState(() => _confirming = false);
    return accepted && widget.isCurrent();
  }

  Future<void> _retire() async {
    final state = _status;
    final id = state?.recoveryId;
    if (state == null ||
        id == null ||
        id.isEmpty ||
        state.phase != ComputerInputRecoveryPhase.needsStop) {
      return;
    }
    if (!await _confirm(review: false, state: state) || !mounted) return;
    _begin(
      (abort) => widget.api.retireInputRecovery(
        widget.sessionId,
        recoveryId: id,
        abortTrigger: abort,
      ),
      expected: state,
      step: _RecoveryStep.retire,
    );
  }

  Future<void> _acknowledge() async {
    final state = _status;
    final id = state?.recoveryId;
    if (state == null ||
        id == null ||
        id.isEmpty ||
        state.phase != ComputerInputRecoveryPhase.needsReview) {
      return;
    }
    if (!await _confirm(review: true, state: state) || !mounted) return;
    _begin(
      (abort) => widget.api.acknowledgeInputRecovery(
        widget.sessionId,
        recoveryId: id,
        abortTrigger: abort,
      ),
      expected: state,
      step: _RecoveryStep.acknowledge,
    );
  }

  @override
  void dispose() {
    _cancel();
    final dialog = _dialog;
    // 设置页/账号/会话切换时关闭旧确认框，不能让新上下文消费旧人工同意。
    if (dialog != null) {
      scheduleMicrotask(() {
        if (dialog.isActive) dialog.navigator?.removeRoute(dialog, false);
      });
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final phase = _status?.phase;
    final theme = Theme.of(context);
    final fallback = switch (phase) {
      ComputerInputRecoveryPhase.needsStop => '输入结果尚未确认。请先停止原执行器，输入继续保持暂停。',
      ComputerInputRecoveryPhase.working => '正在处理恢复请求，请等待状态核对。',
      ComputerInputRecoveryPhase.needsReview =>
        '原执行器停止步骤已完成。请核对外部结果，再单独确认恢复后续输入。',
      ComputerInputRecoveryPhase.complete => '后续输入已恢复；原动作不会自动重试。',
      ComputerInputRecoveryPhase.failed => '恢复未完成，输入保持暂停。',
      _ => _busy ? '正在核对当前会话的输入恢复状态…' : '当前会话的输入恢复不可用，输入保持暂停。',
    };
    final message = _error ?? fallback;
    final note = _status?.message;
    final details = _details;
    final enabled =
        !_busy &&
        !_confirming &&
        _error == null &&
        _status?.recoveryId?.isNotEmpty == true;
    return _RecoveryCard(
      complete: phase == ComputerInputRecoveryPhase.complete && _error == null,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            message,
            key: const ValueKey('computer_input_recovery:status'),
            style: theme.textTheme.bodySmall?.copyWith(
              color:
                  _error != null || phase == ComputerInputRecoveryPhase.failed
                  ? theme.colorScheme.error
                  : null,
            ),
          ),
          if (_error == null && note != null && note != fallback)
            Text(note, key: const ValueKey('computer_input_recovery:message')),
          if (details != null) ...[
            const SizedBox(height: 8),
            Text(
              '原操作：${details.operationId ?? '后端未提供'}',
              style: theme.textTheme.labelSmall,
            ),
            Text(
              '输入代际：${details.epoch ?? '后端未提供'}',
              style: theme.textTheme.labelSmall,
            ),
          ],
          if ((phase == ComputerInputRecoveryPhase.needsStop ||
                  phase == ComputerInputRecoveryPhase.needsReview) &&
              _status?.recoveryId == null)
            const Text('后端未提供恢复凭据，无法继续；请刷新状态核对。'),
          const SizedBox(height: 12),
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              if (phase == ComputerInputRecoveryPhase.needsStop)
                FilledButton.icon(
                  key: const ValueKey('computer_input_recovery:retire'),
                  onPressed: enabled ? _retire : null,
                  icon: const Icon(Icons.stop_circle_outlined, size: 18),
                  label: const Text('停止原执行器…'),
                ),
              if (phase == ComputerInputRecoveryPhase.needsReview)
                FilledButton.icon(
                  key: const ValueKey('computer_input_recovery:review'),
                  onPressed: enabled ? _acknowledge : null,
                  icon: const Icon(Icons.fact_check_outlined, size: 18),
                  label: const Text('核对结果并恢复…'),
                ),
              OutlinedButton.icon(
                key: const ValueKey('computer_input_recovery:refresh'),
                onPressed: _busy || _confirming ? null : _load,
                icon: const Icon(Icons.refresh_rounded, size: 18),
                label: const Text('刷新状态'),
              ),
            ],
          ),
          const SizedBox(height: 8),
          Text(
            '取消确认不会执行恢复或重试原动作；未完成核对时继续保持输入暂停。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.cortex.foregroundTertiary,
            ),
          ),
        ],
      ),
    );
  }
}
