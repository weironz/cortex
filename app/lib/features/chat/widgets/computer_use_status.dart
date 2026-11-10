import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../state/chat_controller.dart';

typedef _StatusView = ({
  String title,
  String badge,
  String action,
  String scope,
  IconData icon,
});

/// Computer Use 的运行中提示，钉在输入框上方。
///
/// 这不是确认框，也不是预览：它只回答用户最需要立刻知道的三件事 ——
/// Cortex 正在使用电脑工具、这一步会不会改变桌面、以及停止键在哪。
/// 停止按钮仍然由 `MessageComposer` 那颗通用停止键承担；这里不造第二条
/// 控制通道，避免两颗按钮对同一个 run 有不同的“停成功了吗”答案。
class ComputerUseStatus extends ConsumerWidget {
  const ComputerUseStatus({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final view = ref.watch(
      chatControllerProvider.select((state) {
        final turn = state.streaming;
        if (turn == null) return null;
        for (final call in turn.toolCalls.reversed) {
          if (call.pending && _isComputerTool(call.name)) {
            return _viewFor(call.name, call.arguments);
          }
        }
        return null;
      }),
    );
    if (view == null) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      key: const ValueKey('computer_use:status'),
      width: double.infinity,
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 8),
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
      decoration: BoxDecoration(
        color: scheme.secondaryContainer.withValues(alpha: 0.55),
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        border: Border.all(color: scheme.secondary.withValues(alpha: 0.45)),
      ),
      child: Row(
        children: [
          Icon(view.icon, size: 18, color: scheme.onSecondaryContainer),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Wrap(
                  spacing: 8,
                  runSpacing: 3,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    Text(
                      view.title,
                      style: theme.textTheme.labelMedium?.copyWith(
                        color: scheme.onSecondaryContainer,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    DecoratedBox(
                      decoration: BoxDecoration(
                        color: scheme.surface.withValues(alpha: 0.72),
                        // 不走圆角五阶：状态标签是胶囊，两端保持全圆。
                        borderRadius: BorderRadius.circular(
                          CortexTokens.radiusPill,
                        ),
                      ),
                      child: Padding(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 7,
                          vertical: 2,
                        ),
                        child: Text(
                          view.badge,
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: scheme.onSecondaryContainer,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 3),
                Text(
                  view.action,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.onSecondaryContainer,
                    height: 1.35,
                  ),
                ),
                const SizedBox(height: 2),
                Text(
                  view.scope,
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: scheme.onSecondaryContainer.withValues(alpha: 0.8),
                    height: 1.4,
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  static bool _isComputerTool(String name) =>
      name.startsWith('computer_') ||
      name == 'screenshot' ||
      name == 'click' ||
      name == 'type_text' ||
      name == 'key' ||
      name == 'scroll';

  static _StatusView? _viewFor(String name, String? arguments) {
    final detail = _safeDetail(arguments);
    String withDetail(String action) =>
        detail == null ? action : '$action · $detail';

    return switch (name) {
      'computer_list' => (
        title: '正在观察本机窗口',
        badge: '只读',
        action: withDetail('列出允许窗口'),
        scope: '仅访问 allowlist 内的应用窗口；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.list_alt_rounded,
      ),
      'computer_observe' => (
        title: '正在观察本机窗口',
        badge: '只读',
        action: withDetail('观察窗口内容'),
        scope: '仅读取 allowlist 内的一个应用窗口；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.visibility_outlined,
      ),
      'computer_wait' => (
        title: '正在观察本机窗口',
        badge: '只读',
        action: withDetail('等待窗口达到指定状态'),
        scope: '仅在 allowlist 内的应用窗口中等待；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.hourglass_top_rounded,
      ),
      'computer_act' => (
        title: '正在操作本机窗口',
        badge: '会改变状态',
        action: withDetail('执行窗口动作'),
        scope: '动作落在真实桌面的 allowlist 应用窗口中；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.touch_app_outlined,
      ),
      'computer_browser_open' => (
        title: '正在启动隔离浏览器',
        badge: '隔离 profile',
        action: withDetail('创建一次性浏览器会话'),
        scope: '不会复用日常浏览器 profile；只访问站点 allowlist；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.public_rounded,
      ),
      'computer_browser_observe' => (
        title: '正在读取隔离网页',
        badge: '只读',
        action: withDetail('读取页面结构'),
        scope: '只返回允许站点的一次性结构化快照；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.web_asset_outlined,
      ),
      'computer_browser_act' => (
        title: '正在操作隔离网页',
        badge: '会改变状态',
        action: withDetail('执行网页动作'),
        scope: '动作发生在一次性浏览器 profile 和站点 allowlist 内；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.ads_click_rounded,
      ),
      'computer_browser_wait' => (
        title: '正在等待隔离网页',
        badge: '只读',
        action: withDetail('等待页面达到指定状态'),
        scope: '只会重新观察允许站点，不把超时当作完成；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.hourglass_top_rounded,
      ),
      'screenshot' => (
        title: '正在读取本机屏幕',
        badge: '只读',
        action: withDetail('截取当前屏幕'),
        scope: '这会读取当前屏幕内容；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.screenshot_monitor_rounded,
      ),
      'click' => (
        title: '正在操作本机桌面',
        badge: '会改变状态',
        action: withDetail('点击屏幕'),
        scope: '点击会发送到当前桌面；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.ads_click_rounded,
      ),
      'type_text' => (
        title: '正在操作本机桌面',
        badge: '会改变状态',
        action: withDetail('输入文字'),
        scope: '文字会发送到当前焦点窗口；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.keyboard_outlined,
      ),
      'key' => (
        title: '正在操作本机桌面',
        badge: '会改变状态',
        action: withDetail('发送按键'),
        scope: '按键会发送到当前焦点窗口；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.keyboard_command_key_rounded,
      ),
      'scroll' => (
        title: '正在操作本机桌面',
        badge: '会改变状态',
        action: withDetail('滚动屏幕'),
        scope: '滚动会作用于指针下的窗口；需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.swap_vert_rounded,
      ),
      _ => (
        title: '正在使用电脑工具',
        badge: '处理中',
        action: withDetail(name),
        scope: '需要立刻停止时点输入框右侧的停止键。',
        icon: Icons.desktop_windows_outlined,
      ),
    };
  }

  /// 只展示窗口/应用/动作这类定位信息。
  ///
  /// 当前后端只发 `调用 computer_observe`，这些字段通常不存在；将来若
  /// 协议真的把它们带进 compact arguments，这里也只取白名单。输入文字、
  /// 按键、元素值和密码类字段永远不能出现在运行态提示里。
  static String? _safeDetail(String? arguments) {
    final raw = arguments?.trim();
    if (raw == null || raw.isEmpty) return null;

    const allowed = <String, String>{
      'app': '应用',
      'application': '应用',
      'app_id': '应用',
      'window': '窗口',
      'window_id': '窗口',
      'window_title': '窗口',
      'title': '窗口',
      'action': '动作',
    };
    final body = raw.startsWith('(') && raw.endsWith(')')
        ? raw.substring(1, raw.length - 1)
        : raw;
    final out = <String>[];
    final seen = <String>{};
    for (final match in RegExp(
      r'(?:^|,\s*)(app_id|application|window_title|window_id|window|title|app|action)=([^,)]*)',
    ).allMatches(body)) {
      final key = match.group(1)!;
      final label = allowed[key]!;
      final value = _oneLine(match.group(2));
      if (value == null || !seen.add('$label:$value')) continue;
      out.add('$label $value');
    }
    return out.isEmpty ? null : out.join(' · ');
  }

  static String? _oneLine(String? raw) {
    if (raw == null) return null;
    var value = raw.trim();
    if (value.length >= 2 &&
        ((value.startsWith('"') && value.endsWith('"')) ||
            (value.startsWith("'") && value.endsWith("'")))) {
      value = value.substring(1, value.length - 1).trim();
    }
    value = value.replaceAll(RegExp(r'[\r\n\t]+'), ' ').trim();
    if (value.isEmpty || value == 'null' || value.startsWith('{')) return null;
    return value.length <= 56 ? value : '${value.substring(0, 53)}...';
  }
}
