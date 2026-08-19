/// 选对话模型 —— **面板与摘要行，全仓库只此一份**。
///
/// # 为什么摘要是一行，不是一屏单选按钮
///
/// 原先这里直接铺开「跟随部署 / 自动 / 每个型号」六个单选项，紧接着下面
/// 是「自己的 API key」和「本机模型」。用户的原话是「上来就给四种选择，
/// 太难理解了」—— 而他圈出来的四项**看起来并列，实际回答三个不同问题**：
///
/// | 看着像 | 其实是 |
/// |---|---|
/// | 跟随部署 / 自动 / 某型号 | 一个问题的三个答案：**用哪个模型** |
/// | 自己的 API key | 另一个问题：**谁的账户付钱** |
/// | 本机模型 | 第三个问题：**离线时打给谁** |
///
/// 铺开六个单选项会把第一个问题的体积撑到另外两个的三倍，于是它看起来
/// 像「主菜单」而后两项像它的下级。收成一行之后，三个问题在页面上体积
/// 相当，各自带标题 —— 那才是它们真实的关系。
///
/// 换模型的主入口本来也不在这儿了：输入框下面那个 chip 是逐轮切换用的
/// （见 `ModelChip`）。这一页服务的是「我要看清楚再挑」，一次点开足够。
///
/// # 三件这个面板必须说实话的事
///
/// 1. **不支持工具调用的模型要拦住。** 那样的模型跑 agent 会流畅地回答
///    而一个工具都不调，用户看不出哪里不对，只会觉得它「不听话」。
/// 2. **「不知道」与「不行」是两回事。** 服务端目录里查不到的模型，三个
///    能力字段都是 null。把它当成不行会把一个能用的模型挡在外面；当成行
///    又会让人踩上面那个坑。所以单独说一句「不知道」。
/// 3. **自动档只能说它真的在做的事。** 它挑的是「在能干这活的模型里最
///    便宜的」，不是「最优的」—— 我们没有任何办法知道哪个模型对某个具体
///    问题答得更好。写成「智能匹配最优模型」是编的。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
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

  // ⚠️ 返回值包一层记录。**直接返回 `String?` 是错的**：
  // 「跟随部署」这一项要 pop 一个 null，而点面板外关掉给到的也是 null ——
  // 两者撞车的表现是「本来选着 Flash，随手关掉面板，就静默退回默认了」，
  // 而用户完全不知道自己改过什么。包一层之后，取消是 `null`，
  // 选中「跟随部署」是 `(id: null)`
  final picked = await showModalBottomSheet<({String? id})>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (ctx) => SafeArea(
      child: ConstrainedBox(
        // 型号多的供应商（OpenAI 十几个）要能滚，而不是把面板撑到顶
        constraints: BoxConstraints(
          maxHeight: MediaQuery.of(ctx).size.height * 0.7,
        ),
        child: ListView(
          shrinkWrap: true,
          children: [
            _tile(
              ctx,
              id: null,
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
                id: kAutoModel,
                title: '自动',
                // 与 `model_pick.rs` 对得上：挑的是「够用里最便宜的」
                subtitle: '每轮在能干这活的模型里挑最便宜的',
                icon: Icons.auto_awesome_outlined,
                current: current,
              ),
            const Divider(height: 12),
            for (final m in catalog.models)
              _tile(
                ctx,
                id: m.id,
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
  ref.read(selectedModelProvider.notifier).select(picked.id);
}

Widget _tile(
  BuildContext ctx, {
  required String? id,
  required String title,
  required String subtitle,
  required IconData icon,
  required String? current,
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
    trailing: id == current ? const Icon(Icons.check_rounded) : null,
    onTap: () => Navigator.of(ctx).pop((id: id)),
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

/// 设置页里「用哪个模型」那一行：**当前是什么 + 一个更改入口**。
class ModelPickerTile extends ConsumerWidget {
  const ModelPickerTile({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final catalog = ref.watch(modelCatalogProvider);
    final label = ref.watch(modelLabelProvider);

    return catalog.when(
      // ⚠️ **重拉时不要退回转圈。**
      //
      // `cortexApiProvider` 在认证落定时会重建一次，那会让这份列表
      // 重拉。Riverpod 那时给的是「带着上一次结果的 AsyncLoading」，
      // 而朴素的 `.when` 会匹配到 loading —— 表现是每次重拉都闪一下
      // 「正在看这个部署能用哪些…」，而上一次的结果明明还在手里。
      skipLoadingOnReload: true,
      skipLoadingOnRefresh: true,
      loading: () => const ListTile(
        contentPadding: EdgeInsets.zero,
        leading: Icon(Icons.auto_awesome_outlined),
        title: Text('正在看这个部署能用哪些…'),
      ),
      error: (e, _) {
        // 老服务端没有 /llm/models。那不是错误，也不是用户能处理的事 ——
        // 说清楚「这个部署选不了」比一条红色的 404 有用
        if (e is CortexApiException && e.isUnsupported) {
          return ListTile(
            contentPadding: EdgeInsets.zero,
            leading: const Icon(Icons.auto_awesome_outlined),
            title: const Text('这个部署选不了模型'),
            subtitle: Text(
              '服务端版本较早，用的是它自己配的那个。',
              style: theme.textTheme.bodySmall,
            ),
          );
        }
        return ListTile(
          contentPadding: EdgeInsets.zero,
          leading: const Icon(Icons.auto_awesome_outlined),
          title: Text(
            e is CortexApiException ? e.message : '$e',
            style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
          ),
          trailing: TextButton(
            onPressed: () => ref.invalidate(modelCatalogProvider),
            child: const Text('重试'),
          ),
        );
      },
      data: (c) => Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          ListTile(
            contentPadding: EdgeInsets.zero,
            leading: const Icon(Icons.auto_awesome_outlined),
            title: Text(label.text),
            subtitle: Text(
              _summary(c, ref.watch(selectedModelProvider)),
              style: theme.textTheme.bodySmall,
            ),
            trailing: TextButton(
              onPressed: () => showModelPicker(context, ref),
              child: const Text('更改'),
            ),
          ),
          if (label.warning != null)
            Container(
              padding: const EdgeInsets.all(10),
              decoration: BoxDecoration(
                color: scheme.errorContainer.withValues(alpha: 0.4),
                borderRadius: BorderRadius.circular(8),
              ),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Icon(
                    Icons.warning_amber_rounded,
                    size: 16,
                    color: scheme.error,
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      label.warning!,
                      style: theme.textTheme.bodySmall,
                    ),
                  ),
                ],
              ),
            ),
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              '这个部署连的是 ${c.provider} · 共 ${c.models.length} 个可选。'
              '输入框下面也能随时换。',
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
            ),
          ),
        ],
      ),
    );
  }

  /// 摘要那一行说什么，取决于选的是哪一档 —— 三档各自要回答的问题不同。
  static String _summary(ModelCatalog c, String? selected) {
    if (selected == null) {
      return '服务端配的那个，跟着部署走';
    }
    if (selected == kAutoModel) {
      return '每轮在能干这活的模型里挑最便宜的';
    }
    final m = c.byId(selected);
    return m == null ? '这个部署已经没有开放它了' : describeModel(m);
  }
}
