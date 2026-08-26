/// 「编辑模型」抽屉 —— 手工按下一个模型能干什么。
///
/// 视觉语言照 Cherry Studio 的同名面板：右侧滑出、只读的模型 ID 在最上、
/// 下面按「显示 / 能力 / 上限」分组，每组一行行摆开。
///
/// # ⚠️ 能力控件是**三态**，不是 Cherry 那种二值 chip
///
/// Cherry 的能力 chip 点亮 = 支持、不亮 = 不支持。照抄的话会把这个仓库
/// 反复保护的那条约定整个抹掉：**「不知道」与「不支持」是两回事**。
///
/// 三处判据都建立在它上面 —— 发送时「不知道」放行（与服务端
/// `ensure_can_see` 同一条）、筛选时「不知道」不算有、徽标在「不知道」时
/// 不画。二值开关一进来，用户只要打开过这个面板，那些他没意见的位就全被
/// 按成「不支持」了，而他什么都没点。
///
/// 所以界面上只有两个 chip（支持 / 不支持），但**「一个都没选中」是第三种
/// 状态**，不是「还没填完」：
///
///  * 查得到 → 把查到的那个预先选中，用户不动它就等于认可；
///  * 查不到 → 两个都不选中，并在下面写明「不选也照样能用」——
///    不写的话，一对空 chip 看起来像一道必答题，而事实相反。
///
/// 早先这里摆的是三个 chip（跟随自动 / 支持 / 不支持）。删掉「跟随自动」的
/// 理由是用户看不懂它：「跟随自动」讲的是我们内部的**取值来源**，而用户想
/// 知道的只是「它到底行不行」。来源改用 chip 的样式（淡 = 自动算的、
/// 实心 = 你按的）加一行小字表达，不再占一个选项位。
///
/// ⚠️ 但**只有用户真按过的位才存进覆盖记录**（见 [_current]）：预选中的那个
/// 值是当下算出来的结论，把它一并存下来等于把此刻的结论冻死 —— 之后目录
/// 更新了、或者这家接口开始报能力了，都再也顶不动那份冻住的值，而症状
/// （模型说它不支持）与「真的不支持」一模一样，没有人能发现。
///
/// # 只摆我们真的存得下的那几位
///
/// Cherry 那一版还有「嵌入 / 重排 / 音频 / 视频 / 币种 / 最大输入 Token」。
/// 我们的覆盖记录里没有这些位（见 [CapsOverride]），摆出来就是又一次
/// 「界面替目录撒谎」——用户按了、以为生效了，而它连发都没发出去。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../core/theme.dart';
import '../../../models/model_source.dart';

class ModelCapsEditor extends StatefulWidget {
  const ModelCapsEditor({
    super.key,
    required this.model,
    required this.sourceLabel,
    required this.canProbe,
    required this.busy,
    required this.onSave,
  });

  /// 要编辑的那个模型 —— **带着已解析的能力**，好让两个 chip 能按当下的
  /// 结论预先选中一个。
  final FetchedModel model;

  /// 这个模型属于哪条来源，画在标题下面。同一个型号名可以在两条来源上
  /// 都有，不说清楚的话用户不知道自己在改哪一个。
  final String sourceLabel;

  /// 这家的接口说不说得出模型能力。
  ///
  /// 一屏「说不出」而不解释为什么，用户不知道该等我们修还是自己补 ——
  /// 而多数 OpenAI 兼容网关的答案永远是后者。
  final bool canProbe;

  final bool busy;

  /// 存。传的是**完整的**覆盖记录，缺省位 = 没意见。
  final void Function(CapsOverride caps) onSave;

  @override
  State<ModelCapsEditor> createState() => _ModelCapsEditorState();
}

class _ModelCapsEditorState extends State<ModelCapsEditor> {
  late final TextEditingController _name;
  late final TextEditingController _ctx;

  /// 用户按下的那一位。**`null` = 他没按过**（此刻显示的是自动算出来的）。
  ///
  /// 这个 `null` 与「他明确按了不支持」必须分得开 —— 保存时只写按过的，
  /// 没按过的一位都不写。理由见 [_current]。
  bool? _vision;
  bool? _tools;
  bool? _reasoning;
  bool? _imageOut;

  @override
  void initState() {
    super.initState();
    final o = widget.model.overridden;
    _name = TextEditingController(text: o.displayName ?? '');
    _ctx = TextEditingController(text: o.context?.toString() ?? '');
    _vision = o.vision;
    _tools = o.toolCall;
    _reasoning = o.reasoning;
    _imageOut = o.imageOutput;
  }

  @override
  void dispose() {
    _name.dispose();
    _ctx.dispose();
    super.dispose();
  }

  /// 现在这一屏对应的覆盖记录。
  ///
  /// # ⚠️ 只写用户**真按过**的那几位
  ///
  /// 界面上那些「已经选中」的 chip 多半来自自动结论（目录 / 探测），
  /// 而**选中不等于他按过**。把它们一并写进覆盖记录，等于把此刻的自动
  /// 结论固化成他自己的断言。
  ///
  /// 那样做今天看不出差别，坏处全在以后：假如它冻在「不支持视觉」而那个
  /// 模型后来支持了，症状是贴图被拦 + 一句「这个模型看不懂图」——
  /// **跟一个真的看不懂图的模型长得一模一样**，用户没有任何线索去想到
  /// 「我几个月前在设置里冻过这一位」。
  ///
  /// （反方向没事：冻在「支持」而它后来不支持了，供应商会报错，看得见。
  /// 两个方向代价不对等，所以往看得见那边倒。）
  ///
  /// 「以当前测出来的为准」这个诉求由**探测结果**满足：那份存在
  /// `probed_caps` 里、点一次「获取模型列表」就刷新，且已经压过目录。
  /// 不必在这里再抄一遍。
  ///
  /// ⚠️ 空串 / 填不出数的输入按**没意见**处理，而不是存一个空字符串或 0：
  /// 一个 `context: 0` 会让上下文预算算出零可用空间，而那种失败发生在
  /// 字已经吐给用户之后。
  CapsOverride get _current {
    final name = _name.text.trim();
    final n = int.tryParse(_ctx.text.trim());
    return CapsOverride(
      displayName: name.isEmpty ? null : name,
      context: (n != null && n > 0) ? n : null,
      vision: _vision,
      toolCall: _tools,
      reasoning: _reasoning,
      imageOutput: _imageOut,
    );
  }

  bool get _same {
    final a = _current;
    final b = widget.model.overridden;
    return a.displayName == b.displayName &&
        a.context == b.context &&
        a.vision == b.vision &&
        a.toolCall == b.toolCall &&
        a.reasoning == b.reasoning &&
        a.imageOutput == b.imageOutput;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final m = widget.model;

    return Drawer(
      width: 420,
      backgroundColor: scheme.surface,
      shape: const RoundedRectangleBorder(),
      child: SafeArea(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _header(theme, m),
            const Divider(height: 1),
            Expanded(
              child: ListView(
                padding: const EdgeInsets.fromLTRB(18, 14, 18, 18),
                children: [
                  _section(theme, '显示'),
                  _field(
                    theme,
                    label: '显示名称',
                    controller: _name,
                    // 占位符给的是**自动解析出来的那个名字** —— 用户据此
                    // 知道留空会显示成什么，而不是对着一个空框猜
                    hint: m.displayName.isEmpty ? m.id : m.displayName,
                  ),
                  const SizedBox(height: 20),

                  _section(theme, '能力'),
                  Padding(
                    padding: const EdgeInsets.only(bottom: 12),
                    child: Text(
                      widget.canProbe
                          ? '下面选中的那些，是拉列表时这家接口自己报的。'
                                '报错了就点一下改过来。'
                          : '下面选中的那些，是从型号目录里查到的。'
                                '这家的接口不报能力（多数 OpenAI 兼容网关都不报），'
                                '所以查不到的那几行两个都不选中 —— 你点一下就补上了。',
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: theme.cortex.foregroundTertiary,
                        height: 1.6,
                      ),
                    ),
                  ),
                  _cap(
                    theme,
                    icon: Icons.visibility_outlined,
                    label: '视觉',
                    hint: '看得懂你贴的图',
                    auto: m.vision,
                    value: _vision,
                    onPick: (v) => setState(() => _vision = v),
                  ),
                  _cap(
                    theme,
                    icon: Icons.handyman_outlined,
                    label: '工具',
                    hint: '跑 agent 要它 —— 不支持的模型会流畅地答而一个工具都不调',
                    auto: m.toolCall,
                    value: _tools,
                    onPick: (v) => setState(() => _tools = v),
                  ),
                  _cap(
                    theme,
                    icon: Icons.lightbulb_outline_rounded,
                    label: '思考',
                    hint: '会先想一段再回答',
                    auto: m.reasoning,
                    value: _reasoning,
                    onPick: (v) => setState(() => _reasoning = v),
                  ),
                  _cap(
                    theme,
                    icon: Icons.image_outlined,
                    label: '生图',
                    hint: '点了能出图。与「视觉」是两件事',
                    auto: m.imageOutput,
                    value: _imageOut,
                    onPick: (v) => setState(() => _imageOut = v),
                  ),
                  const SizedBox(height: 12),

                  _section(theme, '上限'),
                  _field(
                    theme,
                    label: '上下文窗口',
                    controller: _ctx,
                    hint: m.context?.toString() ?? '目录里查不到',
                    numeric: true,
                    note: '填大了的代价不是报错，是字已经吐给你之后被供应商拒掉那一轮。',
                  ),
                ],
              ),
            ),
            const Divider(height: 1),
            _footer(theme),
          ],
        ),
      ),
    );
  }

  Widget _header(ThemeData theme, FetchedModel m) => Padding(
    padding: const EdgeInsets.fromLTRB(18, 14, 10, 12),
    child: Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text('编辑模型', style: theme.textTheme.titleMedium),
              const SizedBox(height: 3),
              // 模型 id 只读 + 可复制：它是发给供应商的那个字符串，
              // 排错时第一个要拿走的就是它
              InkWell(
                borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
                onTap: () async {
                  await Clipboard.setData(ClipboardData(text: m.id));
                  if (!mounted) return;
                  ScaffoldMessenger.maybeOf(
                    context,
                  )?.showSnackBar(SnackBar(content: Text('已复制 ${m.id}')));
                },
                child: Padding(
                  padding: const EdgeInsets.symmetric(vertical: 2),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Flexible(
                        child: Text(
                          m.id,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.labelSmall?.copyWith(
                            fontFamily: 'monospace',
                            color: theme.cortex.foregroundTertiary,
                          ),
                        ),
                      ),
                      const SizedBox(width: 4),
                      Icon(
                        Icons.copy_rounded,
                        size: 11,
                        color: theme.cortex.foregroundTertiary,
                      ),
                    ],
                  ),
                ),
              ),
              Text(
                widget.sourceLabel,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                ),
              ),
            ],
          ),
        ),
        IconButton(
          iconSize: 18,
          onPressed: () => Navigator.of(context).maybePop(),
          icon: const Icon(Icons.close_rounded),
        ),
      ],
    ),
  );

  Widget _section(ThemeData theme, String title) => Padding(
    padding: const EdgeInsets.only(bottom: 8),
    child: Text(
      title,
      style: theme.textTheme.labelSmall?.copyWith(
        fontWeight: FontWeight.w700,
        letterSpacing: 0.8,
        color: theme.cortex.foregroundTertiary,
      ),
    ),
  );

  /// 一位能力：上面说是什么，下面两个 chip。
  ///
  /// # ⚠️ 没有「自动」这个选项
  ///
  /// 第一版有三个 chip：自动 / 支持 / 不支持。用户当场指出它是有歧义的 ——
  /// **选中「自动」到底是「算出来了」还是「没算出来」，从选中态上读不出来**。
  ///
  /// 现在状态由「选了什么」直接表达：
  ///
  /// | 情况 | 界面 |
  /// |---|---|
  /// | 算出来支持 | 「支持」选中，**浅色**（继承来的） |
  /// | 算出来不支持 | 「不支持」选中，浅色 |
  /// | **算不出来** | **两个都不选中** —— 明摆着是「等你说」 |
  /// | 用户按过 | **实色** + 「你改的」+ 那一行出现「改回自动」 |
  ///
  /// 浅色与实色的区别不只是好看：它回答「这个结论是谁下的」，而那正是
  /// 用户决定要不要改它时唯一需要知道的事。
  Widget _cap(
    ThemeData theme, {
    required IconData icon,
    required String label,
    required String hint,
    required bool? auto,
    required bool? value,
    required ValueChanged<bool?> onPick,
  }) {
    final scheme = theme.colorScheme;
    final mine = value != null;
    // 此刻这一位到底算什么：他按过就是他的，没按过就是自动的
    final effective = value ?? auto;

    return Padding(
      padding: const EdgeInsets.only(bottom: 14),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(icon, size: 15, color: scheme.onSurfaceVariant),
              const SizedBox(width: 7),
              Text(label, style: theme.textTheme.bodyMedium),
              const SizedBox(width: 7),
              if (mine) ...[
                _mineBadge(theme),
                const SizedBox(width: 4),
                // 改回自动**只在他改过时出现** —— 没改过的时候摆一个
                // 「改回自动」，用户会问「我改过吗」
                InkWell(
                  onTap: widget.busy ? null : () => onPick(null),
                  borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 4,
                      vertical: 1,
                    ),
                    child: Text(
                      '改回自动',
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: theme.cortex.foregroundTertiary,
                        decoration: TextDecoration.underline,
                      ),
                    ),
                  ),
                ),
              ],
            ],
          ),
          Padding(
            padding: const EdgeInsets.only(left: 22, top: 2, bottom: 7),
            child: Text(
              hint,
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.only(left: 22),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Wrap(
                  spacing: 6,
                  runSpacing: 6,
                  children: [
                    _chip(
                      theme,
                      text: '支持',
                      on: effective == true,
                      mine: mine,
                      onTap: widget.busy ? null : () => onPick(true),
                    ),
                    _chip(
                      theme,
                      text: '不支持',
                      on: effective == false,
                      mine: mine,
                      onTap: widget.busy ? null : () => onPick(false),
                    ),
                  ],
                ),
                if (!mine)
                  Padding(
                    padding: const EdgeInsets.only(top: 5),
                    child: Text(
                      auto == null
                          // 两个都没选中时**必须**说这句：不说的话，一个
                          // 空着的选择看起来像「你必须先选一个才能用」，
                          // 而事实相反 —— 不确定是放行的
                          ? '没查到 —— 你选一个会更准，不选也照样能用'
                          : '自动判断的，你还没改过',
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

  /// 「这一位是你改的」。
  ///
  /// 不标的话，下次打开只看到一个结论，分不清是自动算的还是自己按的，
  /// 于是不敢动 —— 而「不敢动」正是这个面板要消除的那种状态。
  Widget _mineBadge(ThemeData theme) => Container(
    padding: const EdgeInsets.symmetric(horizontal: 5, vertical: 1),
    decoration: BoxDecoration(
      color: theme.cortex.accentSoft,
      borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
    ),
    child: Text(
      '你改的',
      style: theme.textTheme.labelSmall?.copyWith(
        color: theme.cortex.accentInk,
        fontSize: 10,
        height: 1.4,
      ),
    ),
  );

  /// 一颗 chip。
  ///
  /// `mine` 决定选中态画多重：**他按过的是实色，继承来的是浅色**。
  /// 两者画成一样的话，用户分不出「系统这么认为」和「我这么说过」——
  /// 而那正是他决定要不要改时唯一需要知道的事。
  Widget _chip(
    ThemeData theme, {
    required String text,
    required bool on,
    required bool mine,
    required VoidCallback? onTap,
  }) {
    final scheme = theme.colorScheme;
    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 5),
        decoration: BoxDecoration(
          color: on
              ? (mine ? theme.cortex.accentSoft : scheme.surfaceContainerHigh)
              : Colors.transparent,
          borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
          border: Border.all(
            color: on
                ? (mine ? theme.cortex.accentInk : scheme.outlineVariant)
                : scheme.outlineVariant,
          ),
        ),
        child: Text(
          text,
          style: theme.textTheme.labelSmall?.copyWith(
            color: on
                ? (mine ? theme.cortex.accentInk : scheme.onSurface)
                : theme.cortex.foregroundTertiary,
            fontWeight: on && mine ? FontWeight.w600 : null,
          ),
        ),
      ),
    );
  }

  Widget _field(
    ThemeData theme, {
    required String label,
    required TextEditingController controller,
    required String hint,
    bool numeric = false,
    String? note,
  }) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Text(label, style: theme.textTheme.bodyMedium),
      const SizedBox(height: 6),
      TextField(
        controller: controller,
        enabled: !widget.busy,
        keyboardType: numeric ? TextInputType.number : null,
        inputFormatters: numeric
            ? [FilteringTextInputFormatter.digitsOnly]
            : null,
        style: theme.textTheme.bodyMedium,
        // 占位符是**自动解析出来的值**：留空就按它走
        decoration: InputDecoration(
          hintText: hint,
          isDense: true,
          contentPadding: const EdgeInsets.symmetric(
            horizontal: 11,
            vertical: 9,
          ),
        ),
        onChanged: (_) => setState(() {}),
      ),
      if (note != null) ...[
        const SizedBox(height: 5),
        Text(
          note,
          style: theme.textTheme.labelSmall?.copyWith(
            color: theme.cortex.foregroundTertiary,
            height: 1.6,
          ),
        ),
      ],
    ],
  );

  Widget _footer(ThemeData theme) => Padding(
    padding: const EdgeInsets.fromLTRB(10, 8, 14, 10),
    child: Row(
      children: [
        // 「全部改回自动」= 把这个模型的覆盖整条清掉。比让用户一行行点
        // 「改回自动」省事，而那正是他后悔时想做的事。措辞与每行那个
        // 保持一致 —— 同一个动作两种叫法，用户会以为是两件事
        TextButton(
          onPressed: widget.busy || _current.isEmpty
              ? null
              : () => setState(() {
                  _name.clear();
                  _ctx.clear();
                  _vision = null;
                  _tools = null;
                  _reasoning = null;
                  _imageOut = null;
                }),
          child: const Text('全部改回自动'),
        ),
        const Spacer(),
        FilledButton(
          onPressed: widget.busy || _same
              ? null
              : () => widget.onSave(_current),
          child: Text(widget.busy ? '保存中…' : '保存'),
        ),
      ],
    ),
  );
}
