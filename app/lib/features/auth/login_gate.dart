import 'package:flutter/foundation.dart' show kIsWeb;
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

/// 地址字段这一刻该不该出现。
///
/// 拆成纯函数是为了**测得到 web 那条分支**：`kIsWeb` 在 VM 测试里恒为
/// false，直接写在 build 里的话，「web 上不许出现地址字段」这条断言
/// 只能靠跑一次浏览器来验 —— 而那正是这个仓库里没人会跑第二次的那种测试。
@visibleForTesting
bool shouldShowEndpointField({
  required bool isWeb,
  required bool expanded,
  required bool unreachable,
}) => !isWeb && (expanded || unreachable);

class _LoginScreen extends ConsumerStatefulWidget {
  const _LoginScreen();

  @override
  ConsumerState<_LoginScreen> createState() => _LoginScreenState();
}

class _LoginScreenState extends ConsumerState<_LoginScreen> {
  /// ⚠️ **在 `initState` 里建，不要写成 `late final … = ref.read(…)`。**
  ///
  /// 那个懒初始化以前是安全的，因为地址字段每次 build 都渲染，第一次触碰
  /// 必然发生在 build 里。地址字段收起来之后（见 [_showEndpoint]）它可能
  /// **一次都不被渲染** —— 于是第一次触碰变成了 `dispose()` 里那句
  /// `_urlController.dispose()`，而那时 widget 已经卸载，
  /// `ref.read` 当场 StateError：「Using "ref" when a widget is about to
  /// or has been unmounted is unsafe」。
  ///
  /// 症状是整棵树在 finalize 时抛异常，而**与地址字段毫无关系的用例**
  /// （「默认收的是账号密码」）跟着一起红。2026-08-21 被测试抓到。
  late final TextEditingController _urlController;
  final TextEditingController _userController = TextEditingController();
  final TextEditingController _passwordController = TextEditingController();
  bool _revealPassword = false;

  /// 地址字段展开了吗。
  ///
  /// # 为什么默认收起来
  ///
  /// 「部署入口地址」这个问题**只有自托管的人答得上来**，而它此前是第一屏
  /// 上第一个字段 —— 每个新用户都要先处理一个与自己无关的东西。
  ///
  /// 抄 LobeHub 桌面端的 `showEndpoint`：默认只有一个登录按钮加一行小字
  /// 「连接到你自己的服务实例」，点了才出现输入框。三家同类产品
  /// （它、Chatbox、AnythingLLM）没有一家把地址放在第一屏，
  /// 见 docs/agent-lifecycle-survey.md。
  ///
  /// # 但连不上时要自己弹出来
  ///
  /// 编译期默认值是 `127.0.0.1:8080`，那个值**对任何有真部署的人都是错的**
  /// （见 `AppConfig.defaultBaseUrl` 的注释）。收起来而不自动展开的话，
  /// 一个连不上的人看到的是「登录失败」加一个没有出路的屏 —— 而出路
  /// 恰好藏在他不知道要点的那行小字后面。
  bool _showEndpoint = false;

  @override
  void initState() {
    super.initState();
    _urlController = TextEditingController(
      text: ref.read(appConfigProvider).baseUrl,
    );
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
    // 连不上时**强制展开**：那一刻地址就是最可疑的东西，而它默认是
    // `127.0.0.1:8080`。用 `||` 而不是在 setState 里改 `_showEndpoint` ——
    // 后者要在 build 里改状态，而且连上之后还得记得收回去
    final showEndpoint = shouldShowEndpointField(
      isWeb: kIsWeb,
      expanded: _showEndpoint,
      unreachable: unreachable,
    );

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

                // ── 地址字段 ──
                //
                // **Web 上一个字都不给。** 那份构建的 `CORTEX_BASE_URL` 传的是
                // 空串，一切走同源根路径（见 `AppConfig.defaultBaseUrl`）——
                // 它不是「可以不填」，是**填了就坏**：请求会从 nginx 同源那条
                // 路挪到一个绝对地址上去，而那条路没人接。
                //
                // 桌面上默认收起，见 `_showEndpoint`。
                if (showEndpoint) ...[
                  // 要的是**部署入口**，不是记忆服务本身 —— 理由见设置页
                  // 「连接」那一格上方那段注释。这里与那里必须说同一件事：
                  // 一个人只会在其中一处第一次填地址，说法不一致等于漏掉一半
                  TextField(
                    controller: _urlController,
                    autocorrect: false,
                    autofocus: unreachable,
                    decoration: const InputDecoration(
                      labelText: '部署入口地址',
                      hintText: 'https://<域名>/api',
                      helperText: '本机开发是 http://127.0.0.1:5173',
                      border: OutlineInputBorder(),
                    ),
                    onSubmitted: (_) => _submit(),
                  ),
                  const SizedBox(height: 14),
                ],

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

                // 自托管的那条出路。**措辞抄 LobeHub**：说「连接到你自己的
                // 部署」而不是「修改服务器地址」—— 前者描述的是一类用户
                // （我自己搭了一个），后者描述的是一个控件，而看不懂
                // 「部署入口地址」的人同样看不懂「修改」它意味着什么
                if (!kIsWeb && !showEndpoint)
                  Align(
                    child: TextButton(
                      onPressed: () => setState(() => _showEndpoint = true),
                      child: const Text('连接到你自己的部署'),
                    ),
                  ),
                const Divider(height: 34),
                // 连不上服务器时的出路。没有它，一个手上还没有 daemon 的人
                // 会卡在这一屏，而「先去把 cortexd 跑起来」是个很糟的开场。
                //
                // 离线使用：**不是**夹具，是真的能干活 —— 真模型、真工具、
                // 真读写本机文件。缺的是同步：这几轮不会进服务端。
                //
                // ⚠️ 这里曾经写着「唯一缺的是记忆」，界面文案也跟着写
                // 「这段时间不会有记忆」。**那句话在 2026-08-17 之后就是错的**：
                // 长期记忆整条拆去了 Cormex，这一侧连着服务器也不做记忆注入。
                // 说「离线时没有记忆」等于暗示联网时有 —— 承诺一个不存在的
                // 功能，与提示词里写没接的能力是同一个错。
                //
                // 这里曾经写着「放在 mock 上面且更醒目」，而**这一屏上从来
                // 没有过 mock 入口** —— 那句话描述的是一个没做出来的布局。
                // 今天连设置里那个开关也删了（见 connection_page），
                // 所以出路只有这一条：一个连不上服务器的人想要的是
                // 「那我先用着」，而不是「让我看看界面长什么样」。
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
                                  text: '这段时间不会同步到服务端',
                                  style: TextStyle(
                                    fontWeight: FontWeight.w700,
                                    color: scheme.onSurface,
                                  ),
                                ),
                                const TextSpan(
                                  text: ' —— 对话排进本地队列，接上服务器后自动补回去。\n',
                                ),
                                // **这一句此前没有，而它是离线模式最容易被误读的地方。**
                                //
                                // `cortex-local` 的 `list_sessions` 是纯转发
                                // （它不依赖 cortex-store，本机没有第二个库），
                                // 所以离线时会话列表只剩这次新建的草稿。
                                // 不说的话，用户看到的是「我昨天那些对话没了」——
                                // 而它们其实好好地在服务器上。
                                TextSpan(
                                  text: '也看不到以前的会话',
                                  style: TextStyle(
                                    fontWeight: FontWeight.w700,
                                    color: scheme.onSurface,
                                  ),
                                ),
                                const TextSpan(
                                  text:
                                      ' —— 历史在服务器上，接上就全回来。\n'
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
