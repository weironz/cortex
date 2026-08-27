/// 设置 → 我的机器。
///
/// # 为什么这一页值得存在
///
/// 服务端的在线名册（`GET /agents`）2026-08-17 就做完了，而在这一页之前
/// **客户端一行都没接** —— 于是那句「你的会话绑在某台机器上」的 409 里
/// 曾经写着「`GET /agents` 能看到当前在线的机器」：**在错误提示里教用户
/// 发 HTTP 请求**。它写得出来正是因为界面上没有那个东西。
///
/// 这一页存在之后那句话才有地方指。
///
/// # 为什么在设置页，不在左栏或顶栏
///
/// 两个更「顺手」的落点都被否掉了，理由都是实测出来的：
///
/// * **左栏会话行**：2026-08-24 刚删掉一个同类的绿色文件夹角标，用户原话
///   「绿色图标又是什么」。「这个会话钉在哪台机器」与「绑没绑目录」是同一
///   事实的两面 —— 加回去就是同一个坑换个图案。
/// * **顶栏那一排**：`chat_pane.dart` 有一段注释明确把「跑在哪」赶出了顶栏，
///   说那一排是**应用级显示开关**，混进去会让人以为工作区也是「看不看」
///   而不是「跑在哪」。
///
/// 于是它落在设置页的「系统」组，与「连接」并列 —— 那一组回答的正是
/// 「这套东西现在连着什么」。
///
/// # 这一页只读
///
/// 开不开放远程接入是**那台机器上**的决定（`--allow-remote-attach`），
/// 不是这里点一下就能改的 —— 让云端够到一个能跑 shell 的进程，必须是
/// 机器主人在那台机器上的一次显式选择。所以这里只答「有哪些、够不够得着」。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/theme.dart';
import '../../../models/agent_presence.dart';
import '../../../state/app_providers.dart';
import '../../../widgets/empty_state.dart';
import '../../../widgets/panel_header.dart';

/// 名册多久重拉一次。
///
/// # 为什么是轮询，而不是等推送
///
/// presence **刻意不进库、也不进 `sync_log`**（心跳每 30 秒一行写入会挤满
/// 同步流水并下发给所有设备）。而这个仓库唯一的实时通道就是
/// 「`sync_log` 有新行 → `/ws` 推一个信号 → 客户端按游标补拉」——
/// 不落库的东西走不了那条路。所以这一页只能自己拉。
///
/// 10 秒：服务端心跳 30 秒一次、TTL 90 秒，所以状态本来就有最多 30 秒的
/// 滞后；比它更密没有信息量，只是白打请求。
const _refreshEvery = Duration(seconds: 10);

/// 我名下此刻在线的机器。
///
/// `autoDispose`：只有打开这一页时才轮询 —— 一个常驻的 10 秒轮询会在
/// 用户根本没在看的时候一直打服务端。
final machinesProvider = StreamProvider.autoDispose<List<AgentPresence>>((
  ref,
) async* {
  final api = ref.watch(cortexApiProvider);
  while (true) {
    yield await api.agents();
    await Future<void>.delayed(_refreshEvery);
  }
});

class MachinesPage extends ConsumerWidget {
  const MachinesPage({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final machines = ref.watch(machinesProvider);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const PanelHeader(title: '我的机器', subtitle: '在线的 agent · 能不能远程接入'),
        Expanded(
          child: machines.when(
            // 轮询会不断产出新值，不 skip 的话每 10 秒闪一次转圈
            skipLoadingOnReload: true,
            skipLoadingOnRefresh: true,
            loading: () => const Center(child: CircularProgressIndicator()),
            error: (e, _) => Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(
                  e is CortexApiException && e.isUnsupported
                      // 老服务端没有这条路。说清是「这个部署没有」而不是
                      // 「出错了」—— 后者会让人去重试、去重启
                      ? '这个部署的服务端还没有在线名册（升级服务端之后就有了）。'
                      : '读不出在线的机器：$e',
                  textAlign: TextAlign.center,
                  style: theme.textTheme.bodyMedium?.copyWith(
                    color: theme.colorScheme.onSurfaceVariant,
                  ),
                ),
              ),
            ),
            data: (list) => list.isEmpty
                ? const _Nobody()
                : ListView(
                    padding: const EdgeInsets.fromLTRB(20, 8, 20, 24),
                    children: [
                      const _Intro(),
                      const SizedBox(height: 12),
                      for (final m in list) _MachineRow(machine: m),
                    ],
                  ),
          ),
        ),
      ],
    );
  }
}

/// 一台都没有。
///
/// ⚠️ **这不是「出错了」，也不一定是「你没装」** —— 最常见的情况是那几台
/// 机器此刻关着。所以话要往这个方向说，而不是让人去查网络或重装。
class _Nobody extends StatelessWidget {
  const _Nobody();

  @override
  Widget build(BuildContext context) => const EmptyState(
    icon: Icons.devices_other_outlined,
    title: '现在没有在线的机器',
    description:
        '在一台机器上打开 Cortex（或跑起 cortex-local），它会在半分钟内出现在这里。\n'
        '关着的机器不会列出来 —— 这一页只答「现在」。',
  );
}

class _Intro extends StatelessWidget {
  const _Intro();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Text(
      '绑了本机目录的会话只能在那台机器上跑 —— 它的文件只在那儿。\n'
      '开了远程接入的机器，可以从这里（或手机、网页）接过去继续聊。',
      style: theme.textTheme.bodySmall?.copyWith(
        color: theme.cortex.foregroundTertiary,
        height: 1.6,
      ),
    );
  }
}

class _MachineRow extends StatelessWidget {
  const _MachineRow({required this.machine});

  final AgentPresence machine;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final tokens = theme.cortex;

    return Container(
      margin: const EdgeInsets.only(bottom: 8),
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 12),
      decoration: BoxDecoration(
        color: scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(
            Icons.computer_outlined,
            size: 18,
            color: tokens.foregroundTertiary,
          ),
          const SizedBox(width: 11),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(machine.machineHint, style: theme.textTheme.titleSmall),
                const SizedBox(height: 3),
                Text(
                  _subtitle(machine),
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: tokens.foregroundTertiary,
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(width: 10),
          _AttachBadge(attachable: machine.attachable),
        ],
      ),
    );
  }

  /// 「在线」这件事不写出来 —— 列表里的**每一台**都在线（离线的服务端
  /// 根本不回），写了等于每行挂一枚恒真的章，而恒真的章会训练人忽略
  /// 那个位置（`_BackendBadge` 那条论证的同一个形状）。
  ///
  /// 写的是**有信息量**的两件：多久前报到过、手上有几个会话。
  static String _subtitle(AgentPresence m) {
    final seen = m.lastSeenSecs < 60 ? '刚刚报到' : '${m.lastSeenSecs ~/ 60} 分钟前报到';
    return m.sessionCount == 0
        ? '$seen · 还没有绑定本机目录的会话'
        : '$seen · ${m.sessionCount} 个会话绑在它上面';
  }
}

/// 够不够得着。
///
/// # 为什么「够得着」有色而「够不着」没有
///
/// 色彩只表达动作或含义。「这台机器能被远程接过去」是一个**用户可以据此
/// 行动**的事实；而「没开」是默认状态 —— 给它一个红章会让一整列默认状态
/// 的机器看起来像一列故障，而它们什么问题都没有。
class _AttachBadge extends StatelessWidget {
  const _AttachBadge({required this.attachable});

  final bool attachable;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final tokens = theme.cortex;

    if (!attachable) {
      return Tooltip(
        // 说清**怎么开**，而不只是「没开」—— 否则用户唯一能做的是挨个试
        message:
            '没有开放远程接入。要从别处接过去，\n'
            '在那台机器上用 --allow-remote-attach 起 agent。',
        child: Text(
          '仅本机',
          style: theme.textTheme.labelSmall?.copyWith(
            color: tokens.foregroundTertiary,
          ),
        ),
      );
    }
    return Tooltip(
      message: '可以从别的设备接过去继续聊',
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
        decoration: BoxDecoration(
          color: tokens.success.withValues(alpha: 0.12),
          borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        ),
        child: Text(
          '可接入',
          style: theme.textTheme.labelSmall?.copyWith(
            color: tokens.success,
            fontWeight: FontWeight.w600,
          ),
        ),
      ),
    );
  }
}
