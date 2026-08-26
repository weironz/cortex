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
/// 所以每一位是三选一：**跟随自动 / 支持 / 不支持**，并且「跟随自动」那一档
/// 要**把自动的结论说出来**（「跟随自动 · 目录说支持」）—— 不说的话用户
/// 没有任何依据判断该不该覆盖它。
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

/// 一位能力的三种态。
enum _Tri {
  auto('跟随自动'),
  yes('支持'),
  no('不支持');

  const _Tri(this.label);
  final String label;

  static _Tri of(bool? v) => switch (v) {
    null => _Tri.auto,
    true => _Tri.yes,
    false => _Tri.no,
  };

  bool? get value => switch (this) {
    _Tri.auto => null,
    _Tri.yes => true,
    _Tri.no => false,
  };
}

class ModelCapsEditor extends StatefulWidget {
  const ModelCapsEditor({
    super.key,
    required this.model,
    required this.sourceLabel,
    required this.busy,
    required this.onSave,
  });

  /// 要编辑的那个模型 —— **带着已解析的能力**，好让「跟随自动」那一档
  /// 说得出自动的结论是什么。
  final FetchedModel model;

  /// 这个模型属于哪条来源，画在标题下面。同一个型号名可以在两条来源上
  /// 都有，不说清楚的话用户不知道自己在改哪一个。
  final String sourceLabel;

  final bool busy;

  /// 存。传的是**完整的**覆盖记录，缺省位 = 没意见。
  final void Function(CapsOverride caps) onSave;

  @override
  State<ModelCapsEditor> createState() => _ModelCapsEditorState();
}

class _ModelCapsEditorState extends State<ModelCapsEditor> {
  late final TextEditingController _name;
  late final TextEditingController _ctx;
  late _Tri _vision;
  late _Tri _tools;
  late _Tri _reasoning;
  late _Tri _imageOut;

  @override
  void initState() {
    super.initState();
    final o = widget.model.overridden;
    _name = TextEditingController(text: o.displayName ?? '');
    _ctx = TextEditingController(text: o.context?.toString() ?? '');
    _vision = _Tri.of(o.vision);
    _tools = _Tri.of(o.toolCall);
    _reasoning = _Tri.of(o.reasoning);
    _imageOut = _Tri.of(o.imageOutput);
  }

  @override
  void dispose() {
    _name.dispose();
    _ctx.dispose();
    super.dispose();
  }

  /// 现在这一屏对应的覆盖记录。
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
      vision: _vision.value,
      toolCall: _tools.value,
      reasoning: _reasoning.value,
      imageOutput: _imageOut.value,
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
                      '「跟随自动」= 用目录与供应商定义算出来的结论。只有当它说错了才需要改'
                      '—— 而自带网关的模型，它多半说不出来。',
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

  /// 一位能力：左边说是什么，下面三选一。
  Widget _cap(
    ThemeData theme, {
    required IconData icon,
    required String label,
    required String hint,
    required bool? auto,
    required _Tri value,
    required ValueChanged<_Tri> onPick,
  }) {
    final scheme = theme.colorScheme;
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
              if (value != _Tri.auto) _mineBadge(theme),
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
            child: _triPicker(theme, auto: auto, value: value, onPick: onPick),
          ),
        ],
      ),
    );
  }

  /// 「这一位是你改的」。
  ///
  /// 不标的话，下次打开只看到一个结论，分不清是目录说的还是自己按的，
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

  /// 三选一。**「跟随自动」那一档把自动的结论说出来** ——
  /// 不说的话用户没有任何依据判断该不该覆盖它。
  Widget _triPicker(
    ThemeData theme, {
    required bool? auto,
    required _Tri value,
    required ValueChanged<_Tri> onPick,
  }) {
    final autoWord = switch (auto) {
      null => '说不出',
      true => '说支持',
      false => '说不支持',
    };
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        for (final t in _Tri.values)
          _chip(
            theme,
            text: t == _Tri.auto ? '跟随自动 · $autoWord' : t.label,
            on: value == t,
            // 「跟随自动」而自动又说不出来时，这一档等于「不知道」——
            // 那是合法状态（发送时放行），不是错误，所以不画成警告色
            muted: t == _Tri.auto && auto == null,
            onTap: widget.busy ? null : () => onPick(t),
          ),
      ],
    );
  }

  Widget _chip(
    ThemeData theme, {
    required String text,
    required bool on,
    required bool muted,
    required VoidCallback? onTap,
  }) {
    final scheme = theme.colorScheme;
    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
        decoration: BoxDecoration(
          color: on ? theme.cortex.accentSoft : scheme.surfaceContainerHigh,
          borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
          border: Border.all(
            color: on ? theme.cortex.accentInk : Colors.transparent,
          ),
        ),
        child: Text(
          text,
          style: theme.textTheme.labelSmall?.copyWith(
            color: on
                ? theme.cortex.accentInk
                : (muted
                      ? theme.cortex.foregroundTertiary
                      : scheme.onSurfaceVariant),
            fontWeight: on ? FontWeight.w600 : null,
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
        // 「全部跟随自动」= 把这个模型的覆盖整条清掉。比让用户把四个 chip
        // 一个个点回去省事，而那正是他后悔时想做的事
        TextButton(
          onPressed: widget.busy || _current.isEmpty
              ? null
              : () => setState(() {
                  _name.clear();
                  _ctx.clear();
                  _vision = _Tri.auto;
                  _tools = _Tri.auto;
                  _reasoning = _Tri.auto;
                  _imageOut = _Tri.auto;
                }),
          child: const Text('全部跟随自动'),
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
