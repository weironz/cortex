/// 「默认模型」—— 把角色指派给某条来源上的某个型号。
///
/// # 它填的是中间那一层
///
/// 在它之前只有两头：部署配的那个（服务端环境变量），和撰写框下面那个
/// **逐轮**的选择。「我这个账号平时默认用哪个」没有地方存。
///
/// 逐轮选择替代不了它 —— 那是「这一句用哪个」，关掉窗口就该忘。而
/// 「快速模型」那一档（会话命名、内容抽取）**根本不经过用户**：
/// 它此前只能用部署配的那个，一个自带 key 的人没有任何办法让这些调用
/// 走自己的账户。
///
/// # 为什么每一行都复用 `pickModel`，而不是三个下拉框
///
/// 下拉框摆不下这些东西：型号属于哪条来源、占不占配额、支不支持工具调用、
/// 目录里查不查得到。选择器上那些提示是逐条踩出来的，另写一份的下场是
/// 「设置页里能选的，撰写框里选了会被拦」。
///
/// # 绘画那一行与另外两行的规则不一样
///
/// 生图模型基本都**不支持工具调用**。照搬另外两行的规则（不支持工具调用
/// 的画成灰的）会把它该提供的每一项都挡住 —— 于是这一行开
/// `requireTools: false`，同时用 `imageOutput` 筛掉画不出来的。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../models/model_option.dart';
import '../../../models/model_role.dart';
import '../../../state/app_providers.dart';
import '../../../state/model_controller.dart';
import 'model_picker.dart';

/// 现在指派了什么（`GET /settings/model-roles`）。
final modelRolesProvider = FutureProvider.autoDispose<RoleAssignments>(
  (ref) => ref.watch(cortexApiProvider).modelRoles(),
  // 与模型目录同一个判据：没有这条路（老服务端）不是故障，
  // 重试永远不会成功
  retry: (count, error) {
    if (error is CortexApiException && error.isUnsupported) return null;
    final secs = [1, 3, 8, 20, 30];
    return Duration(seconds: secs[count.clamp(0, secs.length - 1)]);
  },
);

class ModelRolesPage extends ConsumerStatefulWidget {
  const ModelRolesPage({super.key});

  @override
  ConsumerState<ModelRolesPage> createState() => _ModelRolesPageState();
}

class _ModelRolesPageState extends ConsumerState<ModelRolesPage> {
  /// 正在存哪个角色。存的时候把那一行禁掉 —— 连点两下会发两个整份替换，
  /// 而后到的那个可能带着旧的三行。
  ModelRole? _saving;
  String? _error;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final roles = ref.watch(modelRolesProvider);
    final catalog = ref.watch(modelCatalogProvider);

    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        Text('默认模型', style: theme.textTheme.titleMedium),
        const SizedBox(height: 4),
        Text(
          '每种活儿默认用哪个模型。撰写框下面选的那个只管当下这一句，'
          '这里配的是「不选的时候用哪个」。',
          style: theme.textTheme.bodySmall?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
        const SizedBox(height: 16),

        if (roles.hasError)
          _notice(
            theme,
            roles.error is CortexApiException &&
                    (roles.error as CortexApiException).isUnsupported
                ? '这个后端还没有「默认模型」这条路 —— 它比这个客户端旧。'
                      '每种活儿仍然按部署配的那个走。'
                : '读不出现在的指派：${roles.error}',
            error: true,
          )
        else ...[
          for (final role in ModelRole.values) ...[
            _RoleRow(
              role: role,
              // ⚠️ 重拉时不要退回空白：`roles.value` 在 loading 阶段仍然
              // 带着上一次的结果，用它画的话不会闪一下「没指派」
              assigned: roles.value?.of(role),
              catalog: catalog.value,
              busy: _saving == role,
              onTap: () => _assign(role),
              onClear: roles.value?.of(role) == null
                  ? null
                  : () => _save(role, null),
            ),
            const Divider(height: 8),
          ],
        ],

        if (_error != null) ...[
          const SizedBox(height: 12),
          _notice(theme, _error!, error: true),
        ],

        const SizedBox(height: 20),
        Text(
          '不指派也能用：主模型走部署配的那个，快速模型走部署配的那个，'
          '绘画模型自动挑一个最便宜的能生图的。',
          style: theme.textTheme.labelSmall?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
      ],
    );
  }

  Widget _notice(ThemeData theme, String text, {bool error = false}) =>
      Container(
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: error
              ? theme.colorScheme.errorContainer
              : theme.colorScheme.surfaceContainerHighest,
          borderRadius: BorderRadius.circular(8),
        ),
        child: Text(
          text,
          style: theme.textTheme.bodySmall?.copyWith(
            color: error
                ? theme.colorScheme.onErrorContainer
                : theme.colorScheme.onSurfaceVariant,
          ),
        ),
      );

  /// 给某个角色挑一个。
  Future<void> _assign(ModelRole role) async {
    final current = ref.read(modelRolesProvider).value?.of(role);
    final picked = await pickModel(
      context,
      ref,
      current: current == null
          ? const ModelPick()
          : ModelPick(source: current.source, model: current.model),
      headline: '${role.label} · ${role.hint}',
      firstTitle: '不指派',
      firstSubtitle: switch (role) {
        ModelRole.main || ModelRole.cheap => '按部署配的那个走',
        ModelRole.image => '自动挑一个最便宜的能生图的',
      },
      // 角色存的是一对 `(来源, 型号)`，而「自动」不是任何一条来源上的
      // 型号 —— 摆出来选了也存不进去
      allowAuto: false,
      // 绘画那一行两条规则都要改，理由见文件头
      where: role == ModelRole.image ? _canDraw : null,
      requireTools: role != ModelRole.image,
    );
    if (picked == null || !mounted) return;

    final p = picked.pick;
    await _save(
      role,
      p.model == null || p.source == null
          ? null
          : RoleAssignment(role: role, source: p.source!, model: p.model!),
    );
  }

  /// 存一条（`null` = 清掉这个角色）。
  ///
  /// **整份发**：服务端那侧是整份替换，所以这里要把另外两行原样带上 ——
  /// 只发改的那一条会把其余两个角色清掉。
  Future<void> _save(ModelRole role, RoleAssignment? assignment) async {
    final before =
        ref.read(modelRolesProvider).value ?? const RoleAssignments();
    setState(() {
      _saving = role;
      _error = null;
    });
    try {
      final saved = await ref
          .read(cortexApiProvider)
          .saveModelRoles(before.with_(role, assignment));
      if (!mounted) return;
      // 用服务端回的那一份覆盖，而不是本地拼的：服务端可能规范化过
      // （比如认不出的角色被丢掉），显示本地那份会与下次打开时不一样
      ref.invalidate(modelRolesProvider);
      // 主模型换了，撰写框上那个「跟随部署」显示的名字也跟着变 ——
      // 那份名字来自模型目录，重拉一次
      if (saved.of(ModelRole.main)?.model != before.of(ModelRole.main)?.model) {
        ref.invalidate(modelCatalogProvider);
      }
    } on CortexApiException catch (e) {
      // 服务端的校验消息**原样显示**：它说得比我们能编的具体
      //（「这条来源没有开放 x，能用的是：…」）
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _saving = null);
    }
  }
}

/// 画得出图的。
bool _canDraw(ModelOption m) => m.imageOutput == true;

class _RoleRow extends StatelessWidget {
  const _RoleRow({
    required this.role,
    required this.assigned,
    required this.catalog,
    required this.busy,
    required this.onTap,
    required this.onClear,
  });

  final ModelRole role;
  final RoleAssignment? assigned;
  final ModelCatalog? catalog;
  final bool busy;
  final VoidCallback onTap;
  final VoidCallback? onClear;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    // 指派的那个还在不在。**不在要说出来** —— 那条来源被删了或那个型号
    // 被下掉了，下一轮会静默回落，而这一行还显示得好好的
    final option = assigned == null
        ? null
        : catalog?.pick(assigned!.source, assigned!.model);
    final gone = assigned != null && catalog != null && option == null;

    return ListTile(
      contentPadding: EdgeInsets.zero,
      enabled: !busy,
      title: Text(role.label, style: theme.textTheme.bodyMedium),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            role.hint,
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
          if (gone)
            Text(
              '指派的「${assigned!.model}」已经不在可选列表里了（那条来源被删了或关了）—— '
              '现在实际走的是默认那个。换一个吧。',
              style: theme.textTheme.labelSmall?.copyWith(color: scheme.error),
            ),
        ],
      ),
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (busy)
            const SizedBox(
              width: 14,
              height: 14,
              child: CircularProgressIndicator(strokeWidth: 2),
            )
          else ...[
            Flexible(
              child: Text(
                assigned == null
                    ? _unassignedLabel()
                    : (option?.displayName ?? assigned!.model),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: assigned == null ? scheme.onSurfaceVariant : null,
                ),
              ),
            ),
            if (onClear != null)
              IconButton(
                onPressed: onClear,
                icon: const Icon(Icons.close_rounded, size: 16),
                tooltip: '不指派',
                visualDensity: VisualDensity.compact,
              ),
            const Icon(Icons.chevron_right_rounded, size: 18),
          ],
        ],
      ),
      onTap: busy ? null : onTap,
    );
  }

  /// 没指派时那一栏显示什么。
  ///
  /// **说的是实际会用哪个，不是「未设置」**：三个角色不指派时各走各的
  /// 回落，而「未设置」三个字读不出这一点。
  String _unassignedLabel() {
    if (role == ModelRole.image) return '自动挑最便宜的';
    final c = catalog;
    if (c == null) return '跟随部署';
    final m = role == ModelRole.main ? c.defaultModel : c.cheapModel;
    return m.isEmpty ? '跟随部署' : '跟随部署 · $m';
  }
}
