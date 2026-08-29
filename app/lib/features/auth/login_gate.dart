import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/app_config.dart';
import '../../core/local_agent.dart';
import '../../state/app_providers.dart';
import '../../state/auth_controller.dart';
import '../shell/app_shell.dart';
import '../../core/theme.dart';

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
  final TextEditingController _confirmController = TextEditingController();
  bool _revealPassword = false;

  /// 这一屏此刻是注册表单吗。
  ///
  /// 只有在部署**开着注册**时才可能为真（入口本身按 `openRegistrationProvider`
  /// 摆放）；万一开关在中途关掉（切换了部署地址），build 里会把它按登录
  /// 表单画 —— 判据是 `_registerMode && openRegistration`，不是这个布尔单独。
  bool _registerMode = false;

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
    _confirmController.dispose();
    super.dispose();
  }

  Future<void> _submit({bool register = false}) async {
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
    if (register) {
      // 「两次输入不一致」是唯一一条客户端自己判的：它防的是打错字，
      // 而不是复刻服务端规则 —— 长度等规则由服务端的拒绝文案说话
      if (_passwordController.text != _confirmController.text) {
        auth.reportFormError('两次输入的密码不一致。');
        return;
      }
      await auth.signUpWithPassword(
        _userController.text,
        _passwordController.text,
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
    final unreachable = state.phase == AuthPhase.unreachable;
    // 只有部署明确说开了才摆注册入口 —— 摆一个必然 403 的入口比没有更糟
    // （约束 2）。加载中 / 探测失败都按关闭画，判据统一在
    // `openRegistrationProvider`，这里只消费
    final openRegistration = ref.watch(openRegistrationProvider).value ?? false;
    final registering = _registerMode && openRegistration;
    // 连不上时**强制展开**：那一刻地址就是最可疑的东西，而它默认是
    // `127.0.0.1:8080`。用 `||` 而不是在 setState 里改 `_showEndpoint` ——
    // 后者要在 build 里改状态，而且连上之后还得记得收回去
    final showEndpoint = shouldShowEndpointField(
      isWeb: kIsWeb,
      expanded: _showEndpoint,
      unreachable: unreachable,
    );

    final known = ref
        .watch(appConfigProvider)
        .knownBaseUrls
        .where((u) => u != _urlController.text.trim())
        .toList();

    return Scaffold(
      body: Center(
        child: SingleChildScrollView(
          padding: const EdgeInsets.symmetric(horizontal: 28, vertical: 40),
          child: ConstrainedBox(
            // 380 而不是 460：一行输入框超过这个宽度，眼睛从标签扫到光标
            // 要跨过一段空白，读起来像一张表单而不是一道门
            constraints: const BoxConstraints(maxWidth: 380),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                // 品牌标。用的是仓库里那份**产品图标本尊**
                // （`web/icons/Icon-192.png`），不是现搭一个字母或者随便挑
                // 一个 Material 图标 —— 登录页是很多人见到这个产品的第一屏，
                // 摆一个临时替身在这儿，第一印象就是「半成品」。
                Center(
                  child: ClipRRect(
                    borderRadius: BorderRadius.circular(CortexTokens.radiusXl),
                    child: Image.asset(
                      'web/icons/Icon-192.png',
                      width: 64,
                      height: 64,
                      filterQuality: FilterQuality.medium,
                    ),
                  ),
                ),
                const SizedBox(height: 18),
                Text(
                  registering ? '注册 Cortex 账号' : '登录 Cortex',
                  textAlign: TextAlign.center,
                  style: theme.textTheme.headlineSmall?.copyWith(
                    fontWeight: FontWeight.w600,
                  ),
                ),
                // ── 你正在连哪 ──
                //
                // **常驻，不是出错才说。** 用户 2026-08-29 的原话：
                // 「关键是这个页面没有告诉我我连的是哪个环境啊，我没有点连
                // 自己的服务器，默认就应该永远连官方的服务器地址才对啊」。
                //
                // 那次的现场是：地址被之前调试 dev 时存下来了（127.0.0.1:5173），
                // 而登录页从头到尾没提过一个字 —— 直到点了登录，才从一行
                // Dart 的 SocketException 里露出来。
                //
                // 非官方地址用琥珀色：那不是错误（自托管是正常用法），
                // 但**是一件你该在动手前知道的事**。
                if (!kIsWeb) ...[
                  const SizedBox(height: 10),
                  _EndpointBanner(
                    url: _urlController.text.trim(),
                    onSwitch: (u) => setState(() => _urlController.text = u),
                    known: ref.watch(appConfigProvider).knownBaseUrls,
                    officialUrl: AppConfig.defaultBaseUrl,
                  ),
                ],
                // 这里曾有一句常驻副标题「登录状态会记住 30 天，关掉再打开
                // 不用重来」。删了：记住登录是行业默认预期，不值得占登录页
                // 视觉中心一行（Claude / ChatGPT 的登录页都不写）；而只要
                // 还存在任何被误踢的路径，这句承诺就摆在「登录已过期」的
                // 红字正上方 —— 读起来像嘲讽。正确的位置是修掉误踢，
                // 而不是提前辩解。unreachable 那句保留：它有用户可做的事
                if (unreachable) ...[
                  const SizedBox(height: 8),
                  Text(
                    '还没连上服务端。先确认地址，再填账号。',
                    textAlign: TextAlign.center,
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: scheme.error,
                      height: 1.5,
                    ),
                  ),
                ],
                const SizedBox(height: 26),

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
                    onChanged: (_) => setState(() {}),
                    decoration: InputDecoration(
                      labelText: '部署入口地址',
                      hintText: 'https://<域名>/api',
                      prefixIcon: const Icon(Icons.dns_outlined, size: 18),
                      // 收起来的入口**必须一直在**：地址是连不上时自动展开的，
                      // 而展开之后如果没有回头路，一个想换回另一个部署的人
                      // 在这一屏上就没有任何可点的东西了。
                      // 2026-08-21 实测到的原话是「进到这里就没办法切换回去了」
                      suffixIcon: kIsWeb || unreachable
                          ? null
                          : IconButton(
                              onPressed: () =>
                                  setState(() => _showEndpoint = false),
                              iconSize: 18,
                              tooltip: '收起',
                              icon: const Icon(Icons.expand_less_rounded),
                            ),
                    ),
                    onSubmitted: (_) => _submit(),
                  ),
                  // 用过的其它部署，一点就换。
                  //
                  // 没有它的话「切回另一个部署」= 凭记忆重打一串 URL，
                  // 而人记不住自己的部署地址是完全正常的事。
                  if (known.isNotEmpty) ...[
                    const SizedBox(height: 10),
                    Wrap(
                      spacing: 6,
                      runSpacing: 6,
                      children: [
                        for (final url in known)
                          ActionChip(
                            avatar: const Icon(Icons.history_rounded, size: 15),
                            label: Text(_shortHost(url)),
                            tooltip: url,
                            onPressed: () => setState(() {
                              _urlController.text = url;
                            }),
                          ),
                      ],
                    ),
                  ],
                  const SizedBox(height: 16),
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
                    prefixIcon: Icon(Icons.person_outline_rounded, size: 18),
                  ),
                  onSubmitted: (_) => _submit(register: registering),
                ),
                const SizedBox(height: 12),
                TextField(
                  controller: _passwordController,
                  obscureText: !_revealPassword,
                  autocorrect: false,
                  enableSuggestions: false,
                  enabled: !state.busy,
                  autofillHints: const [AutofillHints.password],
                  decoration: InputDecoration(
                    labelText: '密码',
                    // 注册时把服务端唯一的硬规则提前说出来 —— 让用户输完
                    // 才被 400 打回，是把一次校验的成本换成一次全程重来
                    helperText: registering ? '至少 12 个字符，不要求大小写或符号组合' : null,
                    prefixIcon: const Icon(
                      Icons.lock_outline_rounded,
                      size: 18,
                    ),
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
                  onSubmitted: (_) => _submit(register: registering),
                ),
                if (registering) ...[
                  const SizedBox(height: 12),
                  TextField(
                    controller: _confirmController,
                    obscureText: !_revealPassword,
                    autocorrect: false,
                    enableSuggestions: false,
                    enabled: !state.busy,
                    decoration: const InputDecoration(
                      labelText: '确认密码',
                      prefixIcon: Icon(Icons.lock_reset_rounded, size: 18),
                    ),
                    onSubmitted: (_) => _submit(register: true),
                  ),
                ],

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
                      borderRadius: BorderRadius.circular(
                        CortexTokens.radiusLg,
                      ),
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

                const SizedBox(height: 20),
                FilledButton(
                  onPressed: state.busy
                      ? null
                      : () => _submit(register: registering),
                  style: FilledButton.styleFrom(
                    minimumSize: const Size.fromHeight(46),
                  ),
                  child: state.busy
                      // 转圈而不是把文字换成「登录中…」：按钮宽度不变，
                      // 而「有没有在动」比多读四个字更快看出来
                      ? const SizedBox(
                          width: 18,
                          height: 18,
                          child: CircularProgressIndicator(
                            strokeWidth: 2,
                            color: Colors.white,
                          ),
                        )
                      : Text(registering ? '注册并登录' : '登录'),
                ),

                // ── 注册入口 ──
                //
                // **仅当部署开着注册**（openRegistrationProvider，服务端说了
                // 算）才出现 —— 关着的部署上摆这个入口，点进去只能收获一个
                // 403，那正是约束 2 说的「界面替部署撒谎」。
                if (openRegistration) ...[
                  const SizedBox(height: 4),
                  Align(
                    child: TextButton(
                      onPressed: state.busy
                          ? null
                          : () =>
                                setState(() => _registerMode = !_registerMode),
                      child: Text(registering ? '已有账号？直接登录' : '注册新账号'),
                    ),
                  ),
                ],

                // 自托管的那条出路。**措辞抄 LobeHub**：说「连接到你自己的
                // 部署」而不是「修改服务器地址」—— 前者描述的是一类用户
                // （我自己搭了一个），后者描述的是一个控件，而看不懂
                // 「部署入口地址」的人同样看不懂「修改」它意味着什么
                if (!kIsWeb && !showEndpoint) ...[
                  const SizedBox(height: 4),
                  Align(
                    child: TextButton.icon(
                      onPressed: () => setState(() => _showEndpoint = true),
                      icon: const Icon(Icons.dns_outlined, size: 16),
                      label: const Text('连接到你自己的部署'),
                    ),
                  ),
                ],
                const SizedBox(height: 26),
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
                  Container(
                    padding: const EdgeInsets.all(14),
                    decoration: BoxDecoration(
                      color: scheme.surfaceContainerLow,
                      borderRadius: BorderRadius.circular(
                        CortexTokens.radiusXl,
                      ),
                      border: Border.all(color: scheme.outlineVariant),
                    ),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Row(
                          children: [
                            Icon(
                              Icons.cloud_off_outlined,
                              size: 17,
                              color: scheme.onSurfaceVariant,
                            ),
                            const SizedBox(width: 8),
                            Text(
                              '先离线用着',
                              style: theme.textTheme.titleSmall?.copyWith(
                                fontWeight: FontWeight.w600,
                              ),
                            ),
                            const Spacer(),
                            // 按钮挪到标题这一行：它此前吊在整段说明下面，
                            // 而那段说明是**读完才决定点不点**的东西 ——
                            // 一个已经知道自己要离线的人，不该为了找按钮
                            // 先扫过四行字
                            TextButton(
                              onPressed: () => ref
                                  .read(appConfigProvider.notifier)
                                  .setOffline(true),
                              style: TextButton.styleFrom(
                                padding: const EdgeInsets.symmetric(
                                  horizontal: 10,
                                ),
                                minimumSize: const Size(0, 32),
                                tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                              ),
                              child: const Text('离线使用'),
                            ),
                          ],
                        ),
                        const SizedBox(height: 4),
                        // **一行说完。** 这里曾是五行说明带两处粗体警示
                        // （不同步 / 看不到以前的会话 / 要配模型）—— 登录页上
                        // 它的视觉重量压过了主表单，而那几句后果是**进入离线
                        // 之后**才用得上的知识。现在它们由 `_OfflineBanner`
                        // 常驻横幅在正确的时刻说（auth_controller 里的原则：
                        // 消息在它有用的那一刻出现，而不是一直挂着）。
                        // 登录页只回答一个问题：这条路是什么、能不能走
                        Text(
                          '不连服务器也能对话、读写本机文件。不同步、看不到'
                          '服务器上的历史 —— 接上之后都回来。',
                          style: theme.textTheme.bodySmall?.copyWith(
                            color: scheme.onSurfaceVariant,
                            height: 1.5,
                          ),
                        ),
                      ],
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

/// 一串 URL 拿来当按钮上的字。
///
/// 只留 host（外加非默认端口）：完整的 `https://cortex.example.com/api`
/// 在一个小按钮上会被截断成 `https://cortex.exa…`，而被截掉的恰恰是
/// **区分两个部署的那一半**。同一台机器上的两个端口也要分得开，
/// 所以端口不能一起丢。
/// 这个地址属于哪一类。**判据只看 URL 本身**，不看「用户有没有点过
/// 那个按钮」—— 后者是个会被存档、被遗忘的历史事实，而人问的是「我现在
/// 连的是哪」。
enum EndpointKind {
  /// 与这份构建编译进去的官方地址一致。
  official,

  /// 回环地址：本机 dev / 自己跑的一份。
  loopback,

  /// 别的什么地方 —— 自托管部署。
  custom,
}

/// 分类。**纯函数，好测** —— 这一行的全部价值在于它说的是实话。
EndpointKind classifyEndpoint(String url, {required String officialUrl}) {
  final u = Uri.tryParse(url);
  if (u == null || u.host.isEmpty) return EndpointKind.custom;
  final host = u.host.toLowerCase();
  if (host == '127.0.0.1' || host == 'localhost' || host == '::1') {
    return EndpointKind.loopback;
  }
  final official = Uri.tryParse(officialUrl);
  if (official != null && official.host.toLowerCase() == host) {
    return EndpointKind.official;
  }
  return EndpointKind.custom;
}

/// 登录页上那一行「你正在连哪」。
class _EndpointBanner extends StatelessWidget {
  const _EndpointBanner({
    required this.url,
    required this.known,
    required this.onSwitch,
    required this.officialUrl,
  });

  final String url;
  final List<String> known;
  final void Function(String) onSwitch;

  /// 这份构建认的「官方」是哪个地址。
  ///
  /// **注入而不是直接读 `AppConfig.defaultBaseUrl`**：那是个编译期常量，
  /// 测试里改不了，于是「连着 dev 时要摆出换回官方的按钮」这条断言在测试
  /// 环境里永远为假 —— 而那正是这个组件最该被验到的一条。
  final String officialUrl;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final kind = classifyEndpoint(url, officialUrl: officialUrl);
    final (icon, label, tone) = switch (kind) {
      EndpointKind.official => (
        Icons.verified_outlined,
        '官方服务器',
        theme.cortex.foregroundTertiary,
      ),
      EndpointKind.loopback => (
        Icons.developer_board_outlined,
        '本机（开发环境）',
        theme.cortex.warning,
      ),
      EndpointKind.custom => (
        Icons.dns_outlined,
        '你自己的部署',
        theme.cortex.warning,
      ),
    };

    // 官方地址里**能切回去的那一个**。没有就不摆按钮 —— 一个点了没地方
    // 去的「换回官方」比没有更糟
    String? official;
    for (final u in [officialUrl, ...known]) {
      if (u.isNotEmpty &&
          classifyEndpoint(u, officialUrl: officialUrl) ==
              EndpointKind.official) {
        official = u;
        break;
      }
    }

    // 已经在官方上就不摆按钮 —— 一个「换回你已经在的地方」的按钮是噪音
    final switchTarget = kind == EndpointKind.official ? null : official;

    return Column(
      children: [
        Row(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            Icon(icon, size: 14, color: tone),
            const SizedBox(width: 6),
            Flexible(
              child: Text(
                '$label · ${_shortHost(url)}',
                textAlign: TextAlign.center,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.labelSmall?.copyWith(color: tone),
              ),
            ),
          ],
        ),
        if (switchTarget != null)
          TextButton(
            onPressed: () => onSwitch(switchTarget),
            style: TextButton.styleFrom(
              visualDensity: VisualDensity.compact,
              padding: const EdgeInsets.symmetric(horizontal: 8),
            ),
            child: const Text('换回官方服务器'),
          ),
      ],
    );
  }
}

String _shortHost(String url) {
  final u = Uri.tryParse(url);
  if (u == null || u.host.isEmpty) return url;
  return u.hasPort ? '${u.host}:${u.port}' : u.host;
}
