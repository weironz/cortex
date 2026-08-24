/// 设置页的版式件。
///
/// # 为什么要抽出来
///
/// 每一页各自现编一套版式的下场，在 2026-08-21 重排之前是这样的：
/// 「数据」页两条裸 `ListTile`、「用量」页把说明卡片画成 `surfaceContainerHigh`
/// 加一个手写的圆角 8、「模型服务」页又是另一套圆角与另一种标题字号。
/// 三页并排看过去，读起来像三个人做的三个产品。
///
/// 更要命的是它**不可核对**：规范里写着「卡片用 `surfaceContainer`、
/// 圆角走那五阶」，但没有任何一处代码是那句话的唯一落点，
/// 于是下一页照旧现编，而 review 时谁都看不出偏了。
///
/// 这里就是那个唯一落点。改版式改这个文件，不是改七页。
library;

import 'package:flutter/material.dart';

import '../../../core/theme.dart';

/// 一节：标题 + 可选说明 + 内容。
///
/// 标题用 `titleSmall`、说明用第三级前景 —— 说明是**解释性**的，
/// 与内容同色时它会和真正的数据抢注意力（规范第三节）。
class SettingsSection extends StatelessWidget {
  const SettingsSection({
    super.key,
    this.title,
    this.description,
    this.trailing,
    required this.children,
  });

  /// 节头。**单节页可以不传**：内容区大标题已经写着页名，节头再抄一遍
  /// 页名是口吃（技能页曾是「技能」「技能」两行叠着）—— 不传时只画
  /// description 与 trailing 那一行。
  final String? title;
  final String? description;

  /// 标题右边那个动作（「刷新」「添加」之类）。
  final Widget? trailing;

  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final desc = description == null
        ? null
        : ConstrainedBox(
            // 说明文字单独限宽：内容区整体封顶 1160 是为模型服务那种三栏
            // 页定的，一段中文说明摊成一整行 1100px 远超可读行宽（约 40 字）
            constraints: const BoxConstraints(maxWidth: 640),
            child: Text(
              description!,
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
          );
    return Padding(
      // 节与节之间靠**留白**分开，不靠分隔线：一页里画三条横线之后，
      // 真正需要一条线的地方（比如表头）就再也强调不出来了
      padding: const EdgeInsets.only(bottom: 24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (title != null)
            Row(
              children: [
                Expanded(
                  child: Text(title!, style: theme.textTheme.titleSmall),
                ),
                ?trailing,
              ],
            )
          else if (trailing case final t?)
            Row(
              children: [
                if (desc case final d?) Expanded(child: d) else const Spacer(),
                t,
              ],
            )
          else
            ?desc,
          if (title != null && desc != null) ...[
            const SizedBox(height: 4),
            desc,
          ],
          const SizedBox(height: 10),
          ...children,
        ],
      ),
    );
  }
}

/// 整页级的说明/空态/错误态 —— 设置各页共用这一个。
///
/// # 为什么值得一个公共件
///
/// 此前是四套手写：技能页的 `_Note`、电脑操作的 `_Unavailable`、连接器页
/// 的 `_NoLocalAgent` 与 `_Failed` —— 图标从 30 到 36、限宽从 380 到不限、
/// 重试按钮一个 OutlinedButton 一个 TextButton。同类东西四个长相，
/// 逐页切换时最容易被看出来是拼的；`_Failed` 最伤：一段红色裸异常文本
/// 居中挂着，读起来像程序崩了而不是一个设计过的「拉不到，重试」。
class SettingsPageNote extends StatelessWidget {
  const SettingsPageNote({
    super.key,
    required this.icon,
    required this.title,
    required this.body,
    this.action,
  });

  final IconData icon;
  final String title;
  final String body;

  /// 出路（重试之类）。错误态**必须**给一个 —— 没有出路的错误是死胡同。
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 420),
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 48),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(icon, size: 32, color: theme.cortex.foregroundTertiary),
              const SizedBox(height: 12),
              Text(title, style: theme.textTheme.titleSmall),
              const SizedBox(height: 6),
              Text(
                body,
                textAlign: TextAlign.center,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                  height: 1.6,
                ),
              ),
              if (action != null) ...[const SizedBox(height: 14), action!],
            ],
          ),
        ),
      ),
    );
  }
}

/// 成组内容的那张卡。
///
/// 表面用 `surfaceContainer`（规范第二节的 card 那一档），圆角用
/// [CortexTokens.radiusXl]，边框用**带透明度**的 `outlineVariant` ——
/// 实色边框只在它被调出来的那一个表面上好看。
class SettingsCard extends StatelessWidget {
  const SettingsCard({
    super.key,
    required this.child,
    this.padding = const EdgeInsets.all(14),
  });

  final Widget child;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: padding,
      decoration: BoxDecoration(
        color: scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(CortexTokens.radiusXl),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: child,
    );
  }
}

/// 卡片里的一行：左边标签、右边值/控件。
///
/// **键用第三级前景，值用正文色**（规范第十节）。两者同色时眼睛要逐行
/// 分辨哪边是标签哪边是内容 —— 而那正是三级前景存在的理由。
class SettingsRow extends StatelessWidget {
  const SettingsRow({
    super.key,
    required this.label,
    this.value,
    this.note,
    this.trailing,
    this.monospace = false,
  });

  final String label;
  final String? value;

  /// 值下面那行小字（为什么、以及有什么后果）。
  final String? note;

  /// 值的位置上放一个控件，而不是一段文字。与 [value] 二选一。
  final Widget? trailing;

  /// 地址、端口、sha 这类**要逐字符比对**的值用等宽 ——
  /// 比例字体下 `l` 与 `1`、`0` 与 `O` 分不开，而这一页上它们正好都出现。
  final bool monospace;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 5),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 96,
            child: Padding(
              // 与右边第一行文字的基线对齐：labelSmall 比 bodySmall 矮，
              // 顶对齐会让标签看起来浮在值的上面半格
              padding: const EdgeInsets.only(top: 1),
              child: Text(
                label,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                ),
              ),
            ),
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (trailing != null)
                  trailing!
                else
                  SelectableText(
                    value ?? '—',
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.onSurface,
                      fontFamily: monospace ? 'monospace' : null,
                    ),
                  ),
                if (note != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 2),
                    child: Text(
                      note!,
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: theme.cortex.foregroundTertiary,
                      ),
                    ),
                  ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// 一行表单：**左边标签 + 说明，右边控件**。
///
/// # 为什么不是「标签在上、控件在下」
///
/// 上下堆的版式里，每一项都要占两到三行，而这一页上「API 密钥」「API 地址」
/// 「连通性检查」三项加起来会推到一屏之外 —— 于是想核对一眼端点的人得先滚。
/// 而端点恰恰是最常需要核对的那一项。
///
/// 左右分栏还顺带解决了另一件事：**说明有地方放了**。上下堆时说明只能
/// 挤在标签与输入框之间，读起来像输入框的一部分。
class SettingsField extends StatelessWidget {
  const SettingsField({
    super.key,
    required this.label,
    this.hint,
    required this.child,
  });

  final String label;

  /// 标签下面那行小字：格式要求、留空会怎样、以及**后果**。
  final String? hint;

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 9),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 160,
            child: Padding(
              // 与右边控件的第一行对齐：输入框自带内边距，标签顶对齐会
              // 显得浮在上面半格
              padding: const EdgeInsets.only(top: 7),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    label,
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.onSurface,
                    ),
                  ),
                  if (hint != null)
                    Padding(
                      padding: const EdgeInsets.only(top: 2),
                      child: Text(
                        hint!,
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: theme.cortex.foregroundTertiary,
                          height: 1.4,
                        ),
                      ),
                    ),
                ],
              ),
            ),
          ),
          const SizedBox(width: 16),
          Expanded(child: child),
        ],
      ),
    );
  }
}

/// 一组互斥选项，横着摆。
///
/// # 为什么不是下拉框
///
/// 这里的选项都只有两三个，而**每一档都需要一句解释**（「跟随系统」跟的是
/// 哪个系统开关？「关闭」关掉的是什么？）。下拉框把选项藏进一次点击后面，
/// 说明就没地方放了 —— 用户只能靠档位名去猜，而档位名恰恰是最省字的地方。
///
/// 选中态用**中性的** [CortexTokens.sidebarAccent] 而不是品牌色：
/// 「我现在选的是哪一档」是位置，不是动作（规范第一节）。
class SettingsChoice<T> extends StatelessWidget {
  const SettingsChoice({
    super.key,
    required this.value,
    required this.options,
    required this.onChanged,
  });

  final T value;
  final List<({T value, String label, String hint})> options;
  final ValueChanged<T> onChanged;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    // ⚠️ **`stretch` 必须裹一层 `IntrinsicHeight`。**
    //
    // 想要的是三块等高（某一块的说明换行时另外两块跟着长，否则一排卡片
    // 参差不齐）。但 `stretch` 是「照着 Row 自己的高度撑满」，而这一行
    // 待在 `ListView` → `Column` 里，竖直方向**无界** —— 于是它把
    // `h=Infinity` 传给子节点，抛 `BoxConstraints forces an infinite height`。
    //
    // 症状不是报错，是**整页一片空白**：外面那层 `ListView` 把异常
    // 吞在渲染阶段，标题照画，内容什么都没有。2026-08-21 就这么发出去过
    // 一次，`flutter analyze` 与 510 条测试全绿 —— 因为当时没有任何一条
    // 测试真的把这一页画出来过（现在有了，见 settings_pages_test.dart）。
    //
    // `IntrinsicHeight` 先量出「最高的那块有多高」，把无界变成有界，
    // 之后 `stretch` 才有东西可撑。三个子节点，这点代价可以忽略。
    return IntrinsicHeight(
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          for (final o in options) ...[
            Expanded(
              child: Semantics(
                selected: o.value == value,
                button: true,
                child: InkWell(
                  onTap: () => onChanged(o.value),
                  borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
                  child: Container(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 10,
                      vertical: 8,
                    ),
                    decoration: BoxDecoration(
                      color: o.value == value
                          ? theme.cortex.sidebarAccent
                          : Colors.transparent,
                      borderRadius: BorderRadius.circular(
                        CortexTokens.radiusMd,
                      ),
                      border: Border.all(
                        color: o.value == value
                            ? scheme.outline
                            : scheme.outlineVariant,
                      ),
                    ),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          o.label,
                          style: theme.textTheme.bodySmall?.copyWith(
                            color: scheme.onSurface,
                            fontWeight: o.value == value
                                ? FontWeight.w600
                                : FontWeight.w400,
                          ),
                        ),
                        const SizedBox(height: 2),
                        Text(
                          o.hint,
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: theme.cortex.foregroundTertiary,
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
            if (o != options.last) const SizedBox(width: 8),
          ],
        ],
      ),
    );
  }
}

/// 一条提示条（说明、警告、「这个部署没有这个功能」）。
///
/// [tone] 只在**真的需要传达状态**时给颜色，默认是中性的（规范第一节）。
class SettingsNote extends StatelessWidget {
  const SettingsNote({
    super.key,
    required this.child,
    this.icon = Icons.info_outline_rounded,
    this.tone,
  });

  final Widget child;
  final IconData icon;
  final Color? tone;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final color = tone ?? scheme.onSurfaceVariant;
    return Container(
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(icon, size: 15, color: color),
          const SizedBox(width: 8),
          Expanded(child: child),
        ],
      ),
    );
  }
}
