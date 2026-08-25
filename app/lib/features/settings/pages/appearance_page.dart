import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/motion.dart';
import '../../../state/app_providers.dart';
import '../widgets/settings_layout.dart';

/// 外观这一页：主题与动效。
///
/// # 为什么值得单独一页
///
/// 这两项此前**都没有家**：
///
/// * 主题只有顶栏那个循环图标按钮。图标按钮是个**快捷方式**，不是入口 ——
///   要点三下才知道它有三档，而且点错了要再绕一圈回来。更糟的是它没落盘，
///   每次启动都回到「跟随系统」。
/// * 动效压根没有开关，只读系统那一位。
///
/// 一个循环按钮加一个读不到的系统开关，合起来的效果是：用户能感觉到这两件事
/// 存在，却没有一个地方能**看见当前是哪一档**。
class AppearancePage extends ConsumerStatefulWidget {
  const AppearancePage({super.key});

  @override
  ConsumerState<AppearancePage> createState() => _AppearancePageState();
}

class _AppearancePageState extends ConsumerState<AppearancePage>
    with WidgetsBindingObserver {
  @override
  void initState() {
    super.initState();
    // 用户在系统设置里改了「显示动画效果」时重画这一页 —— 否则那行
    // 「系统现在：…」会一直停在打开这一页时的值上
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAccessibilityFeatures() => setState(() {});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final mode = ref.watch(themeModeProvider);
    final motion = ref.watch(motionPrefProvider);

    // ⚠️ **不能用 `MediaQuery`。**
    //
    // 三档在 `app.dart` 里已经把 `MediaQuery.disableAnimations` 改写掉了
    // （那正是它生效的方式），所以这一页读 `MediaQuery` 拿到的是
    // **当前生效值**，不是系统的意见。表现是这行小字变成一句同义反复：
    // 选「关闭」，它就跟着写「系统现在：关闭」—— 一个凭空捏造的系统设置。
    //
    // 要系统那一位，只能绕过整棵树去问平台本身。
    final systemReduced = WidgetsBinding
        .instance
        .platformDispatcher
        .accessibilityFeatures
        .disableAnimations;

    return ListView(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      children: [
        SettingsSection(
          title: '主题',
          // ⚠️ 别再写「顶栏那个图标」—— 顶栏的主题快捷图标 2026-08-25 撤了
          //（约束 2：界面只描述当下真的成立的入口）。命令面板那条还在
          description: '「跟随系统」会在系统切换深浅色时一起变。⌘K 命令面板里的「切换主题」改的是同一个设置。',
          children: [
            SettingsChoice<ThemeMode>(
              value: mode,
              onChanged: ref.read(themeModeProvider.notifier).set,
              options: const [
                (value: ThemeMode.system, label: '跟随系统', hint: '随系统深浅色'),
                (value: ThemeMode.light, label: '浅色', hint: '始终浅色'),
                (value: ThemeMode.dark, label: '深色', hint: '始终深色'),
              ],
            ),
          ],
        ),
        SettingsSection(
          title: '密度',
          description: '紧凑档把列表行高收 6px、正文缩到 14px —— 一屏多看几行。默认档为长回答留着行距。',
          children: [
            SettingsChoice<bool>(
              value: ref.watch(densityProvider),
              onChanged: (compact) =>
                  ref.read(densityProvider.notifier).set(compact: compact),
              options: const [
                (value: false, label: '默认', hint: '为长回答留行距'),
                (value: true, label: '紧凑', hint: '行高 −6px · 正文 14px'),
              ],
            ),
          ],
        ),
        SettingsSection(
          title: '动效',
          description:
              '这个界面里会动的东西几乎都是循环的 —— 等待回复的三个点、'
              '流式输出的光标、代码块复制后的提示。它们对前庭功能敏感的人'
              '会引起不适，对注意力障碍的人是一个永远在抢注意力的角落。',
          children: [
            SettingsChoice<MotionPref>(
              value: motion,
              onChanged: ref.read(motionPrefProvider.notifier).set,
              options: [
                (
                  value: MotionPref.system,
                  label: '跟随系统',
                  // 把系统此刻的值写出来。「跟随系统」是个**间接**档位，
                  // 不告诉用户它现在跟到了哪儿，等于让他去别处试
                  hint: systemReduced ? '系统现在：关闭' : '系统现在：开启',
                ),
                (value: MotionPref.full, label: '标准', hint: '始终开启'),
                (value: MotionPref.reduced, label: '关闭', hint: '始终关闭'),
              ],
            ),
            const SizedBox(height: 10),
            SettingsNote(
              child: Text(
                '「跟随系统」读的是 Windows 的「显示动画效果」、macOS 的'
                '「减弱动态效果」、浏览器的 prefers-reduced-motion。'
                '另外两档是显式覆盖 —— 系统开着但你在这里想要动效，'
                '或者系统没开而你不想要，都能表达。',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
            ),
          ],
        ),
      ],
    );
  }
}
