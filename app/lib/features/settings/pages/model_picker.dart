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
  final picked = await pickModel(
    context,
    ref,
    current: ref.read(selectedModelProvider),
  );
  if (picked == null || !context.mounted) return;
  ref.read(selectedModelProvider.notifier).select(picked.pick);
}

/// 同一个面板，但**只把选择交回来**，不写任何状态。
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
///   生图模型基本都不支持工具调用，开着的话，这个面板会把它该提供的
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

  /// 一句话说明这个面板是干嘛的。角色指派要它 ——
  /// 三个角色的面板长得一模一样，没有这一句分不清在选哪个。
  String? headline,
}) async {
  final catalog = ref.read(modelCatalogProvider).value;
  // 还没拉到就先拉一次再开，别弹一个空面板出来
  if (catalog == null) {
    ref.invalidate(modelCatalogProvider);
    return null;
  }

  // ⚠️ 返回值包一层记录。**直接返回 `ModelPick?` 是错的**：
  // 第一项要 pop 一个「空的选择」，而点面板外关掉给到的是
  // `null` —— 两者撞车的表现是「本来选着 Flash，随手关掉面板，
  // 就静默退回默认了」，而用户完全不知道自己改过什么。
  return showModalBottomSheet<({ModelPick pick})>(
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
            if (headline != null)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
                child: Text(
                  headline,
                  style: Theme.of(ctx).textTheme.titleSmall,
                ),
              ),
            _tile(
              ctx,
              pick: const ModelPick(),
              title: firstTitle,
              subtitle:
                  firstSubtitle ??
                  (catalog.defaultModel.isEmpty
                      ? '服务端配的那个'
                      : catalog.defaultModel),
              icon: Icons.settings_suggest_outlined,
              current: current,
            ),
            if (allowAuto && catalog.autoAvailable)
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
            //
            // 筛完一个不剩的分组整组不画：留一个空标题在那儿，
            // 看起来像「这条来源坏了」，而实际只是它没有符合条件的型号
            for (final (source, label, models)
                in catalog.grouped
                    .map(
                      (g) => (g.$1, g.$2, g.$3.where(where ?? _any).toList()),
                    )
                    .where((g) => g.$3.isNotEmpty)) ...[
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
                  // 一个工具都不调，界面上看不出任何异常。
                  //
                  // ⚠️ 只在**要跑 agent** 的场合拦。绘画角色关掉这一条 ——
                  // 生图模型基本都不支持工具调用，开着的话这个面板会把
                  // 它该提供的每一项都画成灰的
                  disabled: requireTools && !_chatty(m),
                  // 目录里查不到的能选，但要说一句「我们不知道」——
                  // 当成不行会把一个能用的模型挡在外面
                  note: switch (m) {
                    // 自定义端点：目录里那句话只能当提醒，不能当结论
                    _ when m.customEndpoint && m.toolCall == false =>
                      '官方那边这个型号不支持工具调用 —— 你这条是自定义端点，'
                          '后面接的是谁我们不知道，能不能跑 agent 得试一下',
                    _ when requireTools && m.toolCall == null && !_draws(m) =>
                      '服务端目录里没有它，不知道它支不支持工具调用',
                    _ => null,
                  },
                  // 拦下来时说的是**这一个**为什么不行，不是一句通用的
                  disabledReason: _draws(m)
                      ? '这是生图模型，对话跑不了 —— 想画图的话，'
                            '在「默认模型」里把它设成绘画模型'
                      : null,
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

Widget _tile(
  BuildContext ctx, {
  required ModelPick pick,
  required String title,
  required String subtitle,
  required IconData icon,
  required ModelPick current,
  bool disabled = false,
  String? note,

  /// 拦下来时**这一个**为什么不行。`null` = 用那句通用的（不支持工具调用）。
  ///
  /// 分开写是因为两种拦法的出路完全不同：不支持工具调用的只能换一个，
  /// 而生图模型是走错了地方 —— 它在「绘画模型」那一栏里正是要选的东西。
  String? disabledReason,
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
          disabled
              ? (disabledReason ?? '不支持工具调用 —— 选它 agent 就读不了文件、跑不了命令')
              : subtitle,
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
