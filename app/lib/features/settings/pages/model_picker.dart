/// 选对话模型的那个面板 —— **全仓库只此一份**。
///
/// 两个入口共用它：输入框下面那个 chip（逐轮切换，主入口），
/// 和设置页里点进来的。两份实现的下场是改了一处忘了另一处，
/// 而两处长得不一样时用户会以为它们是两个功能。
///
/// # 按来源分组，不是一个扁平列表
///
/// **一个型号名离开它的来源是没有意义的**：同一个名字可以在两条来源上
/// 都有（两个 OpenAI 兼容网关），而它们用的是不同的 key、不同的端点、
/// 不同的账单。分组标题还顺带回答了「用这个会不会花我的额度」。
///
/// # 三件这个面板必须说实话的事
///
/// 1. **不支持工具调用的模型要拦住。** 那样的模型跑 agent 会流畅地回答
///    而一个工具都不调，用户看不出哪里不对，只会觉得它「不听话」。
/// 2. **「不知道」与「不行」是两回事。** 服务端目录里查不到的模型，三个
///    能力字段都是 null。把它当成不行会把一个能用的模型挡在外面；当成行
///    又会让人踩上面那个坑。所以单独说一句「不知道」。
/// 3. **自动档只能说它真的在做的事。** 它挑的是「所有来源里最便宜、又
///    干得了这活的」，不是「最优的」—— 我们没有任何办法知道哪个模型对
///    某个具体问题答得更好。写成「智能匹配最优模型」是编的。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/model_option.dart';
import '../../../state/model_controller.dart';

/// 打开选模型面板。**chip 与设置页共用这一个** ——
/// 两份实现的下场是改了一处忘了另一处，而两处长得不一样时用户会以为
/// 它们是两个功能。
Future<void> showModelPicker(BuildContext context, WidgetRef ref) async {
  final catalog = ref.read(modelCatalogProvider).value;
  // 还没拉到就先拉一次再开，别弹一个空面板出来
  if (catalog == null) {
    ref.invalidate(modelCatalogProvider);
    return;
  }
  final current = ref.read(selectedModelProvider);

  // ⚠️ 返回值包一层记录。**直接返回 `ModelPick?` 是错的**：
  // 「跟随部署」这一项要 pop 一个「空的选择」，而点面板外关掉给到的是
  // `null` —— 两者撞车的表现是「本来选着 Flash，随手关掉面板，
  // 就静默退回默认了」，而用户完全不知道自己改过什么。
  final picked = await showModalBottomSheet<({ModelPick pick})>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (ctx) => SafeArea(
      child: ConstrainedBox(
        // 型号多的时候要能滚，而不是把面板撑到顶
        constraints: BoxConstraints(
          maxHeight: MediaQuery.of(ctx).size.height * 0.7,
        ),
        child: ListView(
          shrinkWrap: true,
          children: [
            _tile(
              ctx,
              pick: const ModelPick(),
              title: '跟随部署',
              subtitle: catalog.defaultModel.isEmpty
                  ? '服务端配的那个'
                  : catalog.defaultModel,
              icon: Icons.settings_suggest_outlined,
              current: current,
            ),
            if (catalog.autoAvailable)
              _tile(
                ctx,
                pick: const ModelPick(model: kAutoModel),
                title: '自动',
                // 与 `resolve_auto` 对得上：跨**所有来源**挑够用里最便宜的
                subtitle: '每轮在所有来源里挑最便宜、又干得了这活的',
                icon: Icons.auto_awesome_outlined,
                current: current,
              ),
            // 按来源分组：一个型号名离开它的来源没有意义 ——
            // key、端点、账单三样都跟着来源走
            for (final (source, label, models) in catalog.grouped) ...[
              const Divider(height: 12),
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 4, 16, 4),
                child: Row(
                  children: [
                    Text(
                      label.isEmpty ? source : label,
                      style: Theme.of(ctx).textTheme.labelSmall?.copyWith(
                        fontWeight: FontWeight.w700,
                      ),
                    ),
                    const SizedBox(width: 6),
                    if (models.isNotEmpty && !models.first.freeOfQuota)
                      Text(
                        '占配额',
                        style: Theme.of(ctx).textTheme.labelSmall?.copyWith(
                          color: Theme.of(ctx).colorScheme.onSurfaceVariant,
                        ),
                      )
                    else
                      Text(
                        '你的 key',
                        style: Theme.of(ctx).textTheme.labelSmall?.copyWith(
                          color: Theme.of(ctx).colorScheme.onSurfaceVariant,
                        ),
                      ),
                  ],
                ),
              ),
              for (final m in models)
                _tile(
                  ctx,
                  pick: ModelPick(source: m.source, model: m.id),
                  title: m.displayName,
                  subtitle: describeModel(m),
                  icon: Icons.memory_outlined,
                  current: current,
                  // 不支持工具调用的不给选：那样的模型会流畅地回答而
                  // 一个工具都不调，界面上看不出任何异常
                  disabled: m.toolCall == false,
                  // 目录里查不到的能选，但要说一句「我们不知道」——
                  // 当成不行会把一个能用的模型挡在外面
                  note: m.toolCall == null ? '服务端目录里没有它，不知道它支不支持工具调用' : null,
                ),
            ],
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 16),
              child: Text(
                '价格是每百万 token 的美元数，来自服务端内置的模型目录。'
                '这里不折算成人民币 —— 折算要汇率，而一个随手挑的汇率折出来的'
                '价格，你没法判断它对不对。',
                style: Theme.of(ctx).textTheme.labelSmall?.copyWith(
                  color: Theme.of(ctx).colorScheme.onSurfaceVariant,
                ),
              ),
            ),
          ],
        ),
      ),
    ),
  );
  if (picked == null || !context.mounted) return;
  ref.read(selectedModelProvider.notifier).select(picked.pick);
}

Widget _tile(
  BuildContext ctx, {
  required ModelPick pick,
  required String title,
  required String subtitle,
  required IconData icon,
  required ModelPick current,
  bool disabled = false,
  String? note,
}) {
  final theme = Theme.of(ctx);
  final scheme = theme.colorScheme;
  return ListTile(
    enabled: !disabled,
    leading: Icon(icon, size: 20),
    title: Text(title),
    subtitle: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          disabled ? '不支持工具调用 —— 选它 agent 就读不了文件、跑不了命令' : subtitle,
          style: disabled ? TextStyle(color: scheme.error) : null,
        ),
        if (note != null)
          Text(
            note,
            style: theme.textTheme.labelSmall?.copyWith(color: scheme.tertiary),
          ),
      ],
    ),
    trailing: pick == current ? const Icon(Icons.check_rounded) : null,
    onTap: () => Navigator.of(ctx).pop((pick: pick)),
  );
}

/// 一个模型的一行说明。**查不到就说查不到**，不编。
String describeModel(ModelOption m) {
  if (m.context == null && m.inputMicrosPerMtok == null) {
    return '服务端目录里没有它 —— 能力与价格都不知道';
  }
  final bits = <String>[
    '上下文 ${formatContext(m.context)}',
    '入 ${formatPerMtok(m.inputMicrosPerMtok)} / 出 ${formatPerMtok(m.outputMicrosPerMtok)}',
  ];
  if (m.vision == true) bits.add('看得懂图');
  if (m.reasoning == true) bits.add('有思考');
  return bits.join(' · ');
}
