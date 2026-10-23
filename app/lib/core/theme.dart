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
    required this.accentGradientEnd,
    required this.shadow2,
    required this.shadow3,
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

  /// 渐变的**第二色**（设计稿 `--acc2`）。全产品只有发送键用它。
  ///
  /// # 为什么不是 [accentInk]
  ///
  /// 两者方向不同：`accentInk` 是同一支靛蓝**压深**（#4348DE），为的是当
  /// 文字时对比度够；`acc2` 是**往紫偏**（#8A62F4）。拿 accentInk 当渐变
  /// 终点，出来的是「靛蓝到更暗的靛蓝」—— 看着像没渲染完的纯色，那个
  /// 「唯一的主要动作」的分量就没了。
  final Color accentGradientEnd;

  /// 两档浮层投影（设计稿 `--sh2/--sh3`）：浮起 / 悬空。
  ///
  /// # 为什么不用 Material 的 elevation
  ///
  /// M3 的 elevation 在深色下是**加一层半透明白**来表达「更高」，而这套
  /// 设计的深色阶已经靠表面自己变亮做了分层（win → card → fill）。两者
  /// 叠在一起，浮起来的东西会白得发灰。这三档是纯投影，不动表面色。
  ///
  /// 深色的投影**更黑更散**（0.45→0.55、blur 更大）：深色背景上淡投影
  /// 根本看不见，照抄浅色的值等于没有投影。
  ///
  /// # 为什么没有设计稿那个 `--sh1`
  ///
  /// 它是「贴合」档，设计稿拿它给**并排的小卡片**描边式地垫一层。而这一
  /// 侧的既定约定是**相邻表面用边框分隔、只有浮层才用阴影**（卡片一律
  /// `side: BorderSide(outlineVariant)`）。两种做法混用会让同一屏里的卡片
  /// 有的带影有的不带 —— 收一档比硬塞一个消费者干净。
  final List<BoxShadow> shadow2;
  final List<BoxShadow> shadow3;

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
    // 推出来的那份不追求好看：没有第二支品牌色可推，渐变退化成纯色
    accentGradientEnd: s.primary,
    shadow2: _fallbackShadow(s, 4, 14, 0.10),
    shadow3: _fallbackShadow(s, 10, 34, 0.16),
  );

  static List<BoxShadow> _fallbackShadow(
    ColorScheme s,
    double dy,
    double blur,
    double alpha,
  ) => [
    BoxShadow(
      offset: Offset(0, dy),
      blurRadius: blur,
      color: s.shadow.withValues(alpha: alpha),
    ),
  ];

  /// 圆角阶 —— 2026-10-23 重设计，走 Linear / Vercel / Raycast 那一路。
  ///
  /// 主干五档：**窗口 16 / 卡片 12 / 输入框 8 / 行 8 / 胶囊 999**。
  /// 上一版输入框 18 比对话框还圆 —— 胶囊感是 2021 前后的消费级语言；
  /// 工具类产品的利落感来自**收窄的圆角**。对话框这一阶反而加大到 16：
  /// 收窄的是**天天在眼前**的东西（输入框、按钮、行），而对话框是一屏
  /// 只出现一次的浮层 —— 浮层圆一点才读得出「它飘在窗口之上」，
  /// 贴底的输入条圆一圈则只剩臃肿。成体系的意义不变：每一处圆角都
  /// 答得出「它是五档里的哪一档」。小构件（徽标 6、气泡 10）是辅助档。
  ///
  /// 旧的 Sm/Md/Lg/Xl/2xl 命名保留（消费面 40+ 处），**值重定**到新体系：
  /// 改名的收益是零，改值让所有旧调用点一次到位。
  static const double radiusSm = 6; // 徽标、小按钮（辅助档）
  static const double radiusMd = 8; // 行、菜单项（=radiusRow）
  static const double radiusLg = 10; // 气泡、主按钮（辅助档）
  static const double radiusXl = 12; // 卡片、面板（=radiusCard）
  static const double radius2xl = 16; // 对话框 = 浮起的窗口（=radiusWindow）

  /// 语义别名 —— 新代码写这几个，读的人不用背数值表。
  static const double radiusWindow = 16;
  static const double radiusCard = 12;
  static const double radiusInput = 8;
  static const double radiusRow = 8;
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
    Color? accentGradientEnd,
    List<BoxShadow>? shadow2,
    List<BoxShadow>? shadow3,
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
    accentGradientEnd: accentGradientEnd ?? this.accentGradientEnd,
    shadow2: shadow2 ?? this.shadow2,
    shadow3: shadow3 ?? this.shadow3,
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
      accentGradientEnd: Color.lerp(
        accentGradientEnd,
        other.accentGradientEnd,
        t,
      )!,
      // BoxShadow.lerpList 处理两侧长度不等的情况（短的那边补透明），
      // 自己按下标配对会在换主题时漏掉一层
      shadow2: BoxShadow.lerpList(shadow2, other.shadow2, t)!,
      shadow3: BoxShadow.lerpList(shadow3, other.shadow3, t)!,
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
    // ── 中性阶：锌灰（2026-10-23 重设计，Linear 一路）──
    //
    // 上一版整套灰往品牌靛蓝的方向偏（fg #17171A、sb #EFEEF3 都带紫调）。
    // 这轮去掉色偏：「偏色的灰」与发丝线的语言打架 —— 发丝线要的是
    // **绝对中性**的灰，任何色偏都会让线看起来脏。画布从纯白降到
    // #FAFAFA：大面积纯白刺眼，降一档既柔了底色，又保住「白卡片浮在
    // 灰画布上」的层级位置。文字压到近黑 #09090B —— 上一版与次级灰
    // 拉不开，「清晰」的第一来源就是主文字够黑。
    surface: Color(0xFFFAFAFA),
    onSurface: Color(0xFF09090B),
    // #545454 严格零色偏（zinc-600 的 #52525B 自带 9 的蓝偏，超发丝线
    // 的中性预算）：次级文字不需要任何色温
    onSurfaceVariant: Color(0xFF545454),
    // 发丝线用**实色**（zinc 系）：透明度造出来的线在不同表面上
    // 深浅不一，实色线在每个表面上都是同一条线
    outline: Color(0xFFD4D4D8),
    outlineVariant: Color(0xFFE8E8EA),
    // fill 阶：#F4F4F5（输入框、chip 底）→ #EBEBED（按下、第二档）
    surfaceContainerLowest: Colors.white,
    surfaceContainerLow: Color(0xFFFCFCFD),
    surfaceContainer: Color(0xFFFAFAFB),
    surfaceContainerHigh: Color(0xFFF4F4F5),
    surfaceContainerHighest: Color(0xFFEBEBED),
    inverseSurface: Color(0xFF202024),
    onInverseSurface: Color(0xFFFCFCFD),
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
    // ── 深色阶：锌灰，层越高越亮（2026-10-23 重设计）──
    //
    // 窗底从 #1B1B1F 压到 #0C0C0E、去掉紫调 —— 深色「现代感」的一半
    // 来自**够黑的底**（但绝不是纯黑 #000：纯黑上任何面板色都显得浮白，
    // 而 OLED 上的纯黑块之间没有层次）。面板落在 #18181B —— 这层微弱的
    // 色差就是深色下「卡片浮在底板上」的全部表达。浮起来的东西更亮，
    // 不是更暗 —— 反过来做会让弹层看着陷进去。
    // 发丝线换实色（zinc-700/800），与浅色同一套语言。
    surface: Color(0xFF0C0C0E),
    onSurface: Color(0xFFFAFAFA),
    // #A0A0A0 严格零色偏（zinc-400 的 #A1A1AA 自带 9 蓝偏）
    onSurfaceVariant: Color(0xFFA0A0A0),
    outline: Color(0xFF3F3F46),
    outlineVariant: Color(0xFF26262B),
    surfaceContainerLowest: Color(0xFF08080A),
    surfaceContainerLow: Color(0xFF121214),
    surfaceContainer: Color(0xFF18181B),
    surfaceContainerHigh: Color(0xFF1E1E22),
    surfaceContainerHighest: Color(0xFF242428),
    inverseSurface: Color(0xFFFAFAFA),
    onInverseSurface: Color(0xFF202024),
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
          // 侧栏：锌灰、无色偏（2026-10-23）。上一版 #EFEEF3 的冷紫调
          // 是旧中性阶的语言，这轮整套灰换锌系 —— 侧栏跟着换，
          // 否则「唯一偏色的那一块」看起来像没刷新。深色侧栏仍比
          // 窗底（#0C0C0E）亮一档（分层原则不变），但同步压暗
          sidebar: dark ? const Color(0xFF131316) : const Color(0xFFF1F1F3),
          // 选中/悬停那一项：比 sidebar 再走一档。
          //
          // ⚠️ **2026-10-23 实测反馈之后加深了。** 上一版浅色是 #EBEBED
          // （与 sidebar 只差 6 个亮度单位），而悬停是它 55% 的半透明叠色
          // （差 3）。于是「点一条会话」在屏幕上发生的事是：悬停 238 →
          // 选中 235 → 鼠标移开回到 241 —— **选中态与悬停态只差 3**，
          // 肉眼分不出，用户看到的就是「灰色背景一闪而过」。
          //
          // 现在浅色 #E4E4E7（差 13）、深色 #26262C（差 20）：选中与悬停
          // 之间拉开到 7~10，鼠标移开之后那一行仍然明确地亮着。
          // 仍然**中性**，不是品牌色 —— 「我在哪一条上」是位置不是动作。
          sidebarAccent: dark
              ? const Color(0xFF26262C)
              : const Color(0xFFE4E4E7),
          sidebarBorder: dark
              ? const Color(0xFF1F1F23)
              : const Color(0xFFE4E4E7),
          // fg3：浅 #8D8D8D / 深 #707070。实色、严格零色偏（zinc 系
          // 的 71717A / 8E8E96 自带 8-9 的蓝偏，超发丝线的中性预算）
          foregroundTertiary: dark
              ? const Color(0xFF707070)
              : const Color(0xFF8D8D8D),
          // 反馈三色（--ok / --warn / --del 见 ColorScheme.error）。
          // 浅色取的是**深**的一档（#0F6B32 / #7C500A）：它们大多落在
          // fill 或白底上当文字/描边用，浅色的绿黄在那儿读不出来
          success: dark ? const Color(0xFF4ADE80) : const Color(0xFF0F6B32),
          warning: dark ? const Color(0xFFFBBF24) : const Color(0xFF7C500A),
          info: dark ? const Color(0xFF60A5FA) : const Color(0xFF2563EB),
          // 悬停垫（--hov）：半透明，叠在任何表面上都成立
          hover: dark ? const Color(0x0DFFFFFF) : const Color(0x0A000000),
          // 品牌色垫底（--accSoft）：浅 10% / 深 18%
          accentSoft: dark ? const Color(0x2E7C82FF) : const Color(0x1A5B62F4),
          // 品牌色文字（--accInk）：链接、彩色数字
          accentInk: dark ? const Color(0xFFA5A9FF) : const Color(0xFF4348DE),
          // 渐变第二色（--acc2）：往**紫**偏，不是把靛蓝压深
          accentGradientEnd: dark
              ? const Color(0xFFA78BFA)
              : const Color(0xFF8A62F4),
          // 两档投影：Linear 一路的特征是「浅而克制」—— 浮层靠表面色阶
          // 分层，投影只负责一点点空间暗示。上一版 0.08/0.12 的浓度在
          // #FAFAFA 画布上会显脏；降一档，位移也收小
          shadow2: dark
              ? const [
                  BoxShadow(
                    offset: Offset(0, 2),
                    blurRadius: 8,
                    color: Color.fromRGBO(0, 0, 0, 0.40),
                  ),
                ]
              : const [
                  BoxShadow(
                    offset: Offset(0, 1),
                    blurRadius: 6,
                    color: Color.fromRGBO(9, 9, 11, 0.05),
                  ),
                ],
          shadow3: dark
              ? const [
                  BoxShadow(
                    offset: Offset(0, 8),
                    blurRadius: 28,
                    color: Color.fromRGBO(0, 0, 0, 0.50),
                  ),
                ]
              : const [
                  BoxShadow(
                    offset: Offset(0, 6),
                    blurRadius: 24,
                    color: Color.fromRGBO(9, 9, 11, 0.09),
                  ),
                ],
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
        // 2026-10-23 收紧：14/12 → 12/10。上一版的输入框是消费级
        // 语言（大圆角配大内边距）；工具类产品的输入框要**矮而利落**，
        // 一眼看到光标位置。填充色阶保持 —— 输入框仍然靠 fill 分层
        contentPadding: const EdgeInsets.symmetric(
          horizontal: 12,
          vertical: 10,
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
          // 2026-10-23 收紧 18/14 → 14/10（≈30%）：M3 出厂的敦实感
          // 是这套 UI 显旧的第三个来源（前两个：胶囊圆角、偏色灰）。
          // Linear / Raycast 的按钮是**紧凑**的 —— 高度 ≈ 36 而不是 48
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
          textStyle: const TextStyle(
            fontWeight: FontWeight.w600,
            // 按钮文字是小字号 + 大写不适用（中文），靠字重与字距
            letterSpacing: -0.1,
          ),
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
        // 输入框专属档 radiusInput（8）。值与行档相同但**语义独立**：
        // 一档一个语义，读代码的人不用猜
        borderRadius: BorderRadius.circular(CortexTokens.radiusInput),
        borderSide: BorderSide(color: color, width: width),
      );

  static TextTheme _textTheme(
    TextTheme base,
    ColorScheme scheme, {
    bool compact = false,
  }) => base
      .copyWith(
        // 2026-10-23 字阶：「清晰」的一半来自标题层拉得开。titleLarge 收到
        // 15.5 但字重升到 w700 —— Linear 一路的特征是**小而重**的标题，
        // 而不是大而中庸；titleSmall 的 0.4 字距是 M3 出厂的「胶囊标签」
        // 感，收到 0.2
        titleLarge: base.titleLarge?.copyWith(
          fontSize: 15.5,
          fontWeight: FontWeight.w700,
          letterSpacing: -0.2,
        ),
        titleMedium: base.titleMedium?.copyWith(
          fontSize: 14,
          fontWeight: FontWeight.w600,
          letterSpacing: -0.1,
        ),
        titleSmall: base.titleSmall?.copyWith(
          fontSize: 12,
          fontWeight: FontWeight.w600,
          letterSpacing: 0.2,
        ),
        // 紧凑档正文 14px/1.66：字小一号，行距**反而放宽**
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
