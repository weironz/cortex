/// 「添加模型」弹窗 —— 搜索 + **能力筛选** + 逐个勾选。
///
/// 形态照 Cherry Studio 那个右侧面板：顶上搜索，下面一排筛选 chip
/// （各带计数），再下面是按系列分组的型号，每条带能力徽标与 `+`。
///
/// # 为什么不再「获取模型列表」整份塞进来
///
/// 那把 alibaba key 拉回来 **240 个型号**。整份塞进去之后，撰写框的选择器
/// 里就是 240 条，而其中真正会用的不超过五个。挑，是这个弹窗存在的理由。
///
/// # 三件这个弹窗必须说实话的事
///
/// 1. **不支持工具调用的要在加进来之前就警告。** 那样的模型跑 agent 会
///    流畅地回答而一个工具都不调 —— 等他选中之后在选择器里拦，已经晚了
///    一步：他得先加、再选、再被拦，才知道这个型号不能用。
/// 2. **「不知道」不画徽标**，而不是画一个灰的。灰的看起来像「不支持」，
///    而实际是目录里查不到 —— 那多半是刚发布的新型号。
/// 3. **回落要说出来。** 拉不动时服务端回内置那份并置 `live = false`，
///    那份是编译期写死的，未必与这个账号一致。
library;

import 'package:flutter/material.dart';

import '../../../models/model_option.dart';
import '../../../models/model_source.dart';

/// 一个筛选维度。
enum _Facet {
  all('全部'),
  tools('能跑 agent'),
  vision('看得懂图'),
  image('能生图'),
  reasoning('有思考');

  const _Facet(this.label);
  final String label;

  bool matches(FetchedModel m) => switch (this) {
    _Facet.all => true,
    _Facet.tools => m.toolCall == true,
    _Facet.vision => m.vision == true,
    _Facet.image => m.imageOutput == true,
    _Facet.reasoning => m.reasoning == true,
  };
}

/// 弹出「添加模型」。返回用户勾选的型号名；取消返回 `null`。
Future<List<String>?> showAddModels(
  BuildContext context, {
  required FetchedModels fetched,
  required List<String> already,
}) => showDialog<List<String>>(
  context: context,
  builder: (_) => _AddModelsDialog(fetched: fetched, already: already),
);

class _AddModelsDialog extends StatefulWidget {
  const _AddModelsDialog({required this.fetched, required this.already});

  final FetchedModels fetched;

  /// 这条来源已经有的型号 —— 标出来且不给重复添加。
  final List<String> already;

  @override
  State<_AddModelsDialog> createState() => _AddModelsDialogState();
}

class _AddModelsDialogState extends State<_AddModelsDialog> {
  _Facet _facet = _Facet.all;
  String _query = '';
  final _picked = <String>{};

  /// 当前筛选 + 搜索之后剩下的。
  List<FetchedModel> get _shown {
    final q = _query.trim().toLowerCase();
    return widget.fetched.models
        .where(_facet.matches)
        .where((m) => q.isEmpty || m.id.toLowerCase().contains(q))
        .toList(growable: false);
  }

  /// 这家有几个「会画但我们没接」的（当前搜索范围内）。
  ///
  /// 用来把「能生图 0」这个空结果解释清楚 —— 0 的原因是我们的缺口，
  /// 不是这家不会画。
  int get _unwiredCount {
    final q = _query.trim().toLowerCase();
    return widget.fetched.models
        .where((m) => m.imageUnwired)
        .where((m) => q.isEmpty || m.id.toLowerCase().contains(q))
        .length;
  }

  /// 每个维度剩几个。**搜索也算进去** —— 搜了「image」之后还显示
  /// 「能跑 agent 120」会让人以为筛选没生效。
  int _count(_Facet f) {
    final q = _query.trim().toLowerCase();
    return widget.fetched.models
        .where(f.matches)
        .where((m) => q.isEmpty || m.id.toLowerCase().contains(q))
        .length;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final shown = _shown;

    return AlertDialog(
      title: Row(
        children: [
          Expanded(child: Text('添加模型（${widget.fetched.models.length}）')),
          TextButton(
            onPressed: shown.isEmpty
                ? null
                : () => setState(() {
                    for (final m in shown) {
                      if (!widget.already.contains(m.id)) _picked.add(m.id);
                    }
                  }),
            child: Text('全选这 ${shown.length} 个'),
          ),
        ],
      ),
      content: SizedBox(
        width: 520,
        height: 460,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            // 拉不动时的回落**必须说出来** —— 那份是编译期写死的清单
            if (!widget.fetched.live && widget.fetched.note != null)
              Padding(
                padding: const EdgeInsets.only(bottom: 8),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Icon(
                      Icons.info_outline,
                      size: 14,
                      color: scheme.onSurfaceVariant,
                    ),
                    const SizedBox(width: 6),
                    Expanded(
                      child: Text(
                        widget.fetched.note!,
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            TextField(
              autofocus: true,
              onChanged: (v) => setState(() => _query = v),
              decoration: const InputDecoration(
                isDense: true,
                hintText: '搜索型号',
                prefixIcon: Icon(Icons.search, size: 16),
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 8),
            Wrap(
              spacing: 6,
              children: [
                // 计数为 0 的 chip **照样显示**（Cherry 的「重排 0」就是
                // 这样）—— 藏起来的话用户不知道那一类存在，会以为我们
                // 没有这个筛选维度
                for (final f in _Facet.values)
                  FilterChip(
                    label: Text('${f.label} ${_count(f)}'),
                    selected: _facet == f,
                    onSelected: (_) => setState(() => _facet = f),
                    labelStyle: theme.textTheme.labelSmall,
                    visualDensity: VisualDensity.compact,
                  ),
              ],
            ),
            const Divider(height: 16),
            Expanded(
              child: shown.isEmpty
                  ? Center(
                      child: Padding(
                        padding: const EdgeInsets.symmetric(horizontal: 24),
                        child: Text(
                          // 「能生图」筛出 0 条，而这家其实有会画的型号 ——
                          // 光说「没有符合条件的」等于让用户以为这家不会画。
                          // 这正是 2026-08-20 那次提问的现场
                          _facet == _Facet.image && _unwiredCount > 0
                              ? '这家有 $_unwiredCount 个会生图的型号，但我们还没接它的生图接口，'
                                    '所以都没列出来。现在能画的是通义千问与 Google。'
                              : '没有符合条件的型号',
                          textAlign: TextAlign.center,
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: scheme.onSurfaceVariant,
                          ),
                        ),
                      ),
                    )
                  : ListView.builder(
                      itemCount: shown.length,
                      itemBuilder: (_, i) => _row(shown[i], theme),
                    ),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: _picked.isEmpty
              ? null
              : () => Navigator.of(context).pop(_picked.toList()),
          child: Text('添加 ${_picked.length} 个'),
        ),
      ],
    );
  }

  Widget _row(FetchedModel m, ThemeData theme) {
    final scheme = theme.colorScheme;
    final have = widget.already.contains(m.id);
    // **加之前就警告**，而不是等他选中之后在选择器里拦 —— 那要他先加、
    // 再选、再被拦，才知道这个型号跑不了 agent
    final noTools = m.toolCall == false;

    return CheckboxListTile(
      dense: true,
      contentPadding: EdgeInsets.zero,
      controlAffinity: ListTileControlAffinity.leading,
      value: have || _picked.contains(m.id),
      // 已经有的不给重复添加
      onChanged: have
          ? null
          : (v) => setState(() {
              if (v == true) {
                _picked.add(m.id);
              } else {
                _picked.remove(m.id);
              }
            }),
      title: Row(
        children: [
          Flexible(
            child: Text(
              m.id,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodySmall,
            ),
          ),
          if (have)
            Padding(
              padding: const EdgeInsets.only(left: 6),
              child: Text(
                '已加入',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ),
          for (final b in badgesOf(m))
            Padding(
              padding: const EdgeInsets.only(left: 6),
              child: Tooltip(
                message: b.$2,
                child: Icon(b.$1, size: 13, color: scheme.onSurfaceVariant),
              ),
            ),
        ],
      ),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            noTools
                ? '不支持工具调用 —— 加进来能选，但 agent 用它读不了文件、跑不了命令'
                : describeFetched(m),
            style: theme.textTheme.labelSmall?.copyWith(
              color: noTools ? scheme.error : scheme.onSurfaceVariant,
            ),
          ),
          // ⚠️ 「它会画，但我们没接这家」**要单独说出来**。
          //
          // 不说的话，它在界面上与一个普通对话模型长得一模一样，而
          // 「能生图」那个筛选是 0 —— 用户看到一屏名字带 image 的型号
          // 配一个 0，只能来问为什么（2026-08-20 就是这么问的）。
          //
          // 措辞上责任在我们，不在模型：写「它不支持生图」是错的。
          if (m.imageUnwired)
            Text(
              '它能生图，但我们还没接这家的生图接口 —— 加进来也画不了。现在能画的是通义千问与 Google。',
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.tertiary,
              ),
            ),
        ],
      ),
    );
  }
}

/// 一个型号的能力徽标。
///
/// **查不到的不画** —— 画一个灰的会被读成「不支持」，而实际是「不知道」。
List<(IconData, String)> badgesOf(FetchedModel m) => [
  if (m.toolCall == true) (Icons.build_outlined, '支持工具调用，能跑 agent'),
  if (m.vision == true) (Icons.visibility_outlined, '看得懂图'),
  if (m.imageOutput == true) (Icons.brush_outlined, '能生图'),
  if (m.reasoning == true) (Icons.lightbulb_outline, '有思考过程'),
];

/// 同上，但输入是选择器那份目录里的 [ModelOption]。
///
/// 与 [badgesOf] 挨着放：**判据只此一处**。两个地方各画各的徽标，
/// 迟早出现「弹窗里说能生图、详情页里不说」。
List<(IconData, String)> badgesOfOption(ModelOption? m) => m == null
    ? const []
    : [
        if (m.toolCall == true) (Icons.build_outlined, '支持工具调用，能跑 agent'),
        if (m.vision == true) (Icons.visibility_outlined, '看得懂图'),
        if (m.reasoning == true) (Icons.lightbulb_outline, '有思考过程'),
      ];

/// 一个型号的一行说明。**查不到就说查不到**，不编。
String describeFetched(FetchedModel m) {
  if (m.context == null && m.inputMicrosPerMtok == null) {
    return '服务端目录里没有它 —— 能力与价格都不知道';
  }
  final bits = <String>[
    '上下文 ${formatContextTokens(m.context)}',
    '入 ${formatUsdPerMtok(m.inputMicrosPerMtok)} / 出 ${formatUsdPerMtok(m.outputMicrosPerMtok)}',
  ];
  return bits.join(' · ');
}

/// 上下文窗口的短写法：`128000` → `128K`。
String formatContextTokens(int? tokens) {
  if (tokens == null || tokens <= 0) return '—';
  if (tokens >= 1000000) {
    final m = tokens / 1000000;
    return '${m == m.roundToDouble() ? m.toInt() : m.toStringAsFixed(1)}M';
  }
  if (tokens >= 1000) return '${(tokens / 1000).round()}K';
  return '$tokens';
}

/// 每百万 token 的美元微元 → 人看得懂的价格。
String formatUsdPerMtok(int? micros) {
  if (micros == null) return '—';
  final usd = micros / 1000000;
  if (usd == 0) return r'$0';
  if (usd < 0.01) return '\$${usd.toStringAsFixed(4)}';
  return '\$${usd.toStringAsFixed(2)}';
}
