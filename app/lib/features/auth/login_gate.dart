import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

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
  bool _revealToken = false;

  @override
  void initState() {
    super.initState();
    // Pre-fill from whatever the platform could supply unattended, so a desktop
    // user who set `CORTEXD_TOKEN` and a web user who ticked "remember" both
    // land here with the field already correct — they only ever see this screen
    // when the seed was missing or rejected.
    _tokenController.text = ref.read(authControllerProvider).token ?? '';
  }

  @override
  void dispose() {
    _urlController.dispose();
    _tokenController.dispose();
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
    await auth.signIn(
      _tokenController.text,
      remember: ref.read(authControllerProvider).remember,
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
                      : '这个 cortexd 开着认证，需要一份预共享 token 才能访问记忆库。',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
                ),
                const SizedBox(height: 22),

                TextField(
                  controller: _urlController,
                  autocorrect: false,
                  decoration: const InputDecoration(
                    labelText: 'cortexd 地址',
                    hintText: 'http://127.0.0.1:8080',
                    border: OutlineInputBorder(),
                  ),
                  onSubmitted: (_) => _submit(),
                ),
                const SizedBox(height: 14),

                TextField(
                  controller: _tokenController,
                  // Obscured by default, revealable on demand: this is a
                  // 64-character hex string that gets pasted, and a paste you
                  // cannot verify is a paste you will get wrong once and never
                  // work out why.
                  obscureText: !_revealToken,
                  autocorrect: false,
                  enableSuggestions: false,
                  autofocus: true,
                  // Reaching the daemon is a prerequisite for the token
                  // mattering; there is nothing useful to submit while it is
                  // unreachable except the address above.
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
                ),

                const SizedBox(height: 10),
                if (kCanRememberToken)
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
                  child: Text(state.busy ? '连接中…' : '连接'),
                ),
                const SizedBox(height: 10),
                const _GenerateTokenHint(),

                const Divider(height: 34),
                // The escape hatch. Without it, a first-time user with no
                // daemon at all is stuck on this screen with no way to see the
                // app — and "run cortexd first" is a worse onboarding than
                // "look around with fixtures".
                Row(
                  children: [
                    Expanded(
                      child: Text(
                        '没有 daemon？可以先用内存夹具离线看界面。',
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
                onPressed: () => Clipboard.setData(
                  const ClipboardData(text: _command),
                ),
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
