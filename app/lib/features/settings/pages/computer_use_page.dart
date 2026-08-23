/// 「电脑操作」这一页 —— 一个壳，内容在 [`ComputerUseSection`]。
///
/// # 为什么值得占设置目录一整项
///
/// 它原先压在连接器页最下面。那个归类不算错（两者都在回答「模型手上有
/// 什么」），但代价是：**这个产品里权限最大的一组能力**，要往下滚过一整列
/// MCP server 才看得见。别的连接器交出去的是「一台别人写的 server」，
/// 只有这一条交出去的是你这台机器的屏幕、鼠标和键盘。
///
/// 决定一件事该不该有自己的入口，看的不是它属于哪一类，而是**用户会不会
/// 专门来找它** —— 想确认「我到底有没有开着这个」的人，不会想到去翻连接器。
///
/// # 为什么壳这么薄
///
/// 内容一行没搬。那一节里每一句话都是拿事故换来的（截图里有屏幕上的一切、
/// 点击落在真实窗口上、模型看不懂图时开了也不生效），搬家时改措辞是白白
/// 承担风险。所以这里只负责「做不到的时候说句人话」，其余原样委托。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import 'computer_use_section.dart';

class ComputerUsePage extends ConsumerWidget {
  const ComputerUsePage({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    // ⚠️ 与设置目录里那一项**同一个判据**（见 computerUseAvailableProvider）。
    //
    // 正常情况下走不到 else：做不到时菜单里根本没有这一项，进不来。
    // 但这一支不能省 —— 判据是异步的（/health 还没回来时是 false），
    // 而用户可能正停在这一页上：那时菜单项会消失，`settings_sheet` 把选中
    // 项回落到第一页，中间有一帧会画这里。空白比一句说明难懂得多。
    if (!ref.watch(computerUseAvailableProvider)) {
      return _Unavailable();
    }

    return ListView(
      padding: EdgeInsets.zero,
      children: const [ComputerUseSection()],
    );
  }
}

/// 做不到时说清是**哪一种**做不到。
///
/// 两种情形对用户是完全不同的事：跑在容器里（换个地方就有）与这个构建
/// 没编进那一组（Linux 桌面，换构建也没有）。但客户端手上只有 `/health`
/// 那个布尔，分不出来 —— 所以这里如实把两种都说出来，而不是猜一个。
class _Unavailable extends StatelessWidget {
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 420),
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            Icon(
              Icons.desktop_access_disabled_outlined,
              size: 32,
              color: theme.cortex.foregroundTertiary,
            ),
            const SizedBox(height: 12),
            Text('这里用不了电脑操作', style: theme.textTheme.titleSmall),
            const SizedBox(height: 6),
            Text(
              '这一轮的 agent 跑在没有屏幕的地方（云端会话在容器里），'
              '或者这个构建没有编进那一组工具（Linux 桌面）。\n\n'
              '在桌面端本机跑的会话里，这一页会自己出现。',
              textAlign: TextAlign.center,
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
                height: 1.6,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
