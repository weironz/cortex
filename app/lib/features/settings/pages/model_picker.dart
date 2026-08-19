import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../models/model_option.dart';
import '../../../state/model_controller.dart';

/// 选对话模型。
///
/// # 在它之前，用户根本选不了
///
/// 模型由服务端的 `CORTEX_LLM_MODEL` 环境变量定死，一个部署一个模型。
/// 界面上「模型」这一页只有「自己的 API key」和「本机模型」两项 ——
/// 都不是「用哪个模型」。
///
/// # 三件这一页必须说实话的事
///
/// 1. **不支持工具调用的模型要拦住。** 那样的模型跑 agent 会流畅地回答
///    而一个工具都不调，用户看不出哪里不对，只会觉得它「不听话」。
/// 2. **「不知道」与「不行」是两回事。** 服务端目录里查不到的模型，三个
///    能力字段都是 null。把它当成不行会把一个能用的模型挡在外面；当成行
///    又会让人踩上面那个坑。所以单独说一句「不知道」。
/// 3. **自动档只能说它真的在做的事。** 它挑的是「在能干这活的模型里最
///    便宜的」，不是「最优的」—— 我们没有任何办法知道哪个模型对某个具体
///    问题答得更好。写成「智能匹配最优模型」是编的。
class ModelPickerTile extends ConsumerWidget {
  const ModelPickerTile({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final catalog = ref.watch(modelCatalogProvider);
    final selected = ref.watch(selectedModelProvider);

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
        title: Text('对话模型'),
        subtitle: Text('正在看这个部署能用哪些…'),
      ),
      error: (e, _) {
        // 老服务端没有 /llm/models。那不是错误，也不是用户能处理的事 ——
        // 说清楚「这个部署选不了」比一条红色的 404 有用
        if (e is CortexApiException && e.isUnsupported) {
          return const ListTile(
            contentPadding: EdgeInsets.zero,
            title: Text('对话模型'),
            subtitle: Text('这个部署不支持切换模型（服务端版本较早），用的是它自己配的那个。'),
          );
        }
        return ListTile(
          contentPadding: EdgeInsets.zero,
          title: const Text('对话模型'),
          subtitle: Text(
            e is CortexApiException ? e.message : '$e',
            style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
          ),
          trailing: TextButton(
            onPressed: () => ref.invalidate(modelCatalogProvider),
            child: const Text('重试'),
          ),
        );
      },
      data: (c) => _Picker(catalog: c, selected: selected),
    );
  }
}

class _Picker extends ConsumerWidget {
  const _Picker({required this.catalog, required this.selected});

  final ModelCatalog catalog;
  final String? selected;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final label = ref.watch(modelLabelProvider);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        ListTile(
          contentPadding: EdgeInsets.zero,
          title: const Text('对话模型'),
          subtitle: Text(
            '这个部署连的是 ${catalog.provider}',
            style: theme.textTheme.bodySmall,
          ),
        ),
        const SizedBox(height: 4),
        // 「跟随部署」永远在最前：绝大多数人不该动这个设置，
        // 而把默认项摆在第一个是让他们一眼确认「我没改过」
        _Row(
          title: '跟随部署',
          subtitle: catalog.defaultModel.isEmpty
              ? '服务端配的那个'
              : '服务端配的是 ${catalog.defaultModel}',
          selected: selected == null,
          onTap: () => ref.read(selectedModelProvider.notifier).select(null),
        ),
        if (catalog.autoAvailable)
          _Row(
            title: '自动',
            // ⚠️ 这句话与 `model_pick.rs` 的实现必须对得上。
            // 它挑的是「够用里最便宜的」，不是「最优的」
            subtitle: '每一轮在能干这活的模型里挑最便宜的（看要不要工具、有没有图、装不装得下）',
            selected: selected == kAutoModel,
            onTap: () =>
                ref.read(selectedModelProvider.notifier).select(kAutoModel),
          ),
        const Divider(height: 20),
        ...catalog.models.map(
          (m) => _Row(
            title: m.displayName,
            subtitle: _describe(m),
            selected: selected == m.id,
            // **不支持工具调用的不给选。**
            //
            // 不是灰掉就完事：tooltip 要说清为什么，否则用户只会觉得
            // 「这个选项坏了」。而放它进去的后果比挡住严重得多 ——
            // agent 会照常回答，工具一个都不调，看不出任何异常
            disabledReason: m.toolCall == false
                ? '它不支持工具调用：agent 会照常回答，但读不了文件、跑不了命令，而界面上看不出区别'
                : null,
            warning: m.toolCall == null ? '服务端的目录里没有它，所以不知道它支不支持工具调用' : null,
            onTap: () => ref.read(selectedModelProvider.notifier).select(m.id),
          ),
        ),
        if (label.warning != null) ...[
          const SizedBox(height: 8),
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
                  child: Text(label.warning!, style: theme.textTheme.bodySmall),
                ),
              ],
            ),
          ),
        ],
        const SizedBox(height: 8),
        Text(
          '价格是每百万 token 的美元数，来自服务端内置的模型目录。'
          '这里不折算成人民币 —— 折算要汇率，而一个随手挑的汇率折出来的价格，'
          '你没法判断它对不对。',
          style: theme.textTheme.labelSmall?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
      ],
    );
  }

  /// 一个模型的一行说明。**查不到就说查不到**，不编。
  static String _describe(ModelOption m) {
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
}

class _Row extends StatelessWidget {
  const _Row({
    required this.title,
    required this.subtitle,
    required this.selected,
    required this.onTap,
    this.disabledReason,
    this.warning,
  });

  final String title;
  final String subtitle;
  final bool selected;
  final VoidCallback onTap;

  /// 非空 = 不给选，且这句话说明为什么。
  final String? disabledReason;

  /// 非空 = 能选，但要提醒。
  final String? warning;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final disabled = disabledReason != null;

    final row = Opacity(
      opacity: disabled ? 0.5 : 1,
      child: Container(
        margin: const EdgeInsets.only(bottom: 4),
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
        decoration: BoxDecoration(
          color: selected ? scheme.primary.withValues(alpha: 0.10) : null,
          borderRadius: BorderRadius.circular(8),
          border: Border.all(
            color: selected ? scheme.primary : scheme.outlineVariant,
          ),
        ),
        child: Row(
          children: [
            Icon(
              selected
                  ? Icons.radio_button_checked
                  : Icons.radio_button_unchecked,
              size: 16,
              color: selected ? scheme.primary : scheme.onSurfaceVariant,
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(title, style: theme.textTheme.bodyMedium),
                  Text(
                    subtitle,
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                  if (warning != null)
                    Text(
                      warning!,
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: scheme.tertiary,
                      ),
                    ),
                  if (disabledReason != null)
                    Text(
                      disabledReason!,
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: scheme.error,
                      ),
                    ),
                ],
              ),
            ),
          ],
        ),
      ),
    );

    if (disabled) {
      // 灰掉但**保留 tooltip** —— 不给理由的禁用项只会让人觉得功能坏了
      return Tooltip(message: disabledReason!, child: row);
    }
    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(8),
      child: row,
    );
  }
}
