/// 选对话模型的那个弹层 —— **全仓库只此一份**。
///
/// 三个入口共用它：输入框下面那个 chip（逐轮切换，主入口）、发送失败横幅
/// 的「换模型」、以及「默认模型」/图片页的角色指派。两份实现的下场是
/// 改了一处忘了另一处，而两处长得不一样时用户会以为它们是两个功能。
///
/// # 2026-08-25 起是锚定弹层，不再是全宽底部面板
///
/// 底部面板一拉就是大半屏，把正在读的对话整个盖掉 —— 而换模型是个
/// 「看一眼、点一下」的动作，不配一整屏。现在是宽 440、最高 55% 视口的
/// 紧凑弹层，锚在 chip 上方（参照 Cherry Studio 的模型下拉）；没有锚点的
/// 入口（错误横幅、角色指派）落在屏幕中央。结构从上到下：
/// 搜索框 → 能力筛选 chip → 按来源分组的列表 → 底部「配置模型服务」入口。
///
/// 能力筛选**只摆目录里真实存在的维度**（视觉/工具/思考/生图，都是
/// `ModelOption` 的字段）。Cherry 还有「视频」「免费」两个 —— 我们的目录
/// 没有这两个维度，摆出来就是界面替目录撒谎（CLAUDE.md 约束 2）。
///
/// # 按来源分组，不是一个扁平列表
///
/// **一个型号名离开它的来源是没有意义的**：同一个名字可以在两条来源上
/// 都有（两个 OpenAI 兼容网关），而它们用的是不同的 key、不同的端点、
/// 不同的账单。分组标题还顺带回答了「用这个会不会花我的额度」。
///
/// # 三件这个弹层必须说实话的事
///
/// 1. **不支持工具调用的模型要拦住。** 那样的模型跑 agent 会流畅地回答
///    而一个工具都不调，用户看不出哪里不对，只会觉得它「不听话」。
/// 2. **「不知道」与「不行」是两回事。** 服务端目录里查不到的模型，三个
///    能力字段都是 null。把它当成不行会把一个能用的模型挡在外面；当成行
///    又会让人踩上面那个坑。所以单独说一句「不知道」——**徽标同理**：
///    能力字段是 null 时那颗徽标不画，画了就是把「不知道」说成「有」。
/// 3. **自动档只能说它真的在做的事。** 它挑的是「所有来源里最便宜、又
///    干得了这活的」，不是「最优的」—— 我们没有任何办法知道哪个模型对
///    某个具体问题答得更好。写成「智能匹配最优模型」是编的。
library;

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/theme.dart';
import '../../../models/model_option.dart';
import '../../../state/model_controller.dart';
import '../settings_sheet.dart';

/// 打开选模型弹层并把选择写进 [selectedModelProvider]。
/// **chip、错误横幅共用这一个** —— 两份实现的下场是改了一处忘了另一处。
///
/// [anchor]：弹层锚在哪个矩形上方（全局坐标）。chip 传自己的位置；
/// 没有锚点的入口不传，弹层落在屏幕中央。
///
/// [where]：只列符合条件的。给「贴了图但这个模型看不懂图」那条出路用 ——
/// 它传的是 `vision != false`（**不是** `== true`，理由见
/// `selectedModelIsBlindProvider`）。不传 = 全都列。
Future<void> showModelPicker(
  BuildContext context,
  WidgetRef ref, {
  Rect? anchor,
  bool Function(ModelOption)? where,
}) async {
  final picked = await pickModel(
    context,
    ref,
    current: ref.read(selectedModelProvider),
    anchor: anchor,
    where: where,
    // 筛过的那次不摆「自动」：自动是跨**所有来源**挑最便宜的，它挑出来的
    // 那个未必符合这次的条件 —— 摆出来点了等于没换，而用户以为换了
    allowAuto: where == null,
  );
  if (picked == null || !context.mounted) return;
  ref.read(selectedModelProvider.notifier).select(picked.pick);
}

/// 同一个弹层，但**只把选择交回来**，不写任何状态。
///
/// # 为什么要拆出这一层
///
/// 「默认模型」那一页要选三次（主 / 快速 / 绘画），而每一次都不该动
/// 撰写框上那个逐轮选择。另写一个选择器的下场是两处的能力提示、分组、
/// 配额标注各走各的 —— 而用户会以为它们是两个功能。
///
/// # 两条规则必须能关掉，否则绘画角色一个都选不了
///
/// - [requireTools]：默认拦下不支持工具调用的。**绘画模型要关掉它** ——
///   生图模型基本都不支持工具调用，开着的话，这个弹层会把它该提供的
///   每一项都画成灰的。
/// - [where]：绘画角色只列真的画得出来的。不筛的话，用户会在一屏对话
///   模型里挑一个，然后在**保存那一刻**吃一个「生不了图」。
Future<({ModelPick pick})?> pickModel(
  BuildContext context,
  WidgetRef ref, {
  required ModelPick current,

  /// 只列符合条件的。`null` = 全都列。
  bool Function(ModelOption)? where,

  /// 不支持工具调用的要不要拦。
  bool requireTools = true,

  /// 第一项叫什么（那一项 pop 的是一个空的 [ModelPick]）。
  String firstTitle = '跟随部署',
  String? firstSubtitle,

  /// 摆不摆「自动」。角色指派摆不了 —— 它存的是一对
  /// `(来源, 型号)`，而「自动」不是任何一条来源上的型号。
  bool allowAuto = true,

  /// 一句话说明这个弹层是干嘛的。角色指派要它 ——
  /// 三个角色的弹层长得一模一样，没有这一句分不清在选哪个。
  String? headline,

  /// 弹层锚在哪个矩形上方（全局坐标）。`null` = 屏幕中央。
  Rect? anchor,
}) async {
  // ⚠️ **目录还没拉到时要等它，不能直接 return。**
  //
  // 从前这里是 `if (catalog == null) { invalidate(); return null; }` ——
  // 一个**静默的空操作**：点下去什么都不发生，也没有任何解释。
  //
  // 在「默认模型」那一页它侥幸不发作（那页自己 watch 着目录，打开就拉了），
  // 而 2026-08-22 新加的图片页没有 —— 于是**第一次点那个型号 chip 必然
  // 没反应**，用户报上来的就是这个。侥幸成立的正确性不是正确性。
  final ModelCatalog loaded;
  final cached = ref.read(modelCatalogProvider).value;
  if (cached != null) {
    loaded = cached;
  } else {
    try {
      loaded = await ref.read(modelCatalogProvider.future);
    } on Object catch (e) {
      // 拉不到就**说出来**。这条路上唯一比「点了没反应」更糟的，
      // 是弹一个空弹层让人以为自己一个模型都没配
      if (context.mounted) {
        ScaffoldMessenger.maybeOf(context)?.showSnackBar(
          SnackBar(
            content: Text('拉不到模型列表：${e is CortexApiException ? e.message : e}'),
          ),
        );
      }
      return null;
    }
  }
  // await 之后这个 context 可能已经没了（用户切走了）
  if (!context.mounted) return null;

  // 「配置模型服务」要在弹层关掉**之后**再推设置页 —— 在弹层自己的
  // context 上推的话，pop 掉弹层时那个 context 也跟着没了。
  // 这里捕获的是入口页的 context，弹层关掉后它还在。
  void openModelSettings() {
    if (context.mounted) showSettingsSheet(context);
  }

  // ⚠️ 返回值包一层记录。**直接返回 `ModelPick?` 是错的**：
  // 第一项要 pop 一个「空的选择」，而点弹层外关掉给到的是
  // `null` —— 两者撞车的表现是「本来选着 Flash，随手关掉弹层，
  // 就静默退回默认了」，而用户完全不知道自己改过什么。
  return showDialog<({ModelPick pick})>(
    context: context,
    // 弹层不是模态对话框：不压暗底下的对话 —— 它只占一角，
    // 压暗整屏会把「看一眼、点一下」变成「进了另一个界面」
    barrierColor: Colors.transparent,
    builder: (ctx) => CustomSingleChildLayout(
      delegate: _PopoverLayout(
        anchor: anchor,
        padding: MediaQuery.paddingOf(ctx),
      ),
      child: _PickerPopover(
        catalog: loaded,
        current: current,
        where: where,
        requireTools: requireTools,
        firstTitle: firstTitle,
        firstSubtitle: firstSubtitle,
        allowAuto: allowAuto,
        headline: headline,
        onConfigure: openModelSettings,
      ),
    ),
  );
}

/// 这个模型过不过得了筛子。
///
/// `m == null`（目录里查不到部署配的那个）一律**不过** —— 与 `where` 的
/// 语义一致：调用方给筛子，是因为他要的是「确定符合条件的那些」，
/// 而一个查不到的型号连它是什么都说不出来。
bool _passes(ModelOption? m, bool Function(ModelOption)? where) {
  if (where == null) return true;
  return m != null && where(m);
}

/// 把弹层摆到锚点上方；放不下就摆到下方；没有锚点就居中。
///
/// 尺寸钉死在这里而不是弹层自己的 ConstrainedBox 上：位置计算需要知道
/// 「最大能多大」，两处各写一份 440/55% 的话迟早漂。
class _PopoverLayout extends SingleChildLayoutDelegate {
  const _PopoverLayout({required this.anchor, required this.padding});

  final Rect? anchor;
  final EdgeInsets padding;

  static const _margin = 8.0;

  @override
  BoxConstraints getConstraintsForChild(BoxConstraints constraints) =>
      BoxConstraints(
        maxWidth: math.min(440, constraints.maxWidth - _margin * 2),
        // 最高 55% 视口：弹层要压不住大块聊天区 —— 全高的列表该去设置页看
        maxHeight: math.min(
          constraints.maxHeight * 0.55,
          constraints.maxHeight - padding.vertical - _margin * 2,
        ),
      );

  @override
  Offset getPositionForChild(Size size, Size child) {
    final a = anchor;
    if (a == null) {
      return Offset(
        (size.width - child.width) / 2,
        (size.height - child.height) / 2,
      );
    }
    // 与锚点左对齐（Cherry 的下拉也是这样）；越界就贴边
    final x = a.left
        .clamp(_margin, math.max(_margin, size.width - child.width - _margin))
        .toDouble();
    var y = a.top - child.height - _margin;
    if (y < padding.top + _margin) {
      // 上方放不下（chip 贴着屏幕顶）就翻到下方 —— 消失比翻面更糟
      y = math.min(a.bottom + _margin, size.height - child.height - _margin);
    }
    return Offset(x, y);
  }

  @override
  bool shouldRelayout(covariant _PopoverLayout old) =>
      old.anchor != anchor || old.padding != padding;
}

/// 能力筛选的维度。**只有目录里真实存在的四个** ——
/// 每一项都对应 [ModelOption] 的一个字段，多一个都是编的。
enum _Cap {
  vision('视觉', Icons.visibility_outlined),
  tools('工具', Icons.handyman_outlined),
  reasoning('思考', Icons.lightbulb_outline_rounded),
  imageOut('生图', Icons.image_outlined);

  const _Cap(this.label, this.icon);

  final String label;
  final IconData icon;

  /// 徽标 key 里的名字（测试按它找徽标，别随手改）。
  String get slug => switch (this) {
    _Cap.vision => 'vision',
    _Cap.tools => 'tools',
    _Cap.reasoning => 'reasoning',
    _Cap.imageOut => 'image-out',
  };

  /// ⚠️ 判据是 `== true`，不是 `!= false`：三态字段里 null 是「不知道」，
  /// 筛选和徽标都不能把「不知道」当成「有」。放行「不知道」的地方只有
  /// 选择本身（见 [_chatty]）—— 那是「不拦」，不是「宣称它有」
  bool has(ModelOption m) => switch (this) {
    _Cap.vision => m.vision == true,
    _Cap.tools => m.toolCall == true,
    _Cap.reasoning => m.reasoning == true,
    _Cap.imageOut => m.imageOutput == true,
  };
}

class _PickerPopover extends StatefulWidget {
  const _PickerPopover({
    required this.catalog,
    required this.current,
    required this.where,
    required this.requireTools,
    required this.firstTitle,
    required this.firstSubtitle,
    required this.allowAuto,
    required this.headline,
    required this.onConfigure,
  });

  final ModelCatalog catalog;
  final ModelPick current;
  final bool Function(ModelOption)? where;
  final bool requireTools;
  final String firstTitle;
  final String? firstSubtitle;
  final bool allowAuto;
  final String? headline;
  final VoidCallback onConfigure;

  @override
  State<_PickerPopover> createState() => _PickerPopoverState();
}

class _PickerPopoverState extends State<_PickerPopover> {
  String _query = '';
  final Set<_Cap> _caps = {};

  bool _matches(ModelOption m) {
    if (!(widget.where ?? _any)(m)) return false;
    for (final c in _caps) {
      if (!c.has(m)) return false;
    }
    if (_query.isEmpty) return true;
    final q = _query.toLowerCase();
    return m.displayName.toLowerCase().contains(q) ||
        m.id.toLowerCase().contains(q) ||
        m.sourceLabel.toLowerCase().contains(q);
  }

  void _pop(ModelPick pick) => Navigator.of(context).pop((pick: pick));

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final dark = scheme.brightness == Brightness.dark;

    final groups = widget.catalog.grouped
        .map((g) => (g.$1, g.$2, g.$3.where(_matches).toList()))
        .where((g) => g.$3.isNotEmpty)
        .toList();
    // 「跟随部署」「自动」不是目录里的型号：搜索/筛选一开就收起来 ——
    // 搜「qwen」还杵着一行「跟随部署」，读起来像它匹配上了
    final filtering = _query.isNotEmpty || _caps.isNotEmpty;
    // ⚠️ 调用方带了 `where` 时（「换一个能看图的模型」那条出路），
    // 「跟随部署」只在**部署配的那个真的符合条件**时才摆。
    //
    // 不判的话它是一条死路：用户正是因为当前模型看不懂图才走到这儿，
    // 而当前模型很可能就是部署配的那个 —— 点下去等于原地不动，
    // 而界面看起来像他已经换过了。
    final deploymentQualifies =
        widget.where == null ||
        _passes(widget.catalog.byId(widget.catalog.defaultModel), widget.where);

    // 阴影在 Material **外面**：Material 带 Clip.antiAlias（列表内容要贴着
    // 圆角裁掉），而阴影恰恰画在边界外 —— 放在裁剪层里面的阴影会被整个
    // 裁没，弹层贴在页面上没有任何浮起感（评审抓到的）。边框留在里面：
    // BorderSide 默认往内画，不会被裁。
    return DecoratedBox(
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
        // 同为**悬空**档（--sh3），与命令面板共用一个 token。
        // 此前是就地手调的一组值（blur 24 / dy 8），与设计稿的
        // 34 / 10 差着一档 —— 两个同层级的弹层因此浮得不一样高
        boxShadow: theme.cortex.shadow3,
      ),
      child: Material(
        // popover 层级的表面：深色下比卡片再亮一档（浮起来的东西更亮，
        // 见 theme.dart 深色阶那段），浅色下就是白
        color: dark ? scheme.surfaceContainerHigh : scheme.surface,
        elevation: 0,
        borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
        clipBehavior: Clip.antiAlias,
        child: DecoratedBox(
          decoration: BoxDecoration(
            border: Border.all(color: scheme.outlineVariant),
            borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
          ),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              if (widget.headline != null)
                Padding(
                  padding: const EdgeInsets.fromLTRB(14, 10, 14, 0),
                  child: Text(
                    widget.headline!,
                    style: theme.textTheme.titleSmall,
                  ),
                ),
              Padding(
                padding: const EdgeInsets.fromLTRB(10, 10, 10, 6),
                child: TextField(
                  autofocus: true,
                  onChanged: (v) => setState(() => _query = v.trim()),
                  style: theme.textTheme.bodyMedium,
                  decoration: InputDecoration(
                    hintText: '搜索模型…',
                    prefixIcon: const Icon(Icons.search_rounded, size: 17),
                    prefixIconConstraints: const BoxConstraints(
                      minWidth: 34,
                      minHeight: 30,
                    ),
                    contentPadding: const EdgeInsets.symmetric(
                      horizontal: 10,
                      vertical: 7,
                    ),
                  ),
                ),
              ),
              Padding(
                padding: const EdgeInsets.fromLTRB(10, 0, 10, 6),
                child: Wrap(
                  spacing: 6,
                  runSpacing: 4,
                  children: [
                    for (final cap in _Cap.values)
                      _capChip(
                        theme,
                        cap,
                        active: _caps.contains(cap),
                        onTap: () => setState(() {
                          _caps.contains(cap)
                              ? _caps.remove(cap)
                              : _caps.add(cap);
                        }),
                      ),
                  ],
                ),
              ),
              const Divider(height: 1),
              Flexible(
                child: ListView(
                  shrinkWrap: true,
                  padding: const EdgeInsets.symmetric(vertical: 4),
                  children: [
                    if (!filtering && deploymentQualifies) ...[
                      _specialRow(
                        theme,
                        icon: Icons.settings_suggest_outlined,
                        title: widget.firstTitle,
                        subtitle:
                            widget.firstSubtitle ??
                            (widget.catalog.defaultModel.isEmpty
                                ? '服务端配的那个'
                                : widget.catalog.defaultModel),
                        pick: const ModelPick(),
                      ),
                      if (widget.allowAuto && widget.catalog.autoAvailable)
                        _specialRow(
                          theme,
                          icon: Icons.auto_awesome_outlined,
                          title: '自动',
                          // 与 `resolve_auto` 对得上：跨**所有来源**挑够用里最便宜的
                          subtitle: '每轮在所有来源里挑最便宜、又干得了这活的',
                          pick: const ModelPick(model: kAutoModel),
                        ),
                    ],
                    // 筛完一个不剩的分组整组不画：留一个空组头在那儿，
                    // 看起来像「这条来源坏了」，而实际只是它没有符合条件的型号
                    for (final (source, label, models) in groups) ...[
                      _groupHeader(theme, source, label, models),
                      for (final m in models) _modelRow(theme, m),
                    ],
                    if (groups.isEmpty && filtering)
                      Padding(
                        padding: const EdgeInsets.all(16),
                        child: Text(
                          '没有匹配的模型',
                          textAlign: TextAlign.center,
                          style: theme.textTheme.bodySmall?.copyWith(
                            color: theme.cortex.foregroundTertiary,
                          ),
                        ),
                      ),
                  ],
                ),
              ),
              const Divider(height: 1),
              // 底部固定入口：想加来源/填 key 的人不该被迫先关掉弹层再翻设置。
              // 设置页开在第一项，那一项就是「模型服务」
              InkWell(
                onTap: () {
                  Navigator.of(context).pop();
                  widget.onConfigure();
                },
                child: Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 14,
                    vertical: 10,
                  ),
                  child: Row(
                    children: [
                      Icon(
                        Icons.settings_outlined,
                        size: 15,
                        color: scheme.onSurfaceVariant,
                      ),
                      const SizedBox(width: 8),
                      Text(
                        '配置模型服务',
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
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
    );
  }

  /// 一颗能力筛选 chip。选中态用品牌软垫 + 墨色文字，与列表行的选中
  /// 语言一致 —— 筛选是**动作**，配得上品牌色（不同于侧栏那种「位置」）。
  Widget _capChip(
    ThemeData theme,
    _Cap cap, {
    required bool active,
    required VoidCallback onTap,
  }) {
    final scheme = theme.colorScheme;
    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 3),
        decoration: BoxDecoration(
          color: active ? theme.cortex.accentSoft : scheme.surfaceContainerHigh,
          borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
          border: Border.all(
            color: active ? theme.cortex.accentInk : Colors.transparent,
          ),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              cap.icon,
              size: 12,
              color: active ? theme.cortex.accentInk : scheme.onSurfaceVariant,
            ),
            const SizedBox(width: 4),
            Text(
              cap.label,
              style: theme.textTheme.labelSmall?.copyWith(
                color: active
                    ? theme.cortex.accentInk
                    : scheme.onSurfaceVariant,
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _groupHeader(
    ThemeData theme,
    String source,
    String label,
    List<ModelOption> models,
  ) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 8, 14, 2),
      child: Row(
        children: [
          Text(
            label.isEmpty ? source : label,
            style: theme.textTheme.labelSmall?.copyWith(
              fontWeight: FontWeight.w700,
              color: theme.cortex.foregroundTertiary,
            ),
          ),
          const SizedBox(width: 6),
          // 「用这个会不会花我的额度」要在选之前就看得见
          Text(
            models.isNotEmpty && !models.first.freeOfQuota ? '占配额' : '你的 key',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.cortex.foregroundTertiary,
            ),
          ),
        ],
      ),
    );
  }

  /// 「跟随部署」「自动」那两行。与型号行分开写：它们没有能力徽标、
  /// 没有来源图标，也永远不会被拦。
  Widget _specialRow(
    ThemeData theme, {
    required IconData icon,
    required String title,
    required String subtitle,
    required ModelPick pick,
  }) {
    final scheme = theme.colorScheme;
    final selected = pick == widget.current;
    return InkWell(
      onTap: () => _pop(pick),
      hoverColor: theme.cortex.hover,
      child: Container(
        color: selected ? theme.cortex.accentSoft : null,
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 7),
        child: Row(
          children: [
            Icon(icon, size: 16, color: scheme.onSurfaceVariant),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    title,
                    style: theme.textTheme.bodyMedium?.copyWith(fontSize: 13.5),
                  ),
                  Text(
                    subtitle,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.labelSmall,
                  ),
                ],
              ),
            ),
            if (selected)
              Icon(Icons.check_rounded, size: 16, color: scheme.primary),
          ],
        ),
      ),
    );
  }

  Widget _modelRow(ThemeData theme, ModelOption m) {
    final scheme = theme.colorScheme;
    final pick = ModelPick(source: m.source, model: m.id);
    final selected = pick == widget.current;

    // 不支持工具调用的不给选：那样的模型会流畅地回答而一个工具都不调，
    // 界面上看不出任何异常。
    //
    // ⚠️ 只在**要跑 agent** 的场合拦。绘画角色关掉这一条 —— 生图模型
    // 基本都不支持工具调用，开着的话这个弹层会把它该提供的每一项都画成灰的
    final disabled = widget.requireTools && !_chatty(m);
    // 拦下来时说的是**这一个**为什么不行，不是一句通用的：不支持工具调用的
    // 只能换一个，而生图模型是走错了地方 —— 它在「绘画模型」那一栏里
    // 正是要选的东西
    final disabledReason = disabled
        ? (_draws(m)
              ? '这是生图模型，对话跑不了 —— 想画图的话，在「默认模型」里把它设成绘画模型'
              : '不支持工具调用 —— 选它 agent 就读不了文件、跑不了命令')
        : null;
    // ⚠️ **工具调用那两句只在要跑 agent 的场合说。**
    //
    // `requireTools == false` 的唯一去处是选**绘画模型**，而生图那条路
    // 根本不调工具。在那儿画一句「能不能跑 agent 得试一下」，是拿一件
    // 与这次选择无关的事去警告用户 —— 2026-08-22 实测：`gpt-image-2`
    // 下面挂着一整段橙色警告，而它是这台机器上唯一画得出图的型号。
    final note = !widget.requireTools
        ? null
        : switch (m) {
            // 自定义端点：目录里那句话只能当提醒，不能当结论
            _ when m.customEndpoint && m.toolCall == false =>
              '官方那边这个型号不支持工具调用 —— 你这条是自定义端点，'
                  '后面接的是谁我们不知道，能不能跑 agent 得试一下',
            _ when m.toolCall == null && !_draws(m) => '服务端目录里没有它，不知道它支不支持工具调用',
            _ => null,
          };

    final title = Row(
      children: [
        _sourceMark(theme, m),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            m.displayName,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: theme.textTheme.bodyMedium?.copyWith(
              fontSize: 13.5,
              color: disabled ? theme.cortex.foregroundTertiary : null,
            ),
          ),
        ),
        // 能力徽标：**只对 `== true` 画**。null 是「不知道」，
        // 画了就是把「不知道」说成「有」（约束 2 在徽标上的样子）
        for (final cap in _Cap.values)
          if (cap.has(m))
            Padding(
              key: Key('cap-${cap.slug}-${m.id}'),
              padding: const EdgeInsets.only(left: 4),
              child: Tooltip(
                message: cap.label,
                child: Container(
                  width: 18,
                  height: 18,
                  decoration: BoxDecoration(
                    color: scheme.surfaceContainerHigh,
                    borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
                  ),
                  child: Icon(
                    cap.icon,
                    size: 12,
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ),
            ),
        if (selected)
          Padding(
            padding: const EdgeInsets.only(left: 6),
            child: Icon(Icons.check_rounded, size: 16, color: scheme.primary),
          ),
      ],
    );

    return InkWell(
      onTap: disabled ? null : () => _pop(pick),
      hoverColor: theme.cortex.hover,
      child: Tooltip(
        // 价格与上下文挪到了悬停里：紧凑行放不下一整行数字，
        // 而「查不到就说查不到」那条规矩在 describeModel 里没变
        message: describeModel(m),
        waitDuration: const Duration(milliseconds: 700),
        child: Container(
          color: selected ? theme.cortex.accentSoft : null,
          constraints: const BoxConstraints(minHeight: 34),
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 6),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              title,
              if (disabledReason != null)
                Padding(
                  padding: const EdgeInsets.only(top: 2, left: 26),
                  child: Text(
                    disabledReason,
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: scheme.error,
                    ),
                  ),
                ),
              if (note != null)
                Padding(
                  padding: const EdgeInsets.only(top: 2, left: 26),
                  child: Text(
                    note,
                    // note 是「要留神」不是「坏了」，用警示琥珀。
                    // ⚠️ 不写 scheme.tertiary：这套主题没配它，会静默回落成
                    // Material 默认的 teal —— 一个不属于任何语义的颜色
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: theme.cortex.warning,
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }

  /// 来源小图标：首字母圆标。没有各家的品牌图标资源，而一个装错牌子的
  /// logo 比一个中性字母糟得多 —— 字母至少不会说错话。
  Widget _sourceMark(ThemeData theme, ModelOption m) {
    final label = m.sourceLabel.isEmpty ? m.source : m.sourceLabel;
    final letter = label.isEmpty ? '?' : label.characters.first.toUpperCase();
    return Container(
      width: 18,
      height: 18,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: theme.cortex.accentSoft,
        shape: BoxShape.circle,
      ),
      child: Text(
        letter,
        style: theme.textTheme.labelSmall?.copyWith(
          fontSize: 10,
          fontWeight: FontWeight.w700,
          color: theme.cortex.accentInk,
        ),
      ),
    );
  }
}

/// 不筛。抽成一个具名函数而不是就地写 `(_) => true`，是为了
/// 上面那句 `where ?? _any` 读起来仍然是一句话。
bool _any(ModelOption _) => true;

/// 这是个生图模型。
bool _draws(ModelOption m) => m.imageOutput == true;

/// 拿它跑对话说得过去吗。
///
/// # 为什么「能生图」在这里是一条**否决**
///
/// 生图与对话是两条协议：qwen-image 那些根本不吐 token。而它们在目录里
/// 查不到（`tool_call == null`），于是「不知道就放行」那条规则会把它们
/// 原样摆出来 —— 2026-08-20 在真实账号上就是这样：主模型选择器里
/// 20 个 qwen-image 全都能选，注解还写着一句轻飘飘的「不知道它支不支持
/// 工具调用」。
///
/// 但我们**知道**：`image_output` 那一位就是为绘画角色算出来的，
/// 同一份数据摆在那儿没人用。选中它的代价是每一轮对话都失败，
/// 而错误来自供应商，用户看不出是选错了。
///
/// 「不知道」仍然放行（那多半是刚发布的新型号），「知道它是画画的」不放。
bool _chatty(ModelOption m) =>
    // ⚠️ **自定义端点上一律放行。**
    //
    // 上面两条判据问的都是「厂商官方那个型号怎么样」，而中转站 / 公司网关
    // 后面接的是谁我们不知道。2026-08-20 实测：一个中转站的 `gpt-image-2`
    // 走聊天协议就能出图，而我们照着 OpenAI 官方目录把它画成灰的 ——
    // 一个实测可用的型号被挡在外面，用户只能来问为什么。
    //
    // 不知道就不拦。它真跑不了时，失败来自供应商且带着原话。
    m.customEndpoint || (m.toolCall != false && !_draws(m));

/// 一个模型的一行说明（弹层里挂在悬停上）。**查不到就说查不到**，不编。
String describeModel(ModelOption m) {
  if (m.context == null && m.inputMicrosPerMtok == null) {
    return '服务端目录里没有它 —— 能力与价格都不知道';
  }
  final bits = <String>[
    // ⚠️ 查不到就**整条不画**，不画「上下文 —」。
    //
    // 一个横杠在一行数字里读作「0」或者「不支持」，而事实是
    // 「目录里没有这个型号的上下文长度」。与列表行那边同一条规矩
    //（见 docs/design.md 第十节），从前这两处的规矩是反的。
    if (m.context != null && m.context! > 0) '上下文 ${formatContext(m.context)}',
    '入 ${formatPerMtok(m.inputMicrosPerMtok)} / 出 ${formatPerMtok(m.outputMicrosPerMtok)}'
        '（每百万 token，美元）',
  ];
  if (m.vision == true) bits.add('看得懂图');
  if (m.reasoning == true) bits.add('有思考');
  return bits.join(' · ');
}
