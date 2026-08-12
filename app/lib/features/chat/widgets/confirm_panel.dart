import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/pending_confirmation.dart';
import '../../../state/chat_controller.dart';
import '../../../state/confirm_controller.dart';

/// The tool-confirmation prompt, pinned directly above the composer.
///
/// ## Why pinned rather than a modal dialog
///
/// A modal would be the reflex — it is blocking and urgent, and both are true
/// here. But the question being asked is "should this command run", and the
/// evidence for answering it is the conversation immediately behind the dialog:
/// what the user asked for, and what the model said it was about to do. A modal
/// barrier covers exactly that. Pinned above the composer the prompt is
/// impossible to miss, cannot be dismissed without answering, and leaves the
/// transcript scrollable while the clock runs.
///
/// ## Why every pending prompt is shown, not just the active session's
///
/// The queue is shared across devices and survives the connection that raised
/// it. Filtering to the session on screen would hide precisely the case the
/// recovery endpoint exists for — a request raised on another device, or
/// recovered after a reconnect into a session the user has since navigated away
/// from. Prompts are rare and self-expiring; showing all of them costs nothing
/// and losing one costs a suspended turn.
class ConfirmPanel extends ConsumerWidget {
  const ConfirmPanel({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final state = ref.watch(confirmControllerProvider);
    if (state.pending.isEmpty &&
        state.resolved.isEmpty &&
        state.error == null) {
      return const SizedBox.shrink();
    }

    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (state.error != null) _ReceiptError(message: state.error!),
        for (final resolution in state.resolved)
          _ResolvedNote(key: ValueKey(resolution.request.token), resolution: resolution),
        for (final request in state.pending)
          _ConfirmCard(
            key: ValueKey(request.token),
            request: request,
            busy: state.answering.contains(request.token),
          ),
      ],
    );
  }
}

class _ConfirmCard extends ConsumerWidget {
  const _ConfirmCard({super.key, required this.request, required this.busy});

  final PendingConfirmation request;
  final bool busy;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final controller = ref.read(confirmControllerProvider.notifier);
    final remaining = request.remainingFrom(DateTime.now());

    // Which session this belongs to, but only when it is *not* the one on
    // screen. Labelling the obvious case would be noise; leaving the
    // non-obvious one unlabelled would let a user approve a command belonging
    // to a conversation they are not looking at.
    final activeSessionId = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    final elsewhere =
        request.sessionId != null && request.sessionId != activeSessionId;
    final otherTitle = elsewhere
        ? ref.watch(
            // `select` down to the title string: the sessions list changes on
            // every `updated_at` bump, and this card must not rebuild for that.
            chatControllerProvider.select((s) {
              for (final session in s.sessions) {
                if (session.id == request.sessionId) return session.title;
              }
              return null;
            }),
          )
        : null;

    return Container(
      width: double.infinity,
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 8),
      decoration: BoxDecoration(
        color: scheme.errorContainer.withValues(alpha: 0.42),
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: scheme.error.withValues(alpha: 0.55)),
      ),
      padding: const EdgeInsets.fromLTRB(14, 11, 14, 11),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.gpp_maybe_outlined, size: 18, color: scheme.error),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  '需要你确认：${request.tool}',
                  style: theme.textTheme.titleSmall?.copyWith(
                    color: scheme.onErrorContainer,
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ),
              _RiskChip(risk: request.risk),
              const SizedBox(width: 8),
              _Countdown(remaining: remaining),
            ],
          ),
          if (elsewhere) ...[
            const SizedBox(height: 6),
            Text(
              '来自另一个会话：${otherTitle ?? request.sessionId}',
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.onErrorContainer.withValues(alpha: 0.85),
              ),
            ),
          ],
          const SizedBox(height: 9),
          _Preview(text: request.preview),
          const SizedBox(height: 6),
          Text(
            // States the default outcome, because the default is silent and
            // irreversible in one direction only. The daemon refuses on
            // timeout precisely so that walking away is safe; saying so turns
            // the countdown from a threat into a fact.
            '这一轮已经挂起，在等你的答复。${remaining.inSeconds} 秒后没有答复就按拒绝处理，'
            '模型会收到「没人回答」并换一条路。',
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onErrorContainer.withValues(alpha: 0.9),
            ),
          ),
          const SizedBox(height: 10),
          Row(
            mainAxisAlignment: MainAxisAlignment.end,
            children: [
              TextButton(
                onPressed: busy
                    ? null
                    : () => controller.answer(request.token, allow: false),
                child: const Text('拒绝'),
              ),
              const SizedBox(width: 8),
              FilledButton.tonal(
                onPressed: busy
                    ? null
                    : () => controller.answer(request.token, allow: true),
                style: FilledButton.styleFrom(
                  backgroundColor: scheme.error,
                  foregroundColor: scheme.onError,
                ),
                child: Text(busy ? '发送中…' : '允许执行'),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// The command itself.
///
/// Four properties, each of which the security value of this dialog depends on:
///
/// * **Monospace.** `rm -rf /tmp/x` and `rm -rf /tmp /x` differ by one space,
///   and a proportional font makes them look the same.
/// * **Never truncated.** `cortexd::confirm::preview_of` already capped this at
///   8 KiB and appended a visible marker when it did. A second trim here would
///   remove the tail — which is where `| sh` lives — and the user would approve
///   what they could not see. Long previews scroll instead.
/// * **Selectable.** A command worth being asked about is worth pasting
///   somewhere to read properly.
/// * **Whitespace preserved.** The daemon renders one `key: value` per line
///   rather than a JSON blob for the same reason: escaped `\n` and `\"` turn
///   one command into the appearance of another.
class _Preview extends StatelessWidget {
  const _Preview({required this.text});

  final String text;

  /// Past this the block scrolls rather than pushing the buttons off screen.
  /// Chosen so an ordinary shell invocation (one or two lines) never scrolls
  /// at all — scrolling is itself a signal that there is more to read.
  static const double _maxHeight = 208;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Container(
      width: double.infinity,
      decoration: BoxDecoration(
        color: scheme.surface.withValues(alpha: 0.72),
        borderRadius: BorderRadius.circular(7),
        border: Border.all(color: scheme.outlineVariant),
      ),
      padding: const EdgeInsets.fromLTRB(11, 9, 11, 9),
      constraints: const BoxConstraints(maxHeight: _maxHeight),
      child: Scrollbar(
        child: SingleChildScrollView(
          primary: false,
          child: SelectableText(
            text.isEmpty ? '（服务端没有给出参数预览）' : text,
            style: theme.textTheme.bodySmall?.copyWith(
              fontFamily: 'monospace',
              height: 1.45,
              color: scheme.onSurface,
            ),
          ),
        ),
      ),
    );
  }
}

class _RiskChip extends StatelessWidget {
  const _RiskChip({required this.risk});

  final String risk;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    // Unknown levels are shown verbatim rather than mapped to a default: a
    // daemon that grows a new risk level must not have it silently rendered as
    // the mildest one.
    final label = switch (risk) {
      'execute' => '执行',
      'write' => '写入',
      // 'safe' 会出现在**越界**确认上：一个 read_file 按风险根本不用问，
      // 但它读的是工作区外的文件。少了这一条，那种确认框上会显示一个
      // 生硬的 "safe"
      'safe' => '读取',
      final other => other,
    };
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
      decoration: BoxDecoration(
        color: scheme.error.withValues(alpha: 0.16),
        borderRadius: BorderRadius.circular(4),
      ),
      child: Text(
        label,
        style: theme.textTheme.labelSmall?.copyWith(
          color: scheme.error,
          fontWeight: FontWeight.w700,
        ),
      ),
    );
  }
}

/// Seconds left, monospaced so the digits do not jitter as they change width.
class _Countdown extends StatelessWidget {
  const _Countdown({required this.remaining});

  final Duration remaining;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final seconds = remaining.inSeconds;
    final minutes = seconds ~/ 60;
    final text = minutes > 0
        ? '$minutes:${(seconds % 60).toString().padLeft(2, '0')}'
        : '${seconds}s';

    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(
          Icons.timer_outlined,
          size: 14,
          color: scheme.onErrorContainer.withValues(alpha: 0.9),
        ),
        const SizedBox(width: 4),
        Text(
          text,
          style: theme.textTheme.labelMedium?.copyWith(
            fontFamily: 'monospace',
            fontWeight: FontWeight.w700,
            color: scheme.onErrorContainer,
          ),
        ),
      ],
    );
  }
}

/// Why a prompt that was on screen a moment ago is gone.
///
/// Styled as a neutral note, never as an error. Three of the four outcomes are
/// completely ordinary, and the fourth ("你拒绝了") is the user's own choice.
class _ResolvedNote extends ConsumerWidget {
  const _ResolvedNote({super.key, required this.resolution});

  final ConfirmResolution resolution;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    final (IconData icon, String text) = switch (resolution.outcome) {
      ConfirmOutcome.allowed => (
        Icons.play_arrow_rounded,
        '已允许 ${resolution.request.tool} 执行。',
      ),
      ConfirmOutcome.denied => (
        Icons.block_rounded,
        '已拒绝 ${resolution.request.tool}，拒绝理由会回传给模型。',
      ),
      ConfirmOutcome.superseded => (
        Icons.devices_rounded,
        '这条确认已经不在等待了 —— 可能是别的设备先答了、超时了，或者那一轮已经结束。'
            'cortexd 刻意不区分这几种情况，所以这里也说不出是哪一种。你的这次点击没有生效。',
      ),
      ConfirmOutcome.expired => (
        Icons.timer_off_outlined,
        '${resolution.request.tool} 的确认已超时，按拒绝处理。重新发一句就能再来一次。',
      ),
    };

    return Container(
      width: double.infinity,
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 8),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest.withValues(alpha: 0.6),
        borderRadius: BorderRadius.circular(8),
      ),
      padding: const EdgeInsets.fromLTRB(12, 8, 6, 8),
      child: Row(
        children: [
          Icon(icon, size: 15, color: scheme.onSurfaceVariant),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              text,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
            ),
          ),
          IconButton(
            onPressed: () => ref
                .read(confirmControllerProvider.notifier)
                .clearResolved(resolution.request.token),
            iconSize: 15,
            visualDensity: VisualDensity.compact,
            tooltip: '知道了',
            icon: const Icon(Icons.close_rounded),
          ),
        ],
      ),
    );
  }
}

/// A receipt that genuinely failed to reach the daemon — network down, 500.
///
/// Distinct from [ConfirmOutcome.superseded] because the remedy differs: this
/// one is worth retrying, and the prompt is still on screen to retry with.
class _ReceiptError extends ConsumerWidget {
  const _ReceiptError({required this.message});

  final String message;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 6),
      padding: const EdgeInsets.fromLTRB(12, 8, 6, 8),
      decoration: BoxDecoration(
        color: scheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: [
          Icon(Icons.error_outline_rounded, size: 15, color: scheme.error),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              '$message 倒计时还在走，可以再试一次。',
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onErrorContainer,
              ),
            ),
          ),
          IconButton(
            onPressed: ref.read(confirmControllerProvider.notifier).clearError,
            iconSize: 15,
            visualDensity: VisualDensity.compact,
            icon: const Icon(Icons.close_rounded),
          ),
        ],
      ),
    );
  }
}
