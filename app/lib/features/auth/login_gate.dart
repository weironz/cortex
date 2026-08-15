import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/local_agent.dart';
import '../../auth/token_store.dart';
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
  final TextEditingController _tokenController = TextEditingController();
  final TextEditingController _userController = TextEditingController();
  final TextEditingController _passwordController = TextEditingController();
  bool _revealToken = false;
  bool _revealPassword = false;

  /// 走旧的预共享 token 那条路吗。
  ///
  /// **默认否**。账号密码是现在的主路；token 留着是因为 CLI、现有安装与
  /// 单用户自托管都还在用它，一次性切断会让它们当天全部失联。
  ///
  /// 但它不该是**第一眼**看到的东西：粘一串 64 位十六进制是这个应用里
  /// 最糟的一步交互，而绝大多数人根本不需要它。
  bool _useToken = false;

  @override
  void initState() {
    super.initState();
    // 平台能无人值守拿到的凭据先填上（桌面端的 CORTEXD_TOKEN、
    // Web 端勾了"记住"的那份）。会走到这个界面，说明它缺了或者被拒了。
    final seeded = ref.read(authControllerProvider).token ?? '';
    _tokenController.text = seeded;
    // 环境里**确实有**一份 token 才默认落到那条路上 —— 那种情况下这个人
    // 显然是老用户或自托管，让他先看账号密码框是白绕一圈
    _useToken = seeded.isNotEmpty;
  }

  @override
  void dispose() {
    _urlController.dispose();
    _tokenController.dispose();
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
    if (_useToken) {
      await auth.signIn(
        _tokenController.text,
        remember: ref.read(authControllerProvider).remember,
      );
      return;
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
    final auth = ref.read(authControllerProvider.notifier);
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
                Text('连接 cortexd', style: theme.textTheme.headlineSmall),
                const SizedBox(height: 6),
                Text(
                  unreachable
                      ? '还没连上 daemon。先确认地址，再填凭据。'
                      : _useToken
                      ? '用这个部署的预共享 token 登录（旧方式）。'
                      : '用你的账号登录。登录状态会记住 30 天，关掉再打开不用重来。',
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

                if (_useToken)
                  TextField(
                    controller: _tokenController,
                    // 默认遮住、可点开：这是一串粘进来的 64 位十六进制，
                    // 而一次看不见的粘贴迟早会错一次，且永远查不出为什么
                    obscureText: !_revealToken,
                    autocorrect: false,
                    enableSuggestions: false,
                    autofocus: true,
                    enabled: !state.busy,
                    decoration: InputDecoration(
                      labelText: 'CORTEXD_TOKEN',
                      hintText: '64 位十六进制',
                      border: const OutlineInputBorder(),
                      suffixIcon: IconButton(
                        onPressed: () =>
                            setState(() => _revealToken = !_revealToken),
                        iconSize: 18,
                        tooltip: _revealToken ? '隐藏' : '显示',
                        icon: Icon(
                          _revealToken
                              ? Icons.visibility_off_outlined
                              : Icons.visibility_outlined,
                        ),
                      ),
                    ),
                    onSubmitted: (_) => _submit(),
                  )
                else ...[
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
                ],

                const SizedBox(height: 10),
                if (_useToken && kCanRememberToken)
                  CheckboxListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    controlAffinity: ListTileControlAffinity.leading,
                    value: state.remember,
                    onChanged: (v) => auth.setRemember(v ?? false),
                    title: const Text('在这个标签页内记住'),
                    subtitle: Text(
                      kTokenStorageNote,
                      style: theme.textTheme.labelSmall,
                    ),
                  )
                else
                  Text(
                    // 密码登录时这句话不是"要不要记住"的开关，而是在说明
                    // 凭据存在哪儿 —— 用户凭它判断这台机器安不安全
                    kTokenStorageNote,
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
                  ),

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
                  child: Text(
                    state.busy
                        ? '连接中…'
                        : _useToken
                        ? '连接'
                        : '登录',
                  ),
                ),
                const SizedBox(height: 6),
                Align(
                  alignment: Alignment.centerLeft,
                  child: TextButton(
                    onPressed: state.busy
                        ? null
                        : () => setState(() => _useToken = !_useToken),
                    child: Text(_useToken ? '改用账号密码登录' : '用预共享 token 登录（旧方式）'),
                  ),
                ),
                if (_useToken) ...[
                  const SizedBox(height: 4),
                  const _GenerateTokenHint(),
                ],

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
                const SizedBox(height: 12),
                Row(
                  children: [
                    Expanded(
                      child: Text(
                        '只想看看界面长什么样？用内存夹具（假数据）。',
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    ),
                    TextButton(
                      onPressed: () =>
                          ref.read(appConfigProvider.notifier).setUseMock(true),
                      child: const Text('用 Mock 数据源'),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// The exact command that produces a credential, copyable.
///
/// Worth its own widget because getting this wrong is the single most likely
/// way to fail here: `cortexd --generate-token` prints **two** values, and the
/// one that goes in the field above is the plaintext, not the digest that goes
/// into the server's `.env`. They are both 64 hex characters and they look
/// identical.
class _GenerateTokenHint extends StatelessWidget {
  const _GenerateTokenHint();

  static const _command = 'cortexd --generate-token';

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Container(
      padding: const EdgeInsets.fromLTRB(11, 9, 7, 9),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest.withValues(alpha: 0.55),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: SelectableText(
                  _command,
                  style: theme.textTheme.bodySmall?.copyWith(
                    fontFamily: 'monospace',
                    color: scheme.onSurface,
                  ),
                ),
              ),
              IconButton(
                onPressed: () =>
                    Clipboard.setData(const ClipboardData(text: _command)),
                iconSize: 15,
                visualDensity: VisualDensity.compact,
                tooltip: '复制命令',
                icon: const Icon(Icons.copy_rounded),
              ),
            ],
          ),
          const SizedBox(height: 4),
          Text(
            '它会打印两行：`CORTEX_AUTH_TOKEN_SHA256=…` 写进服务端 .env，'
            '`CORTEXD_TOKEN=…` 填到上面。两串都是 64 位十六进制，长得一模一样 —— '
            '填错了服务端只会说「没通过」，因为它对收到的东西还要再哈希一次。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}
