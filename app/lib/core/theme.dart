import 'package:flutter/material.dart';

/// `ColorScheme` 装不下的那些角色。
///
/// # 为什么要多一个扩展，而不是把它们塞进 ColorScheme
///
/// M3 的 `ColorScheme` 是一份**固定的**角色表，里面没有「侧栏」这个表面，
/// 没有第三级前景，也只有 error 一种反馈色。硬塞的做法（比如拿
/// `tertiary` 当侧栏、拿 `secondary` 当 success）会让每一个读代码的人
/// 都要先知道一份不成文的对照表 —— 而那份表迟早会有人记错。
///
/// 角色的划分抄 Cherry Studio 的 `DESIGN.md`：
/// background / card / popover / **sidebar** 四种表面，
/// foreground / muted / **tertiary** 三级前景（都是实色，
/// 层级**不靠透明度也不靠更细的字重**），
/// 以及 success / warning / info 三种反馈。
@immutable
class CortexTokens extends ThemeExtension<CortexTokens> {
  const CortexTokens({
    required this.sidebar,
    required this.sidebarAccent,
    required this.sidebarBorder,
    required this.foregroundTertiary,
    required this.success,
    required this.warning,
    required this.info,
  });

  /// 导航区那一整块。**与内容区是两个表面** —— 这是 Cherry Studio
  /// 明确单列出来的一档，也是「一眼看出哪边是导航」最省力的做法：
  /// 不用画分隔线，也不用加阴影。
  final Color sidebar;

  /// 侧栏里**被选中 / 悬停**的那一项的底色。
  ///
  /// # 为什么不是品牌色的淡淡一层
  ///
  /// 此前是 `primary.withValues(alpha: .12)` —— 整条侧栏因此常年挂着一块
  /// 紫。按第一节那条（色彩只表达动作或含义），「当前在看哪个会话」是
  /// **位置**，不是动作；而一个永远在那儿的彩色块会把真正需要注意的东西
  /// 稀释掉。
  ///
  /// Cherry Studio 为这件事单列了 `--cs-sidebar-accent`，也是中性的一档。
  final Color sidebarAccent;

  /// 侧栏与内容区之间那条线。比通用 outlineVariant 实一点点 ——
  /// 它分的是两个**功能区**，不是两行内容。
  final Color sidebarBorder;

  /// 第三级前景：说明文字、时间戳、禁用态。
  ///
  /// **必须是实色。** 用 `onSurfaceVariant.withValues(alpha: .6)` 造出来的
  /// 第三级在不同表面上深浅不一，而且与「这个控件被禁用了」的半透明
  /// 撞在一起 —— 到那时就分不清「这是次要信息」还是「这个点不了」。
  final Color foregroundTertiary;

  /// 三种反馈色。**只在传达状态时用**，不当装饰。
  final Color success;
  final Color warning;
  final Color info;

  /// 没挂扩展时从 `ColorScheme` 现推一份。见 [CortexTokensX.cortex]。
  factory CortexTokens.fallbackFor(ColorScheme s) => CortexTokens(
    sidebar: s.surfaceContainerLow,
    sidebarAccent: s.surfaceContainerHigh,
    sidebarBorder: s.outlineVariant,
    foregroundTertiary: s.onSurfaceVariant,
    success: s.secondary,
    warning: s.tertiary,
    info: s.primary,
  );

  /// 圆角阶。按密度取，不要每处现编一个数。
  ///
  /// 数值来自 Cherry Studio 的 `radius.css`（它写成 rem，这里换算成逻辑
  /// 像素）。**紧凑控件用小的，容器用大的**，全圆只留给胶囊、头像和圆钮。
  static const double radiusSm = 6; // 徽标、小按钮
  static const double radiusMd = 8; // 输入框、菜单项
  static const double radiusLg = 10; // 主按钮、气泡
  static const double radiusXl = 14; // 卡片、面板
  static const double radius2xl = 18; // 对话框

  @override
  CortexTokens copyWith({
    Color? sidebar,
    Color? sidebarAccent,
    Color? sidebarBorder,
    Color? foregroundTertiary,
    Color? success,
    Color? warning,
    Color? info,
  }) => CortexTokens(
    sidebar: sidebar ?? this.sidebar,
    sidebarAccent: sidebarAccent ?? this.sidebarAccent,
    sidebarBorder: sidebarBorder ?? this.sidebarBorder,
    foregroundTertiary: foregroundTertiary ?? this.foregroundTertiary,
    success: success ?? this.success,
    warning: warning ?? this.warning,
    info: info ?? this.info,
  );

  @override
  CortexTokens lerp(covariant CortexTokens? other, double t) {
    if (other == null) return this;
    return CortexTokens(
      sidebar: Color.lerp(sidebar, other.sidebar, t)!,
      sidebarAccent: Color.lerp(sidebarAccent, other.sidebarAccent, t)!,
      sidebarBorder: Color.lerp(sidebarBorder, other.sidebarBorder, t)!,
      foregroundTertiary: Color.lerp(
        foregroundTertiary,
        other.foregroundTertiary,
        t,
      )!,
      success: Color.lerp(success, other.success, t)!,
      warning: Color.lerp(warning, other.warning, t)!,
      info: Color.lerp(info, other.info, t)!,
    );
  }
}

/// `Theme.of(context).cortex` —— 少写一遍那串泛型查找。
///
/// # 挂了扩展就用挂着那份，没挂就从 `ColorScheme` 现推一份
///
/// **不写成 `extension<CortexTokens>()!`。** 那个感叹号的代价是：任何一个
/// 把部件挂在裸 `MaterialApp` 下的 widget 测试都会当场
/// 「Null check operator used on a null value」炸掉，而报错指向的是那个
/// 部件，跟主题一点关系都看不出来。2026-08-21 就这么红过一次。
///
/// 推出来的那份**不追求好看**，只保证「在任何主题下都说得通」：
/// 侧栏取 `surfaceContainerLow`、第三级前景取 `onSurfaceVariant`。
/// 真正调过的那份在 [CortexTheme] 里，两者不会同时生效。
extension CortexTokensX on ThemeData {
  CortexTokens get cortex =>
      extension<CortexTokens>() ?? CortexTokens.fallbackFor(colorScheme);
}

/// Cortex design tokens.
///
/// # 中性优先，色彩只用来表达动作或含义
///
/// 刻意手写而不是 `ColorScheme.fromSeed`：生成的色调板会给**每一个表面**
/// 都染上一层可见的色偏，读起来像「一个 Flutter demo」。
///
/// 2026-08-21 按 Cherry Studio 与 LobeHub 的设计语言重做了一遍
/// （逐条对照见 [docs/design.md](../../../docs/design.md)）。三条最关键的：
///
/// 1. **灰是纯中性的**（色度为 0），不是「略偏冷」。偏冷的灰在与真正的
///    彩色并排时会读成「一个没调准的颜色」，而不是「无色」。
/// 2. **边框用透明度，不用实色**。实色边框只在它被调出来的那一个表面上
///    好看；换到卡片、弹层、侧栏上就会偏亮或偏暗。
/// 3. **层级靠表面，不靠阴影**。ground → card → popover 逐层变亮
///    （深色下尤其重要：弹层在卡片**之上**，就该更亮）。
///
/// 品牌靛蓝**保留**。抄的是它们的体系（角色划分、表面分层、克制程度），
/// 不是它们的色相 —— 换成 Cherry 的品牌绿只会让 Cortex 看起来像 Cherry。
abstract final class CortexTheme {
  // 主强调色 —— 用户气泡、焦点、主要动作。整个产品里唯一的品牌色。
  static const _indigo = Color(0xFF5B62F4);
  static const _indigoLight = Color(0xFF7A80FF);

  /// 第二个语义强调色：**状态**（完成、激活、附件就位）。
  ///
  /// 它原来叫「记忆强调色」，理由是「让来源标注不与主要动作抢注意力」——
  /// 而长期记忆 2026-08-17 整条拆去了 Cormex，那个理由随之消失。
  /// 颜色没动（改它会牵动六处调用），但它现在表达的是状态而不是来源。
  static const _tealLight = Color(0xFF0E7C86);
  static const _tealDark = Color(0xFF4FD1C5);

  static ThemeData light() => _build(_lightScheme, Brightness.light);
  static ThemeData dark() => _build(_darkScheme, Brightness.dark);

  static const _lightScheme = ColorScheme(
    brightness: Brightness.light,
    primary: _indigo,
    onPrimary: Colors.white,
    primaryContainer: Color(0xFFE4E5FF),
    onPrimaryContainer: Color(0xFF1B1D66),
    secondary: _tealLight,
    onSecondary: Colors.white,
    secondaryContainer: Color(0xFFD3F1F3),
    onSecondaryContainer: Color(0xFF04353A),
    error: Color(0xFFB3261E),
    onError: Colors.white,
    errorContainer: Color(0xFFF9DEDC),
    onErrorContainer: Color(0xFF410E0B),
    // ── 中性阶：色度全为 0 ──
    //
    // 数值对齐 Cherry Studio 的 provider（它用 oklch 写，`oklch(0.556 0 0)`
    // 这一串就是 Tailwind neutral-500 = #737373）。**不要顺手把它们调"暖"
    // 或"冷"一点**：那正是这次要去掉的东西。
    surface: Colors.white,
    onSurface: Color(0xFF1C1C1C),
    onSurfaceVariant: Color(0xFF737373),
    // 边框用**透明度**：同一个值在白底、卡片、侧栏上都成立。
    // 实色边框只在被调出来的那一个表面上好看
    outline: Color(0x33000000),
    outlineVariant: Color(0x14000000),
    surfaceContainerLowest: Colors.white,
    surfaceContainerLow: Color(0xFFFAFAFA),
    surfaceContainer: Color(0xFFF5F5F5),
    surfaceContainerHigh: Color(0xFFF0F0F0),
    surfaceContainerHighest: Color(0xFFE9E9E9),
    inverseSurface: Color(0xFF262626),
    onInverseSurface: Color(0xFFFAFAFA),
  );

  static const _darkScheme = ColorScheme(
    brightness: Brightness.dark,
    primary: _indigoLight,
    onPrimary: Color(0xFF10123D),
    primaryContainer: Color(0xFF32357F),
    onPrimaryContainer: Color(0xFFE0E1FF),
    secondary: _tealDark,
    onSecondary: Color(0xFF00312F),
    secondaryContainer: Color(0xFF0C4A4C),
    onSecondaryContainer: Color(0xFFB8F0EC),
    error: Color(0xFFF2B8B5),
    onError: Color(0xFF601410),
    errorContainer: Color(0xFF8C1D18),
    onErrorContainer: Color(0xFFF9DEDC),
    // ── 深色下**层越高越亮** ──
    //
    // Cherry Studio 在它的 provider 里专门为这一条写了注释：弹层坐在卡片
    // **之上**，所以它该更亮，而不是更暗。反过来做（弹层更深）会让浮起来的
    // 东西看着陷进去，而那是浅色模式的直觉被原样搬过来的结果。
    surface: Color(0xFF0F0F0F),
    onSurface: Color(0xFFEDEDED),
    onSurfaceVariant: Color(0xFFA1A1A1),
    outline: Color(0x33FFFFFF),
    outlineVariant: Color(0x14FFFFFF),
    surfaceContainerLowest: Color(0xFF0A0A0A),
    surfaceContainerLow: Color(0xFF141414),
    surfaceContainer: Color(0xFF171717),
    surfaceContainerHigh: Color(0xFF1F1F1F),
    surfaceContainerHighest: Color(0xFF262626),
    inverseSurface: Color(0xFFEDEDED),
    onInverseSurface: Color(0xFF171717),
  );

  static ThemeData _build(ColorScheme scheme, Brightness brightness) {
    final base = ThemeData(
      useMaterial3: true,
      colorScheme: scheme,
      brightness: brightness,
    );

    final dark = brightness == Brightness.dark;
    return base.copyWith(
      scaffoldBackgroundColor: scheme.surface,
      extensions: [
        CortexTokens(
          // 侧栏比内容区**暗一档**（深色下则是亮一档）。抄 Cherry Studio：
          // 它的 `--cs-sidebar` 在浅色是 0.9672、深色是 0.2393，
          // 两边都与 background 差出肉眼分得清的一档
          sidebar: dark ? const Color(0xFF171717) : const Color(0xFFF4F4F4),
          // 比 sidebar 再走一档。差得太小选中态就看不出来，
          // 差得太大它会读成「另一个区域」而不是「这一项」
          sidebarAccent: dark
              ? const Color(0xFF2A2A2A)
              : const Color(0xFFE4E4E4),
          sidebarBorder: dark
              ? const Color(0xFF262626)
              : const Color(0xFFE5E5E5),
          foregroundTertiary: dark
              ? const Color(0xFF737373)
              : const Color(0xFF909090),
          // 反馈三色：深色下取更亮的一档，否则在深底上读不出来
          success: dark ? const Color(0xFF4ADE80) : const Color(0xFF16A34A),
          warning: dark ? const Color(0xFFFBBF24) : const Color(0xFFD97706),
          info: dark ? const Color(0xFF60A5FA) : const Color(0xFF2563EB),
        ),
      ],
      dividerTheme: DividerThemeData(
        color: scheme.outlineVariant,
        thickness: 1,
        space: 1,
      ),
      textTheme: _textTheme(base.textTheme, scheme),
      inputDecorationTheme: InputDecorationTheme(
        filled: true,
        fillColor: scheme.surfaceContainerHigh,
        hintStyle: TextStyle(color: scheme.onSurfaceVariant),
        contentPadding: const EdgeInsets.symmetric(
          horizontal: 14,
          vertical: 12,
        ),
        border: _inputBorder(scheme.outlineVariant),
        enabledBorder: _inputBorder(Colors.transparent),
        focusedBorder: _inputBorder(scheme.primary, width: 1.5),
        isDense: true,
      ),
      filledButtonTheme: FilledButtonThemeData(
        style: FilledButton.styleFrom(
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
          ),
          padding: const EdgeInsets.symmetric(horizontal: 18, vertical: 14),
          textStyle: const TextStyle(fontWeight: FontWeight.w600),
        ),
      ),
      iconButtonTheme: IconButtonThemeData(
        style: IconButton.styleFrom(
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
          ),
        ),
      ),
      tooltipTheme: TooltipThemeData(
        waitDuration: const Duration(milliseconds: 500),
        decoration: BoxDecoration(
          color: scheme.inverseSurface,
          borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        ),
        textStyle: TextStyle(color: scheme.onInverseSurface, fontSize: 12),
      ),
      chipTheme: ChipThemeData(
        side: BorderSide(color: scheme.outlineVariant),
        backgroundColor: scheme.surfaceContainer,
        labelStyle: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant),
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 0),
      ),
      // 对话框那一阶。**在此之前 `radius2xl` 用量是 0** —— 规范第四节写着
      // 「对话框 18」，却没有任何一处代码是那句话的落点，于是所有对话框
      // 一直用的是 Material 3 的默认 28，比规范圆了一半还多。
      //
      // 放在主题里而不是每个 `Dialog` 上：这个产品有七八个对话框
      // （设置、导入、权限、会话改动…），逐个设的下场就是这次收拾的
      // 那 42 处 —— 迟早有人新开一个而忘了设。
      dialogTheme: DialogThemeData(
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(CortexTokens.radius2xl),
        ),
      ),
      scrollbarTheme: ScrollbarThemeData(
        thickness: const WidgetStatePropertyAll(8),
        // 半宽 = 胶囊，**不走圆角那五阶**。跟着 thickness 走：
        // 改粗细时这个数要一起改，取一个阶常量反而会脱钩
        radius: const Radius.circular(4),
        thumbColor: WidgetStatePropertyAll(
          scheme.onSurfaceVariant.withValues(alpha: 0.35),
        ),
      ),
      splashFactory: InkSparkle.splashFactory,
    );
  }

  static OutlineInputBorder _inputBorder(Color color, {double width = 1}) =>
      OutlineInputBorder(
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        borderSide: BorderSide(color: color, width: width),
      );

  static TextTheme _textTheme(TextTheme base, ColorScheme scheme) => base
      .copyWith(
        titleLarge: base.titleLarge?.copyWith(
          fontSize: 17,
          fontWeight: FontWeight.w600,
          letterSpacing: -0.2,
        ),
        titleMedium: base.titleMedium?.copyWith(
          fontSize: 14.5,
          fontWeight: FontWeight.w600,
        ),
        titleSmall: base.titleSmall?.copyWith(
          fontSize: 12,
          fontWeight: FontWeight.w600,
          letterSpacing: 0.4,
        ),
        bodyLarge: base.bodyLarge?.copyWith(fontSize: 15, height: 1.62),
        bodyMedium: base.bodyMedium?.copyWith(fontSize: 14, height: 1.6),
        bodySmall: base.bodySmall?.copyWith(
          fontSize: 12.5,
          height: 1.5,
          color: scheme.onSurfaceVariant,
        ),
        labelSmall: base.labelSmall?.copyWith(
          fontSize: 11,
          letterSpacing: 0.3,
          color: scheme.onSurfaceVariant,
        ),
      )
      .apply(fontFamilyFallback: _cjkFallback);

  /// Flutter's default `Roboto` has no CJK coverage; without an explicit
  /// fallback chain, Chinese renders as tofu on Windows in some locales and
  /// picks an inconsistent face on Web.
  static const _cjkFallback = <String>[
    'Microsoft YaHei UI',
    'Microsoft YaHei',
    'PingFang SC',
    'Hiragino Sans GB',
    'Noto Sans CJK SC',
    'Source Han Sans SC',
    'sans-serif',
  ];

  /// Monospace stack for code blocks and ids.
  static const monoFallback = <String>[
    'JetBrains Mono',
    'Cascadia Code',
    'Consolas',
    'SF Mono',
    'Menlo',
    'DejaVu Sans Mono',
    'monospace',
  ];
}
