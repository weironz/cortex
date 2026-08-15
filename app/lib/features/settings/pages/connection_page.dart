import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../auth/token_store.dart';
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
        SwitchListTile.adaptive(
          contentPadding: EdgeInsets.zero,
          value: config.useMock,
          onChanged: notifier.setUseMock,
          title: const Text('使用 Mock 数据源'),
          subtitle: Text(
            config.useMock ? '不发起任何网络请求，数据来自内存夹具。' : '直连 cortexd。',
            style: theme.textTheme.bodySmall,
          ),
        ),
        const SizedBox(height: 8),
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
              onPressed: () => ref.invalidate(healthProvider),
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
              _kv(context, 'version', h.version),
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
                        '这个 cortexd 关闭了认证：任何能连上 '
                        '${config.baseUrl} 的人都拥有全部记忆。'
                        '只有监听地址确实是回环时才可接受。',
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: scheme.error,
                        ),
                      ),
                    ),
                  ],
                ),
              ],
            ],
          ),
        ),
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

  Widget _kv(BuildContext context, String k, String v) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Row(
        children: [
          SizedBox(
            width: 80,
            child: Text(k, style: theme.textTheme.labelSmall),
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
