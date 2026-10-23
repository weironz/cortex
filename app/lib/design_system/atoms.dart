/// 原子件：**微边框面板、悬停按钮、可按下的一行、状态胶囊**。
///
/// # 为什么要有这一层
///
/// 2026-10-23 清点过：`BoxDecoration` 92 处、`Border.all(...)` 32 处、
/// `BorderSide(...)` 16 处。它们绝大多数在重复同一件事 —— 一块
/// `surfaceContainer` 底 + 一条 `outlineVariant` 发丝线 + 一档圆角。
/// 重复本身不是问题，问题是**每一处都可以悄悄偏一点**：某处用了
/// `surfaceContainerHigh`、某处圆角写了 10、某处忘了边框。单看每一处都
/// 说得过去，合起来就是「这个界面有点说不清哪里不齐」。
///
/// 所以这里把四类东西收成唯一落点：
///
/// 1. [CortexSurface] —— 任何「有底色、有边、盛着内容」的面。
/// 2. [CortexHover] —— 「鼠标进来要变一点」的那个判断（含键盘焦点）。
/// 3. [CortexIconButton] / [CortexPill] —— 两种最小交互单元。
/// 4. [CortexHairline] —— 一条 1px 的分隔线。
///
/// # 为什么不直接包一层 Material
///
/// `Card` / `Material` 自带 elevation 语义，而这一套设计的层级**不用
/// elevation 表达**（见 docs/design.md 第二节）。用它们就得在每个调用点
/// 写 `elevation: 0`，而那正是「每处都能悄悄偏一点」的老问题。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../core/theme.dart';
import 'tokens.dart';

/// 一块有底色、有发丝线、有圆角的面。
///
/// # 三种材质，按用途选
///
/// * [CortexSurfaceTone.canvas] —— 与底板同色，**只靠边框**划出范围。
///   用在「不该比底板更显眼」的容器上（工具调用面板）。
/// * [CortexSurfaceTone.panel] —— `surfaceContainer`，比底板亮一档。
///   最常用的一档：卡片、输入框外壳、浮条。
/// * [CortexSurfaceTone.sunken] —— `surfaceContainerLow`，比底板**暗**
///   一档。用在「嵌进去」的东西上（代码块、日志输出）。
///
/// # 悬停与焦点
///
/// 给了 [onTap] 就自动带悬停/焦点反馈：边框平滑提亮到
/// `onSurface` 的 18%，底色提亮 [CortexSpace.hair] 那一档。
/// 手写 `MouseRegion` + `setState` 也能做到，但这个产品里有二十多处
/// 这么写的地方，每一处都在自己决定「提亮多少」。
class CortexSurface extends StatefulWidget {
  const CortexSurface({
    super.key,
    required this.child,
    this.tone = CortexSurfaceTone.panel,
    this.radius = CortexRadius.card,
    this.padding,
    this.margin,
    this.onTap,
    this.selected = false,
    this.floating = false,
    this.focused = false,
    this.borderColor,
    this.backgroundColor,
    this.width,
    this.height,
  });

  final Widget child;
  final CortexSurfaceTone tone;
  final double radius;
  final EdgeInsetsGeometry? padding;
  final EdgeInsetsGeometry? margin;

  /// 可点。给了它才有悬停/焦点反馈与手型指针。
  final VoidCallback? onTap;

  /// 选中态。**中性**的一档提亮，不是品牌色 —— 「我现在在这一项上」
  /// 是位置，不是动作（见 docs/design.md 第九节）。
  final bool selected;

  /// 浮起来：加一档投影。只有菜单、弹层、浮条该用。
  final bool floating;

  /// 外部强制焦点态（例如输入框外壳跟着内部 `TextField` 的焦点走）。
  final bool focused;

  final Color? borderColor;
  final Color? backgroundColor;
  final double? width;
  final double? height;

  @override
  State<CortexSurface> createState() => _CortexSurfaceState();
}

enum CortexSurfaceTone { canvas, panel, sunken }

class _CortexSurfaceState extends State<CortexSurface> {
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final tokens = theme.cortex;

    final interactive = widget.onTap != null;

    Widget paint(bool hot) {
      final lit = hot || widget.focused;

      final Color base = switch (widget.tone) {
        CortexSurfaceTone.canvas => scheme.surface,
        CortexSurfaceTone.panel => scheme.surfaceContainer,
        CortexSurfaceTone.sunken => scheme.surfaceContainerLow,
      };

      // 提亮一档的垫色。选中与悬停都走它 —— 选中比悬停实一点，
      // 但两者都是**中性**的：品牌色只留给动作。
      final Color background =
          widget.backgroundColor ??
          (widget.selected
              ? tokens.sidebarAccent
              : hot
              ? Color.alphaBlend(tokens.hover, base)
              : base);

      // 边框：常态发丝线 → 悬停/焦点提亮。用实色而不是透明度，
      // 与主题里那条一致（透明度线在每个表面上深浅不一）。
      final Color border =
          widget.borderColor ??
          (lit
              ? scheme.onSurface.withValues(alpha: 0.18)
              : scheme.outlineVariant);

      return AnimatedContainer(
        duration: CortexMotion.resolve(context, CortexMotion.fast),
        curve: CortexMotion.enter,
        width: widget.width,
        height: widget.height,
        margin: widget.margin,
        padding: widget.padding,
        decoration: BoxDecoration(
          color: background,
          borderRadius: BorderRadius.circular(widget.radius),
          border: CortexStroke.all(border),
          boxShadow: widget.floating ? tokens.shadow3 : null,
        ),
        child: widget.child,
      );
    }

    if (!interactive) return paint(false);

    return CortexHover(onTap: widget.onTap, builder: (_, hot) => paint(hot));
  }
}

/// 「鼠标进来 / 键盘聚焦」那一位，交给 [builder] 自己画。
///
/// # 为什么是 builder 而不是一个现成的容器
///
/// 有些地方要变的不是底色：侧栏行要渐变底色、工具行要提亮边框、
/// 输入框外壳要换焦点环。把「怎么变」写死在这个部件里，就又要为每一种
/// 变体各开一个参数。builder 只回答「此刻热不热」。
///
/// # 键盘可达性
///
/// 悬停走自己的 `MouseRegion`，聚焦走 `Focus` + `Shortcuts` —— **不用
/// `FocusableActionDetector`**。它把悬停高亮挂在
/// `FocusManager.highlightMode` 上：那个开关的意思是「此刻是不是键盘在
/// 操作」，而鼠标悬停与它是两件事。实测（2026-10-23）在
/// `highlightMode == touch` 时它一次都不回调，于是整屏的悬停反馈在
/// 某些输入设备下静默失效 —— 而那种失效没有任何报错。
class CortexHover extends StatefulWidget {
  const CortexHover({
    super.key,
    required this.builder,
    this.onTap,
    this.cursor = SystemMouseCursors.click,
    this.autofocus = false,
    this.focusNode,
    this.onFocusChange,
  });

  /// `hot` = 悬停或聚焦。绝大多数地方两者应当一样。
  final Widget Function(BuildContext context, bool hot) builder;

  final VoidCallback? onTap;
  final MouseCursor cursor;
  final bool autofocus;
  final FocusNode? focusNode;
  final ValueChanged<bool>? onFocusChange;

  @override
  State<CortexHover> createState() => _CortexHoverState();
}

class _CortexHoverState extends State<CortexHover> {
  bool _hovered = false;
  bool _focused = false;

  void _setFocus(bool v) {
    if (_focused == v) return;
    setState(() => _focused = v);
    widget.onFocusChange?.call(v);
  }

  @override
  Widget build(BuildContext context) {
    final enabled = widget.onTap != null;

    Widget child = MouseRegion(
      cursor: enabled ? widget.cursor : SystemMouseCursors.basic,
      onEnter: (_) {
        if (_hovered) return;
        setState(() => _hovered = true);
      },
      onExit: (_) {
        if (!_hovered) return;
        setState(() => _hovered = false);
      },
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: widget.onTap,
        child: widget.builder(context, _hovered || _focused),
      ),
    );

    if (!enabled) return child;

    return Focus(
      autofocus: widget.autofocus,
      focusNode: widget.focusNode,
      onFocusChange: _setFocus,
      child: Shortcuts(
        shortcuts: const {
          SingleActivator(LogicalKeyboardKey.enter): ActivateIntent(),
          SingleActivator(LogicalKeyboardKey.space): ActivateIntent(),
        },
        child: Actions(
          actions: {
            // Enter / Space 触发 —— 一个能 Tab 到却按不动的行，
            // 比 Tab 不到更让人困惑
            ActivateIntent: CallbackAction<ActivateIntent>(
              onInvoke: (_) {
                widget.onTap?.call();
                return null;
              },
            ),
          },
          child: child,
        ),
      ),
    );
  }
}

/// 一个只有图标的小按钮。**悬停才显形**的那一类也用它。
///
/// 与 `IconButton` 的差别：
///
/// * 没有 M3 的水波纹（`InkSparkle` 在深色下是一团亮斑，与「克制」相反）；
/// * 悬停垫色走 [CortexTokens.hover]，与侧栏行同一档；
/// * 尺寸是 26×26 的**桌面**尺寸 —— M3 的 48 是触屏尺寸，
///   一排 48 的图标在侧栏里会撑出两倍高度。
class CortexIconButton extends StatefulWidget {
  const CortexIconButton({
    super.key,
    required this.icon,
    required this.tooltip,
    this.onPressed,
    this.size = 15,
    this.box = 26,
    this.color,
    this.selected = false,
  });

  final IconData icon;
  final String tooltip;

  /// `null` = 停用。**停用而不是消失**：按钮忽隐忽现会让这一行的宽度
  /// 在每一轮开始和结束时跳一下。
  final VoidCallback? onPressed;

  final double size;
  final double box;
  final Color? color;
  final bool selected;

  @override
  State<CortexIconButton> createState() => _CortexIconButtonState();
}

class _CortexIconButtonState extends State<CortexIconButton> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final tokens = theme.cortex;
    final enabled = widget.onPressed != null;

    final Color fg =
        widget.color ??
        (enabled ? scheme.onSurfaceVariant : tokens.foregroundTertiary);

    return Tooltip(
      message: widget.tooltip,
      waitDuration: const Duration(milliseconds: 500),
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onPressed,
          child: AnimatedContainer(
            duration: CortexMotion.resolve(context, CortexMotion.instant),
            width: widget.box,
            height: widget.box,
            decoration: BoxDecoration(
              color: widget.selected
                  ? tokens.sidebarAccent
                  : _hovered && enabled
                  ? tokens.hover
                  : Colors.transparent,
              borderRadius: BorderRadius.circular(CortexRadius.row),
            ),
            child: Icon(widget.icon, size: widget.size, color: fg),
          ),
        ),
      ),
    );
  }
}

/// 一颗胶囊：状态、标签、计数。
///
/// 高度是**内容撑出来的**（`padding` 6/2 + 11px 字 ≈ 20），不写死 ——
/// 写死高度之后，换字体或换语言就会截字。
class CortexPill extends StatelessWidget {
  const CortexPill({
    super.key,
    required this.label,
    this.icon,
    this.leading,
    this.color,
    this.background,
    this.border = true,
    this.onTap,
  });

  final String label;
  final IconData? icon;

  /// 标签前面那颗点/图标（例如运行中的呼吸灯）。
  final Widget? leading;

  final Color? color;
  final Color? background;

  /// 要不要发丝线。挤在一起的一排小标签可以不要（线会让它们糊成一片）。
  final bool border;

  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final fg = color ?? scheme.onSurfaceVariant;

    final content = Container(
      padding: const EdgeInsets.symmetric(
        horizontal: CortexSpace.md,
        vertical: CortexSpace.xxs,
      ),
      decoration: BoxDecoration(
        color: background ?? scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(CortexRadius.pill),
        border: border ? Border.all(color: scheme.outlineVariant) : null,
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (leading != null) ...[
            leading!,
            const SizedBox(width: CortexSpace.sm),
          ],
          if (icon != null) ...[
            Icon(icon, size: 13, color: fg),
            const SizedBox(width: CortexSpace.sm),
          ],
          Text(
            label,
            style: theme.textTheme.labelSmall?.copyWith(
              color: fg,
              fontWeight: FontWeight.w500,
            ),
          ),
        ],
      ),
    );

    if (onTap == null) return content;
    return CortexHover(
      onTap: onTap,
      builder: (context, hot) => AnimatedOpacity(
        duration: CortexMotion.resolve(context, CortexMotion.instant),
        opacity: hot ? 1 : 0.88,
        child: content,
      ),
    );
  }
}

/// 一条 1px 的分隔线。
///
/// 比 `Divider` 少两件事：没有 8px 的默认 `space`（那会让「一条线」变成
/// 「一段空白」），也没有 `thickness` 与 `height` 两个要一起改的参数。
class CortexHairline extends StatelessWidget {
  const CortexHairline({super.key, this.vertical = false, this.indent = 0});

  final bool vertical;

  /// 两端缩进。竖直分隔线缩进之后读作「这两块属于同一组」。
  final double indent;

  @override
  Widget build(BuildContext context) {
    final color = Theme.of(context).colorScheme.outlineVariant;
    return vertical
        ? Padding(
            padding: EdgeInsets.symmetric(vertical: indent),
            child: VerticalDivider(width: 1, thickness: 1, color: color),
          )
        : Padding(
            padding: EdgeInsets.symmetric(horizontal: indent),
            child: Divider(height: 1, thickness: 1, color: color),
          );
  }
}
