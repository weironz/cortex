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
    required this.hover,
    required this.accentSoft,
    required this.accentInk,
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
  ///
  /// ⚠️ [warning]（琥珀）从 2026-08-24 起兼任**第四状态** awaiting_confirm
  /// 的颜色：它是唯一一个「不处理就永远卡住」的状态，却曾与 running 共用
  /// 蓝色 —— 人扫一眼分不出「在跑」和「在等我」。三处落点（左栏状态点、
  /// 顶栏 pill、托盘图标）必须都取这一个值，各配各的迟早漂。
  final Color success;
  final Color warning;
  final Color info;

  /// 悬停垫色。**半透明**，与 [sidebarAccent]（实色、表达选中）分开 ——
  /// 悬停要能叠在任何表面上，选中只出现在侧栏那一种表面上。
  final Color hover;

  /// 品牌色的**垫底**版本：选中态背景、pill 底。半透明，
  /// 叠在哪个表面上都成立（设计稿 `--accSoft`）。
  final Color accentSoft;

  /// 品牌色的**文字**版本：链接、彩色数字。比主色深/亮一档 ——
  /// 主色是为按钮底调的，直接当文字用对比度不够（设计稿 `--accInk`）。
  final Color accentInk;

  /// 没挂扩展时从 `ColorScheme` 现推一份。见 [CortexTokensX.cortex]。
  factory CortexTokens.fallbackFor(ColorScheme s) => CortexTokens(
    sidebar: s.surfaceContainerLow,
    sidebarAccent: s.surfaceContainerHigh,
    sidebarBorder: s.outlineVariant,
    foregroundTertiary: s.onSurfaceVariant,
    success: s.secondary,
    warning: s.tertiary,
    info: s.primary,
    hover: s.onSurface.withValues(alpha: 0.045),
    accentSoft: s.primary.withValues(alpha: 0.10),
    accentInk: s.primary,
  );

  /// 圆角阶 —— 2026-08-24 起对齐设计稿的五档体系。
  ///
  /// 主干五档：**窗口 14 / 卡片 13 / 输入框 18 / 行 9 / 胶囊 999**。
  /// 成体系的意义是不再逐个组件拍脑袋 —— 每一处圆角都答得出
  /// 「它是五档里的哪一档」。小构件（徽标 7、气泡 11）是主干外的两个
  /// 辅助档，设计稿里各出现 20/18 次，不是漏网之鱼。
  ///
  /// 旧的 Sm/Md/Lg/Xl/2xl 命名保留（消费面 40+ 处），**值重定**到新体系：
  /// 改名的收益是零，改值让所有旧调用点一次到位。
  static const double radiusSm = 7; // 徽标、小按钮（辅助档）
  static const double radiusMd = 9; // 行、菜单项（=radiusRow）
  static const double radiusLg = 11; // 气泡、主按钮（辅助档）
  static const double radiusXl = 13; // 卡片、面板（=radiusCard）
  static const double radius2xl = 14; // 对话框 = 浮起的窗口（=radiusWindow）

  /// 语义别名 —— 新代码写这几个，读的人不用背数值表。
  static const double radiusWindow = 14;
  static const double radiusCard = 13;
  static const double radiusInput = 18; // 输入框刻意比对话框还圆：胶囊感
  static const double radiusRow = 9;
  static const double radiusPill = 999;

  @override
  CortexTokens copyWith({
    Color? sidebar,
    Color? sidebarAccent,
    Color? sidebarBorder,
    Color? foregroundTertiary,
    Color? success,
    Color? warning,
    Color? info,
    Color? hover,
    Color? accentSoft,
    Color? accentInk,
  }) => CortexTokens(
    sidebar: sidebar ?? this.sidebar,
    sidebarAccent: sidebarAccent ?? this.sidebarAccent,
    sidebarBorder: sidebarBorder ?? this.sidebarBorder,
    foregroundTertiary: foregroundTertiary ?? this.foregroundTertiary,
    success: success ?? this.success,
    warning: warning ?? this.warning,
    info: info ?? this.info,
    hover: hover ?? this.hover,
    accentSoft: accentSoft ?? this.accentSoft,
    accentInk: accentInk ?? this.accentInk,
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
      hover: Color.lerp(hover, other.hover, t)!,
      accentSoft: Color.lerp(accentSoft, other.accentSoft, t)!,
      accentInk: Color.lerp(accentInk, other.accentInk, t)!,
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
/// 2026-08-24 对齐设计稿（`docs/design/cortex-ui-design.html`，13 屏双主题）
/// 重做了一遍，数值**逐个照抄**它的 CSS 变量，不再自己调：
///
/// 1. **中性阶从纯灰换成冷调**（fg `#17171A`、sb `#EFEEF3` 都带一丝蓝紫）。
///    上一版坚持色度为 0，理由是「偏色的灰像没调准」—— 设计稿的答案是
///    让整套灰**往品牌靛蓝的方向**统一偏，偏得成体系就不是没调准，
///    而且与 macOS 的 sidebar/content 分层观感一致。
/// 2. **边框仍用透明度**：浅色 7% 黑、深色 9% 白，同一个值在每个表面上
///    都成立。强分隔（功能区之间）浅 11% / 深 15%。
/// 3. **层级靠表面**：深色下 win `#1B1B1F` → card `#26262C` → fill
///    `#2C2C33` 逐层变亮 —— 浮起来的东西更亮，不是更暗。
///
/// 品牌靛蓝保留（浅 `#5B62F4` / 深 `#7C82FF`）。渐变**只留发送键一处**：
/// 渐变一多，「主要动作」就不再突出。
abstract final class CortexTheme {
  // 主强调色 —— 用户气泡、焦点、主要动作。整个产品里唯一的品牌色。
  static const _indigo = Color(0xFF5B62F4);
  static const _indigoLight = Color(0xFF7C82FF);

  /// 第二个语义强调色：**状态**（完成、激活、附件就位）。
  ///
  /// 它原来叫「记忆强调色」，理由是「让来源标注不与主要动作抢注意力」——
  /// 而长期记忆 2026-08-17 整条拆去了 Cormex，那个理由随之消失。
  /// 颜色没动（改它会牵动六处调用），但它现在表达的是状态而不是来源。
  static const _tealLight = Color(0xFF0E7C86);
  static const _tealDark = Color(0xFF4FD1C5);

  /// [compact] 是桌面端的「紧凑」密度档：行高收 −6px（VisualDensity
  /// vertical −1.5，Material 一档 4px）、正文 14px/1.66。
  /// **默认档一个数都不动** —— 长回答需要那个行距。
  static ThemeData light({bool compact = false}) =>
      _build(_lightScheme, Brightness.light, compact: compact);
  static ThemeData dark({bool compact = false}) =>
      _build(_darkScheme, Brightness.dark, compact: compact);

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
    // 失败红加深到 #BC2E38：原值在 fill 上只有 4.1:1，
    // 比旁边的绿弱一档 —— 失败恰恰是最不能弱的那个
    error: Color(0xFFBC2E38),
    onError: Colors.white,
    errorContainer: Color(0xFFF9DEDC),
    onErrorContainer: Color(0xFF410E0B),
    // ── 中性阶：冷调（设计稿 LIGHT）──
    //
    // fg #17171A / fg2 #5E5E68 / fg3 见 CortexTokens.foregroundTertiary。
    // 整套灰往品牌靛蓝的方向统一偏 —— 偏得成体系就不是「没调准」
    surface: Colors.white,
    onSurface: Color(0xFF17171A),
    onSurfaceVariant: Color(0xFF5E5E68),
    // 边框用**透明度**：sep 7% 黑、sepStrong 11%。
    // 同一个值在白底、卡片、侧栏上都成立
    outline: Color(0x1C17171A),
    outlineVariant: Color(0x1217171A),
    // fill 阶：#F4F4F7（输入框、chip 底）→ #EDEDF1（按下、第二档）
    surfaceContainerLowest: Colors.white,
    surfaceContainerLow: Color(0xFFF9F9FB),
    surfaceContainer: Color(0xFFF6F6F9),
    surfaceContainerHigh: Color(0xFFF4F4F7),
    surfaceContainerHighest: Color(0xFFEDEDF1),
    inverseSurface: Color(0xFF26262C),
    onInverseSurface: Color(0xFFF9F9FB),
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
    error: Color(0xFFF87171),
    onError: Color(0xFF3A0A0D),
    errorContainer: Color(0xFF8C1D18),
    onErrorContainer: Color(0xFFF9DEDC),
    // ── 深色阶照抄设计稿 DARK，层越高越亮 ──
    //
    // win #1B1B1F → card #26262C → fill #2C2C33 → fill2 #33333B。
    // 浮起来的东西更亮，不是更暗 —— 反过来做会让弹层看着陷进去
    surface: Color(0xFF1B1B1F),
    onSurface: Color(0xFFF2F2F5),
    onSurfaceVariant: Color(0xFFB4B4BE),
    // sep 9% 白 / sepStrong 15%
    outline: Color(0x26FFFFFF),
    outlineVariant: Color(0x17FFFFFF),
    surfaceContainerLowest: Color(0xFF17171B),
    surfaceContainerLow: Color(0xFF232328),
    surfaceContainer: Color(0xFF26262C),
    surfaceContainerHigh: Color(0xFF2C2C33),
    surfaceContainerHighest: Color(0xFF33333B),
    inverseSurface: Color(0xFFF2F2F5),
    onInverseSurface: Color(0xFF26262C),
  );

  static ThemeData _build(
    ColorScheme scheme,
    Brightness brightness, {
    bool compact = false,
  }) {
    final base = ThemeData(
      useMaterial3: true,
      colorScheme: scheme,
      brightness: brightness,
      // 紧凑档：Material 组件整体收 6px（vertical −1.5 × 4px）。
      // 水平只收一半 —— 行高是这档要省的，左右缩太狠会挤到文案
      visualDensity: compact
          ? const VisualDensity(horizontal: -1.0, vertical: -1.5)
          : VisualDensity.standard,
    );

    final dark = brightness == Brightness.dark;
    return base.copyWith(
      scaffoldBackgroundColor: scheme.surface,
      extensions: [
        CortexTokens(
          // 侧栏（设计稿 --sb）：浅色 #EFEEF3 带冷紫调，与 macOS 的
          // sidebar/content 分层观感一致；深色 #232328 比 win 亮一档
          sidebar: dark ? const Color(0xFF232328) : const Color(0xFFEFEEF3),
          // 选中/悬停那一项（--sb2）：比 sidebar 再走一档。
          // 差得太小选中态看不出来，差得太大会读成「另一个区域」
          sidebarAccent: dark
              ? const Color(0xFF2A2A30)
              : const Color(0xFFE7E6ED),
          sidebarBorder: dark
              ? const Color(0xFF2A2A30)
              : const Color(0xFFE3E2E9),
          // fg3（--fg3）：浅 #63636C / 深 #9A9AA5。实色，不是透明度
          foregroundTertiary: dark
              ? const Color(0xFF9A9AA5)
              : const Color(0xFF63636C),
          // 反馈三色（--ok / --warn / --del 见 ColorScheme.error）。
          // 浅色取的是**深**的一档（#0F6B32 / #7C500A）：它们大多落在
          // fill 或白底上当文字/描边用，浅色的绿黄在那儿读不出来
          success: dark ? const Color(0xFF4ADE80) : const Color(0xFF0F6B32),
          warning: dark ? const Color(0xFFFBBF24) : const Color(0xFF7C500A),
          info: dark ? const Color(0xFF60A5FA) : const Color(0xFF2563EB),
          // 悬停垫（--hov）：半透明，叠在任何表面上都成立
          hover: dark ? const Color(0x0FFFFFFF) : const Color(0x0B17171A),
          // 品牌色垫底（--accSoft）：浅 10% / 深 18%
          accentSoft: dark ? const Color(0x2E7C82FF) : const Color(0x1A5B62F4),
          // 品牌色文字（--accInk）：链接、彩色数字
          accentInk: dark ? const Color(0xFFA5A9FF) : const Color(0xFF4348DE),
        ),
      ],
      dividerTheme: DividerThemeData(
        color: scheme.outlineVariant,
        thickness: 1,
        space: 1,
      ),
      textTheme: _textTheme(base.textTheme, scheme, compact: compact),
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

  static TextTheme _textTheme(
    TextTheme base,
    ColorScheme scheme, {
    bool compact = false,
  }) => base
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
        // 紧凑档正文 14px/1.66（设计稿的数）：字小一号，行距**反而放宽**
        // 一点 —— 小字挤行距是最快变得读不动的组合
        bodyLarge: compact
            ? base.bodyLarge?.copyWith(fontSize: 14, height: 1.66)
            : base.bodyLarge?.copyWith(fontSize: 15, height: 1.62),
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
