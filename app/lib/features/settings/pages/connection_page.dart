import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../auth/token_store.dart';
import '../../../core/app_config.dart';
import '../../../core/local_agent.dart';
import '../../../models/health_status.dart';
import '../../../models/sandbox_health.dart';
import '../../../state/app_providers.dart';
import '../../../state/auth_controller.dart';

/// 连接这一页：连的是谁、它现在什么样。
class ConnectionPage extends ConsumerStatefulWidget {
  const ConnectionPage({super.key});

  @override
  ConsumerState<ConnectionPage> createState() => _ConnectionPageState();
}

class _ConnectionPageState extends ConsumerState<ConnectionPage> {
  late final TextEditingController _urlController = TextEditingController(
    text: ref.read(appConfigProvider).baseUrl,
  );

  @override
  void dispose() {
    _urlController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final config = ref.watch(appConfigProvider);
    final notifier = ref.read(appConfigProvider.notifier);
    final health = ref.watch(healthProvider);
    final auth = ref.watch(authControllerProvider);

    return ListView(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      children: [
        // ── 这里曾经有一个「使用 Mock 数据源」开关，删了 ──────────
        //
        // 它是**开发用的实现细节，被摆到了产品设置里**。问一句「用户关掉它
        // 能得到什么好处」就露馅了：Mock 是内存夹具，不连任何后端，
        // 打开它等于把一个能用的客户端换成一个演示。没有哪个真实用户
        // 会想要那个，而看到它的人只会困惑「我现在的数据是真的吗」。
        //
        // 与「云沙箱」那个开关同一个形状：把只有我们自己关心的实现细节
        // 推给用户去决定。那次的判据是「省 10 MiB，代价是一个他不理解的
        // 开关」，这次更干脆 —— 它对用户**没有任何好处**。
        //
        // 夹具本身留着（`MockCortexApi`，测试与离线演示要用），入口退回成
        // 构建参数：`flutter run --dart-define=USE_MOCK=true`。
        // 一个开发用的东西该长成开发用的样子，见 app/README.md。
        // ── 这里要的是**部署入口**，不是记忆服务本身 ──
        //
        // 原本写的是「cortexd 地址」，提示词给的是 `http://127.0.0.1:8080`
        // —— 那正是记忆服务自己。填了它，会话、历史、实时同步全都正常，
        // **只有云端对话不通**：拆成 cortexd + agentd 之后 `/chat` 归 agentd，
        // 而知道该转给谁的只有边缘（dev 的 nginx / prod 的 traefik）。
        //
        // 2026-08-15 真机上把人卡了半天：症状是「桌面端发不出消息」，
        // 而所有健康检查都是绿的 —— `/health` 两个地址都答 `role: cortexd`，
        // 光看它分不出来。**一个默认值把用户领到了唯一走不通的那条路上。**
        TextField(
          controller: _urlController,
          enabled: !config.useMock,
          decoration: const InputDecoration(
            labelText: '部署入口地址',
            hintText: 'https://<域名>/api',
            helperText:
                '填部署入口，不是记忆服务本身 —— 云端对话由它转给 agent 编排服务。'
                '本机开发是 http://127.0.0.1:5173。',
            helperMaxLines: 3,
          ),
          onSubmitted: notifier.setBaseUrl,
        ),
        const SizedBox(height: 6),
        Align(
          alignment: Alignment.centerRight,
          child: TextButton(
            onPressed: config.useMock
                ? null
                : () => notifier.setBaseUrl(_urlController.text),
            child: const Text('应用地址'),
          ),
        ),
        const Divider(height: 24),
        Row(
          children: [
            Expanded(child: Text('后端状态', style: theme.textTheme.titleSmall)),
            TextButton(
              // 两条都要重来。只刷 `/health` 的话，用户去把沙箱那一套起好、
              // 回来点「重新检测」，看到的还是上一次的能力结论 ——
              // 而他刚做的事恰恰是为了改变那个结论
              onPressed: () {
                ref.invalidate(healthProvider);
                ref.invalidate(sandboxHealthProvider);
              },
              child: const Text('重新检测'),
            ),
          ],
        ),
        const SizedBox(height: 8),
        health.when(
          loading: () => const Padding(
            padding: EdgeInsets.symmetric(vertical: 6),
            child: SizedBox(
              width: 16,
              height: 16,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
          ),
          error: (e, _) => Text(
            '$e',
            style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
          ),
          data: (h) => Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              _kv(context, 'status', h.status),
              // 版本号与 sha 并排。**只有版本号是不够的** —— semver 打完
              // tag 的下一秒就不再唯一，之后每个提交都还报同一个版本号。
              // 判「线上有没有那个修复」靠的是 sha，见 cortex_core::BUILD_SHA。
              // 老服务端不报，那时只显示版本号（不写「未知」：那会让人以为
              // 对面那台有问题，而它只是旧一点）
              _kv(
                context,
                'version',
                h.commit == null ? h.version : '${h.version}  ·  ${h.commit}',
              ),
              _kv(context, 'database', h.database),
              _kv(context, 'auth', h.auth),
              // 说出来，而不是默默庆幸。cortexd 只有在有人**特意**写了
              // `CORTEX_AUTH=disabled` 时才会到这个状态，而且它每次启动都会
              // 警告；客户端要是不提，它就是整套系统里唯一一个没有指出
              // 「记忆库对任何够得着这个端口的人开放」的地方
              if (h.authDisabled && !config.useMock) ...[
                const SizedBox(height: 6),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Icon(
                      Icons.lock_open_rounded,
                      size: 14,
                      color: scheme.error,
                    ),
                    const SizedBox(width: 6),
                    Expanded(
                      child: Text(
                        // 说「拥有全部记忆」是过期的：长期记忆 2026-08-17
                        // 拆去了 Cormex，这一侧没有。而真正暴露的东西
                        // 一点不轻 —— 会话、历史、以及 agent 够得着的文件
                        '这个部署关闭了认证：任何能连上 '
                        '${config.baseUrl} 的人都拥有你的全部会话与历史，'
                        '也能让 agent 读写它够得着的文件。'
                        '只有监听地址确实是回环时才可接受。',
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: scheme.error,
                        ),
                      ),
                    ),
                  ],
                ),
              ],
              // ── 「连得上」与「能跑云端对话」是两件事 ──────────────
              //
              // 上面那几行全部来自 `/health`，而 `/health` 只证明这个地址上
              // 有个进程在答话。云端对话不是它答的：一轮云端对话要 agent
              // 编排服务向记忆服务换委托凭据、据它拉起沙箱容器、容器再回连
              // 编排服务 —— 这三段断哪一段，`/health` 都照样 200。于是这一页
              // 画绿灯「已连接」，用户回到对话框发一句，得到的是失败。
              //
              // 生产上更狠：边缘把 `/health` 分给了**记忆服务**，
              // `/sandbox/health` 才是编排服务 —— 那条路上的 `/health`
              // 连「编排服务在不在」都没有回答。
              const SizedBox(height: 10),
              _cloudChat(context, ref),
            ],
          ),
        ),
        _localAgent(context, ref),
        // 「退出登录」搬去了左下角账号菜单。凭据存在哪这句话留着 ——
        // 它回答的是「我关掉这个窗口，密码会留在哪」，属于知情，不是一个操作
        if (!config.useMock && auth.token != null) ...[
          const Divider(height: 24),
          Text(
            '已用 token 连接。凭据只存在于内存'
            '${kCanRememberToken ? '（以及你勾选的 sessionStorage）' : '与环境变量 $kTokenEnvVar'}。',
            style: theme.textTheme.bodySmall,
          ),
        ],
      ],
    );
  }

  /// 本机 agent 那一节。
  ///
  /// # 为什么必须有它
  ///
  /// 桌面端**不是一个进程，是两个**：这个界面，与它拉起的 `cortex-local`
  /// （工具、文件读写、命令执行全在后者）。后者 `--bind 127.0.0.1:0`，
  /// 端口每次由内核随机分。
  ///
  /// 而在这一节之前，**产品里没有任何一个地方承认它存在**。2026-08-20
  /// 用户报「串台了」：设置页显示 `https://…/api`，报错却说
  /// `127.0.0.1:9826` —— 那个端口他从没配过，除了「串台」得不出别的结论。
  ///
  /// # 三件它要说清的事
  ///
  /// 1. **在不在跑。**「不在跑」本身是一个要显示的状态（抄 Codex 的
  ///    `app-server ○ not running` —— 把架构事实告诉用户，而不是留白让他猜）。
  /// 2. **版本一不一致。** 抄 Claude Code 的 `daemon status`。对应的真实
  ///    故障是「桌面端一直跑着旧 agent，而看起来完全正常」。
  /// 3. **上面那节数字是谁的。** agent 在跑时 `/health` 是**它**答的 ——
  ///    `database unknown` 是它没有数据库，不是你的服务端坏了。
  Widget _localAgent(BuildContext context, WidgetRef ref) {
    // Web 上没有本地 agent，这一整节都不该出现 —— 画一个恒为「不在跑」的
    // 状态，只会让人去找一个这个平台上根本不存在的东西
    if (!kLocalAgentSupported) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final muted = scheme.onSurfaceVariant;
    final origin = ref.watch(localAgentOriginProvider);
    final health = ref.watch(healthProvider).value;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Divider(height: 24),
        Text('本机 agent', style: theme.textTheme.titleSmall),
        const SizedBox(height: 4),
        Text(
          '工具、读写文件、跑命令都在它里面。由界面自动拉起、随界面退出，'
          '端口每次由系统随机分 —— 所以报错里那个 127.0.0.1 不是你配的地址。',
          style: theme.textTheme.labelSmall?.copyWith(color: muted),
        ),
        const SizedBox(height: 8),
        ...switch (origin) {
          AsyncData(value: final o?) => _agentRunning(context, o, health),
          // 起不来 / 这台机器上没有它。**说清后果**：不是「少了个进程」，
          // 是工具全都不能用，而对话本身照常 —— 用户否则会以为整个坏了
          AsyncData() => [
            _capability(
              context,
              Icons.highlight_off,
              '没在跑 —— 已直接连远端',
              '对话照常，但读写本机文件、跑命令这些都不可用。'
                  '重启一次应用通常就好了。',
              scheme.error,
            ),
          ],
          AsyncError(:final error) => [
            _capability(
              context,
              Icons.error_outline,
              '起不来',
              '$error',
              scheme.error,
            ),
          ],
          _ => [
            _capability(context, Icons.hourglass_empty, '正在启动…', null, muted),
          ],
        },
      ],
    );
  }

  /// agent 在跑时那两三行。
  List<Widget> _agentRunning(
    BuildContext context,
    String origin,
    HealthStatus? health,
  ) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final muted = scheme.onSurfaceVariant;
    final out = <Widget>[
      _capability(
        context,
        Icons.check_circle_outline,
        '在跑 · $origin',
        null,
        muted,
      ),
    ];

    // 版本比对 —— 这一节最值钱的一行。
    //
    // ⚠️ **只在 `/health` 确实是 agent 答的时候比**：它不在跑时那份 health
    // 来自远端部署，拿远端版本去比就是在比两个无关的数字，
    // 而结论会是一个不存在的「版本不一致」
    if (health != null && health.isLocalAgent) {
      final mine = AppConfig.appVersion;
      if (mine.isEmpty) {
        // 开发构建没有版本号（`just app` 不传 --dart-define）。
        // 这时**不比**，也不说「一致」—— 那是编出来的
        out.add(
          _capability(
            context,
            Icons.info_outline,
            'agent v${health.version}（这份界面没有版本号，比不了）',
            null,
            muted,
          ),
        );
      } else if (health.version == mine) {
        out.add(
          _capability(
            context,
            Icons.check_circle_outline,
            '版本一致（v$mine）',
            null,
            muted,
          ),
        );
      } else {
        out.add(
          _capability(
            context,
            Icons.warning_amber_rounded,
            'agent 是 v${health.version}，界面是 v$mine',
            '两个版本混着跑，行为无法预期。重装一次桌面端让两者对齐。',
            scheme.error,
          ),
        );
      }
      // 上面那节数字是谁的 —— 不说的话，agent 报的 `database unknown`
      // 读起来像「我的服务端坏了」
      final link = health.server;
      if (link != null) {
        out.add(
          _capability(
            context,
            link.reachable ? Icons.cloud_done_outlined : Icons.cloud_off,
            link.reachable
                ? '上面那节是 agent 报的；它连着 ${link.remote}'
                : '上面那节是 agent 报的；它连不上 ${link.remote}',
            link.backlog > 0 ? '还有 ${link.backlog} 条对话等着补写，联网后自动重放。' : null,
            link.reachable ? muted : scheme.error,
          ),
        );
      }
    }
    return out;
  }

  /// 云端对话这项能力此刻在不在，以及不在时是哪一种「不在」。
  ///
  /// # 为什么它不是红的
  ///
  /// 「这个部署没有沙箱」是自托管与纯本机形态的**正常样子**，不是故障。
  /// 画成红色错误的后果很具体：用户会去修一个没坏的东西，而真正的失败
  /// （编排服务在、但它够不着记忆服务）反而淹没在同一片红里。
  ///
  /// 所以只有 [CloudChatStatus.blocked] 这一档才换颜色 —— 那一档是
  /// 「本该能跑，现在跑不了」，用户确实有事可做，而下一步是什么由
  /// [SandboxHealth.reason] 说。
  ///
  /// Mock 数据源下同样显示（它答的也是 absent）：那时「云端对话不可用」
  /// 是实情 —— 那个模式一个网络请求都不发。
  Widget _cloudChat(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;

    return ref
        .watch(sandboxHealthProvider)
        .when(
          loading: () =>
              _capability(context, Icons.cloud_queue, '云端对话：检测中…', null, muted),
          // 探测本身出错也不画红。这个方法承诺不抛（见 `CortexApi.sandboxHealth`），
          // 走到这里说明是别的地方炸了 —— 而对用户来说「问不出来」与
          // 「没有」的下一步是同一件事：云端对话别指望
          error: (e, _) =>
              _capability(context, Icons.cloud_off, '云端对话：探测不到', '$e', muted),
          data: (s) => switch (s.status) {
            CloudChatStatus.ready => _capability(
              context,
              Icons.cloud_done,
              '云端对话：可用',
              // 版本单独说：与上面那个 version 可能来自两个不同的进程，
              // 滚更新滚到一半时这是唯一看得出来的地方
              s.version == null ? null : 'agent 编排服务 ${s.version}',
              muted,
            ),
            CloudChatStatus.absent => _capability(
              context,
              Icons.cloud_off,
              '云端对话：不可用（这个部署没有沙箱）',
              '本机对话不受影响 —— 自托管与纯本机形态本来就没有它。',
              muted,
            ),
            CloudChatStatus.blocked => _capability(
              context,
              Icons.cloud_off,
              '云端对话：现在跑不起来',
              s.reason,
              theme.colorScheme.tertiary,
            ),
          },
        );
  }

  Widget _capability(
    BuildContext context,
    IconData icon,
    String label,
    String? note,
    Color color,
  ) {
    final theme = Theme.of(context);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Icon(icon, size: 14, color: color),
        const SizedBox(width: 6),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                label,
                style: theme.textTheme.bodySmall?.copyWith(color: color),
              ),
              if (note != null)
                Text(
                  note,
                  style: theme.textTheme.labelSmall?.copyWith(color: color),
                ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _kv(BuildContext context, String k, String v) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Row(
        children: [
          SizedBox(
            width: 80,
            // 键用第三级前景：这一列是**标签**，值才是内容。
            // 两者同色的话眼睛要逐行分辨哪边是哪边
            child: Text(
              k,
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
          ),
          Text(
            v,
            style: theme.textTheme.bodySmall?.copyWith(
              fontFamily: 'monospace',
              color: theme.colorScheme.onSurface,
            ),
          ),
        ],
      ),
    );
  }
}
