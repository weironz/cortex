/// 输入框底部那个模型开关。
///
/// # 为什么在这儿，而不是只在设置里
///
/// 换模型是**逐轮**的决定，和它左边那两个（工作区、权限档）同一类：
/// 「这一句用哪个」，不是「我这个人的偏好」。埋进设置意味着每次想换都要
/// 开窗、找页、关窗，而真实的用法是问一句便宜的、下一句换个贵的。
///
/// 设置里那份（`ModelPickerTile`）留着 —— 它是「我要看清楚再挑」的场景。
/// 两处**弹的是同一个面板**（`showModelPicker`）、读写**同一个**
/// `selectedModelProvider`：一个功能两份实现是本仓库已经吃过几次亏的形状。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../features/settings/pages/model_picker.dart';
import '../../../state/model_controller.dart';

class ModelChip extends ConsumerWidget {
  const ModelChip({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final label = ref.watch(modelLabelProvider);

    // 这个部署没有 /llm/models（老服务端）—— 一个点开只会说「选不了」的
    // chip 是纯噪音，直接不占位置。设置页那份会解释原因
    if (label.unsupported) return const SizedBox.shrink();

    final warn = label.warning != null;
    final color = warn ? scheme.error : scheme.onSurfaceVariant;

    return Tooltip(
      message: label.warning ?? '这一轮用哪个模型',
      child: InkWell(
        borderRadius: BorderRadius.circular(6),
        onTap: () => showModelPicker(context, ref),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 3),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                warn
                    ? Icons.warning_amber_rounded
                    : Icons.auto_awesome_outlined,
                size: 13,
                color: color,
              ),
              const SizedBox(width: 4),
              // 型号名可以很长（`claude-sonnet-4-5-20250929`），
              // 不封顶的话它会把工作区那个 chip 挤出可视区
              ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 132),
                child: Text(
                  label.text,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: color,
                    fontWeight: warn ? FontWeight.w700 : null,
                  ),
                ),
              ),
              Icon(Icons.arrow_drop_up_rounded, size: 15, color: color),
            ],
          ),
        ),
      ),
    );
  }
}
