import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/local_agent.dart';
import '../../state/app_providers.dart';
import '../../state/auth_controller.dart';
import '../shell/app_shell.dart';

/// Stands between the app and the daemon's front door.
///
/// Renders [AppShell] once the daemon either accepted a credential or told us
/// it does not want one. Until then it renders the reason it cannot — which is
/// the whole point: before this existed, a client pointed at any real `cortexd`
/// showed an empty session list and a `DOWN` badge, and nothing anywhere said
/// "you need a token".
class LoginGate extends ConsumerWidget {
  const LoginGate({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final phase = ref.watch(authControllerProvider.select((s) => s.phase));

    return switch (phase) {
      AuthPhase.ready => const AppShell(),
      // The probe is one unauthenticated GET against localhost in the common
      // case; a full-screen spinner is right for something that resolves in
      // milliseconds and gates everything behind it.
      AuthPhase.probing => const _Probing(),
      AuthPhase.needsToken || AuthPhase.unreachable => const _LoginScreen(),
    };
  }
}

class _Probing extends StatelessWidget {
  const _Probing();

  @override
  Widget build(BuildContext context) => const Scaffold(
    body: Center(
      child: SizedBox(
        width: 22,
        height: 22,
        child: CircularProgressIndicator(strokeWidth: 2.2),
      ),
    ),
  );
}

class _LoginScreen extends ConsumerStatefulWidget {
  const _LoginScreen();

  @override
  ConsumerState<_LoginScreen> createState() => _LoginScreenState();
}

class _LoginScreenState extends ConsumerState<_LoginScreen> {
  late final TextEditingController _urlController = TextEditingController(
    text: ref.read(appConfigProvider).baseUrl,
  );
  final TextEditingController _userController = TextEditingController();
  final TextEditingController _passwordController = TextEditingController();
  bool _revealPassword = false;

  /// 走旧的预共享 token 那条路吗。
  ///

  @override
  void initState() {
    super.initState();
  }

  @override
  void dispose() {
    _urlController.dispose();
    _userController.dispose();
    _passwordController.dispose();
    super.dispose();
  }

  Future<void> _submit() async {
    final auth = ref.read(authControllerProvider.notifier);
    final url = _urlController.text.trim();
    if (url.isNotEmpty && url != ref.read(appConfigProvider).baseUrl) {
      // Changing the address resets the gate (a token is scoped to one daemon),
      // so the sign-in has to follow the reset rather than race it.
      ref.read(appConfigProvider.notifier).setBaseUrl(url);
      await auth.probe();
      if (!mounted) return;
      // The new daemon may not want a token at all, in which case the probe has
      // already opened the door and there is nothing to sign in with.
      if (ref.read(authControllerProvider).isReady) return;
    }
    await auth.signInWithPassword(
      _userController.text,
      _passwordController.text,
    );
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final state = ref.watch(authControllerProvider);
    final unreachable = state.phase == AuthPhase.unreachable;

    return Scaffold(
      body: Center(
        child: SingleChildScrollView(
          padding: const EdgeInsets.all(28),
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 460),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text('登录 Cortex', style: theme.textTheme.headlineSmall),
                const SizedBox(height: 6),
                Text(
                  unreachable
                      ? '还没连上服务端。先确认地址，再填账号。'
                      : '登录状态会记住 30 天，关掉再打开不用重来。',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
                ),
                const SizedBox(height: 22),

                // 要的是**部署入口**，不是记忆服务本身 —— 理由见设置页
                // 「连接」那一格上方那段注释。这里与那里必须说同一件事：
                // 一个人只会在其中一处第一次填地址，说法不一致等于漏掉一半
                TextField(
                  controller: _urlController,
                  autocorrect: false,
                  decoration: const InputDecoration(
                    labelText: '部署入口地址',
                    hintText: 'https://<域名>/api',
                    helperText: '本机开发是 http://127.0.0.1:5173',
                    border: OutlineInputBorder(),
                  ),
                  onSubmitted: (_) => _submit(),
                ),
                const SizedBox(height: 14),

                TextField(
                  controller: _userController,
                  autocorrect: false,
                  enableSuggestions: false,
                  autofocus: true,
                  enabled: !state.busy,
                  // 密码管理器要认得出这是登录表单，否则它不会来填
                  autofillHints: const [AutofillHints.username],
                  decoration: const InputDecoration(
                    labelText: '用户名',
                    border: OutlineInputBorder(),
                  ),
                  onSubmitted: (_) => _submit(),
                ),
                const SizedBox(height: 14),
                TextField(
                  controller: _passwordController,
                  obscureText: !_revealPassword,
                  autocorrect: false,
                  enableSuggestions: false,
                  enabled: !state.busy,
                  autofillHints: const [AutofillHints.password],
                  decoration: InputDecoration(
                    labelText: '密码',
                    border: const OutlineInputBorder(),
                    suffixIcon: IconButton(
                      onPressed: () =>
                          setState(() => _revealPassword = !_revealPassword),
                      iconSize: 18,
                      tooltip: _revealPassword ? '隐藏' : '显示',
                      icon: Icon(
                        _revealPassword
                            ? Icons.visibility_off_outlined
                            : Icons.visibility_outlined,
                      ),
                    ),
                  ),
                  onSubmitted: (_) => _submit(),
                ),

                const SizedBox(height: 10),
                // 「凭据存在哪儿」那一大段说明**删掉了**：它讲的是 token 那条路
                // 的存储权衡，而 token 登录已经不在这个界面上了。密码登录换回来的
                // 是 refresh token，由平台的凭据库保管 —— 那件事不需要用户决策，
                // 也就不该占着登录页最显眼的位置。
                //
                if (state.error != null) ...[
                  const SizedBox(height: 14),
                  Container(
                    padding: const EdgeInsets.all(11),
                    decoration: BoxDecoration(
                      color: scheme.errorContainer,
                      borderRadius: BorderRadius.circular(8),
                    ),
                    child: Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Icon(
                          Icons.error_outline_rounded,
                          size: 16,
                          color: scheme.error,
                        ),
                        const SizedBox(width: 9),
                        Expanded(
                          child: SelectableText(
                            state.error!,
                            style: theme.textTheme.bodySmall?.copyWith(
                              color: scheme.onErrorContainer,
                            ),
                          ),
                        ),
                      ],
                    ),
                  ),
                ],

                const SizedBox(height: 18),
                FilledButton(
                  onPressed: state.busy ? null : _submit,
                  child: Text(state.busy ? '登录中…' : '登录'),
                ),
                const Divider(height: 34),
                // The escape hatch. Without it, a first-time user with no
                // daemon at all is stuck on this screen with no way to see the
                // app — and "run cortexd first" is a worse onboarding than
                // "look around with fixtures".
                // 离线使用：**不是**夹具，是真的能干活 —— 真模型、真工具、
                // 真读写本机文件。唯一缺的是记忆。
                //
                // 放在 mock 上面且更醒目：一个连不上服务器的人想要的是
                // 「那我先用着」，而不是「让我看看界面长什么样」
                //
                // ── 只在有本机 agent 的平台上给 ──
                //
                // 这一整块**此前没有平台判断**，于是 Web 上也画了出来 ——
                // 而那里 `kLocalAgentSupported` 是 false：没有进程可起、没有
                // 文件系统、没有本机凭据库。卡片上「能读写你本机的文件」
                // 这句话在浏览器里是**假的**，点下去只会进一个什么都干不了的
                // 模式。2026-08-15 发现。
                //
                // 一个给不出承诺的入口比没有入口更糟：没有的话用户去想别的办法，
                // 有的话他会以为自己已经解决了。
                if (kLocalAgentSupported)
                  Card.filled(
                    margin: EdgeInsets.zero,
                    child: Padding(
                      padding: const EdgeInsets.all(12),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Row(
                            children: [
                              Icon(
                                Icons.cloud_off_outlined,
                                size: 18,
                                color: scheme.onSurfaceVariant,
                              ),
                              const SizedBox(width: 8),
                              Text('先离线用着', style: theme.textTheme.titleSmall),
                            ],
                          ),
                          const SizedBox(height: 6),
                          // 强调用真的粗体，**不要在 Text 里写 Markdown 星号** ——
                          // 那是纯文本组件，星号会原样显示给用户看。
                          // 这个错在 0.1.6 发版前的目视冒烟里被抓到，
                          // 而所有 widget 测试都是绿的：它们断言的是文字内容，
                          // 而字面星号恰恰**在**文字内容里
                          Text.rich(
                            TextSpan(
                              style: theme.textTheme.bodySmall?.copyWith(
                                color: scheme.onSurfaceVariant,
                              ),
                              children: [
                                const TextSpan(text: '不连服务器也能对话、能读写你本机的文件。'),
                                TextSpan(
                                  text: '这段时间不会有记忆',
                                  style: TextStyle(
                                    fontWeight: FontWeight.w700,
                                    color: scheme.onSurface,
                                  ),
                                ),
                                const TextSpan(
                                  text: ' —— 对话会排进本地队列，以后接上服务器时自动补回去。\n',
                                ),
                                // 云端工作区那一条**必须在这儿说**，而不是等用户
                                // 发现 agent 找不到文件。
                                //
                                // 离线时那些会话的执行现场在服务器上，够不到；
                                // 本地 agent 照旧跑，但手上只有这台机器
                                // （没绑目录就是一个文件工具都没有）。不说的话，
                                // 用户看到的是「它昨天还能读那个文件，今天说没有」
                                TextSpan(
                                  text: '云端工作区也够不到',
                                  style: TextStyle(
                                    fontWeight: FontWeight.w700,
                                    color: scheme.onSurface,
                                  ),
                                ),
                                const TextSpan(
                                  text:
                                      ' —— 那些会话的文件在服务器上，'
                                      '离线期间只有绑了本机目录的会话动得了文件。\n'
                                      '需要先在设置里配好本机模型（也可以进去之后再配）。',
                                ),
                              ],
                            ),
                          ),
                          const SizedBox(height: 8),
                          Align(
                            alignment: Alignment.centerRight,
                            child: FilledButton.tonal(
                              onPressed: () => ref
                                  .read(appConfigProvider.notifier)
                                  .setOffline(true),
                              child: const Text('离线使用'),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
