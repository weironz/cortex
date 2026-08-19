/// 输入框底部那个模型开关。
///
/// # 为什么在这儿，而不是只在设置里
///
/// 换模型是**逐轮**的决定，和它左边那两个（工作区、权限档）同一类：
/// 「这一句用哪个」，不是「我这个人的偏好」。埋进设置意味着每次想换都要
/// 开窗、找页、关窗，而真实的用法是问一句便宜的、下一句换个贵的。
///
/// 设置里那份（`ModelPickerTile`）留着 —— 它带价目表和能力说明，是
/// 「我要挑一个」的场景。两处读写的是**同一个** `selectedModelProvider`，
/// 不是各自一份状态：一个功能两份状态是本仓库已经吃过几次亏的形状。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/model_option.dart';
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
        onTap: () => _open(context, ref),
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

  Future<void> _open(BuildContext context, WidgetRef ref) async {
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
                  subtitle: _line(m),
                  icon: Icons.memory_outlined,
                  current: current,
                  // 不支持工具调用的不给选：那样的模型会流畅地回答而
                  // 一个工具都不调，界面上看不出任何异常
                  disabled: m.toolCall == false,
                ),
            ],
          ),
        ),
      ),
    );
    if (picked == null || !context.mounted) return;
    ref.read(selectedModelProvider.notifier).select(picked.id);
  }

  static Widget _tile(
    BuildContext ctx, {
    required String? id,
    required String title,
    required String subtitle,
    required IconData icon,
    required String? current,
    bool disabled = false,
  }) {
    final scheme = Theme.of(ctx).colorScheme;
    return ListTile(
      enabled: !disabled,
      leading: Icon(icon, size: 20),
      title: Text(title),
      subtitle: Text(
        disabled ? '不支持工具调用 —— 选它 agent 就读不了文件、跑不了命令' : subtitle,
        style: disabled ? TextStyle(color: scheme.error) : null,
      ),
      trailing: id == current ? const Icon(Icons.check_rounded) : null,
      onTap: () => Navigator.of(ctx).pop((id: id)),
    );
  }

  static String _line(ModelOption m) {
    if (m.context == null && m.inputMicrosPerMtok == null) {
      return '目录里没有它 —— 能力与价格都不知道';
    }
    final bits = <String>[
      formatContext(m.context),
      '入 ${formatPerMtok(m.inputMicrosPerMtok)}',
    ];
    if (m.vision == true) bits.add('看图');
    if (m.reasoning == true) bits.add('思考');
    return bits.join(' · ');
  }
}
