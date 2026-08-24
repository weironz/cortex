/// 技能这一页 —— 写下「这件事怎么做」，模型要用时自己取。
///
/// # 两层，而这一页必须把这件事讲出来
///
/// 用户在这里填两样东西，而它们**进上下文的时机完全不同**：
///
/// * **说明**：每一轮都在提示词里。模型就靠它决定要不要取正文。
/// * **正文**：只在模型调了 `load_skill` 之后才进去。
///
/// 不讲的话，最常见的写法是「说明」留空、正文写满 —— 那样模型永远不知道
/// 这条技能是干什么的，于是**永远不取**。而这次失败没有任何征兆：
/// 没有报错，技能就是不生效。所以说明那一栏的提示语是这一页最重要的一行字。
///
/// # 为什么是列表 + 编辑，而不是卡片墙
///
/// 与智能体那一页刻意不同。人设是「挑一个用」，所以卡片墙（要看清楚、
/// 要一眼比较）；技能是「开着就都在」，用户在这里做的事是**扫一眼哪些开着**
/// 并偶尔改一条 —— 一列带开关的行比一墙卡片快得多。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../models/skill.dart';
import '../../../state/skill_controller.dart';
import '../widgets/settings_layout.dart';

class SkillsPage extends ConsumerWidget {
  const SkillsPage({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final state = ref.watch(skillControllerProvider);

    if (state.unsupported) {
      return _Note(
        icon: Icons.auto_stories_outlined,
        title: '这个后端没有技能',
        // 说清是**后端**旧了，不是出错了 —— 重试永远不会成功
        body: '它是一个老版本的部署，还没有 /skills 这条路。升级之后这里会自己出现。',
      );
    }
    if (state.error != null) {
      return _Note(
        icon: Icons.cloud_off_rounded,
        title: '拉不到技能',
        body: '${state.error}',
        action: OutlinedButton.icon(
          onPressed: () => ref.read(skillControllerProvider.notifier).refresh(),
          icon: const Icon(Icons.refresh_rounded, size: 16),
          label: const Text('重试'),
        ),
      );
    }
    if (state.loading && state.skills.isEmpty) {
      return const Center(
        child: SizedBox(
          width: 20,
          height: 20,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    }

    return ListView(
      padding: const EdgeInsets.fromLTRB(4, 4, 4, 24),
      children: [
        SettingsSection(
          // 不传节头：内容区大标题已经写着「技能」，节头再抄一遍页名是
          // 口吃（见 SettingsSection.title 的注释）—— 单节页只留说明与动作
          // ⚠️ 这两句是这一页最重要的一行字：不讲清两层，用户会把说明留空，
          // 于是模型永远不知道该不该取正文 —— 而那次失败没有任何征兆
          description:
              '一份写好的做法。每一轮只有「名字 + 一句话说明」进模型的上下文，'
              '正文要等它判断用得上、自己取回来 —— 所以那句说明写得越准，它越取得对。',
          trailing: TextButton.icon(
            key: const ValueKey('skills:new'),
            onPressed: () => showSkillEditor(context, ref, null),
            icon: const Icon(Icons.add_rounded, size: 16),
            label: const Text('新建'),
          ),
          children: [
            if (state.skills.isEmpty)
              SettingsCard(
                child: Text(
                  '还没有技能。写一条试试 —— 比如「周报」：说明写「按公司模板写周报」，'
                  '正文写模板本身。以后说一句「写周报」就够了。',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
                  ),
                ),
              )
            else
              SettingsCard(
                padding: EdgeInsets.zero,
                // ⚠️ `ListTile` 的水波纹画在**最近的 Material 上**，而
                // `SettingsCard` 是一个带背景的 `DecoratedBox` —— 它会把水波纹
                // 整个盖住。framework 会为此抛断言（widget 测试里直接红），
                // 而在真机上表现为「这一行点下去毫无反应」
                child: Material(
                  type: MaterialType.transparency,
                  child: Column(
                    children: [
                      for (final (i, s) in state.skills.indexed) ...[
                        if (i > 0) const Divider(height: 1),
                        _SkillRow(skill: s),
                      ],
                    ],
                  ),
                ),
              ),
          ],
        ),
      ],
    );
  }
}

class _SkillRow extends ConsumerWidget {
  const _SkillRow({required this.skill});

  final Skill skill;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final messenger = ScaffoldMessenger.maybeOf(context);
    // 关掉的整行压暗：它在提示词里根本不存在，长得和开着的一样是在骗人
    final dim = !skill.enabled;

    final tile = ListTile(
      key: ValueKey('skill:${skill.id}'),
      onTap: () => showSkillEditor(context, ref, skill),
      contentPadding: const EdgeInsets.fromLTRB(14, 2, 8, 2),
      title: Text(
        skill.name,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
        style: theme.textTheme.bodyMedium?.copyWith(
          color: dim ? theme.cortex.foregroundTertiary : null,
        ),
      ),
      subtitle: Text(
        // 没写说明时**明说这件事有后果**，而不是留一片空白：
        // 空白读起来像「可填可不填」，而它决定这条技能会不会被取用
        skill.description.isNotEmpty
            ? skill.description
            : '没写说明 —— 模型不知道什么时候该用它',
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
        style: theme.textTheme.labelSmall?.copyWith(
          color: skill.description.isEmpty
              ? theme.colorScheme.error
              : theme.cortex.foregroundTertiary,
        ),
      ),
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Switch(
            key: ValueKey('skill:toggle:${skill.id}'),
            value: skill.enabled,
            onChanged: (v) async {
              final err = await ref
                  .read(skillControllerProvider.notifier)
                  .setEnabled(skill.id, v);
              if (err != null) {
                messenger?.showSnackBar(SnackBar(content: Text(err)));
              }
            },
          ),
          IconButton(
            key: ValueKey('skill:delete:${skill.id}'),
            tooltip: '删除「${skill.name}」',
            iconSize: 17,
            icon: const Icon(Icons.delete_outline_rounded),
            onPressed: () => _confirmDelete(context, ref, skill),
          ),
        ],
      ),
    );
    // **整行** 0.45，不只是标题变灰（设计稿的数）：标题灰、开关和图标
    // 照常鲜亮的行，扫一眼仍像开着的。Opacity 不挡命中 —— 开关照样能点
    return dim ? Opacity(opacity: 0.45, child: tile) : tile;
  }

  Future<void> _confirmDelete(
    BuildContext context,
    WidgetRef ref,
    Skill s,
  ) async {
    final messenger = ScaffoldMessenger.maybeOf(context);
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('删除「${s.name}」？'),
        // 说清历史不会变 —— 用户在这里真正怕的是「用过它的那些对话会不会坏」
        content: const Text('用过它的那些对话一条都不会变。只是从下一轮起，模型不再知道它存在。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            // 不用默认的 primary：那会让「删除」与编辑器里的「保存」同款 ——
            // 确认框里肌肉记忆点的正是右下角那颗主按钮。危险动作要长得
            // 危险，把 error 配色画在按钮自己身上，而不是只靠标题那个问号
            style: FilledButton.styleFrom(
              backgroundColor: Theme.of(ctx).colorScheme.error,
              foregroundColor: Theme.of(ctx).colorScheme.onError,
            ),
            onPressed: () => Navigator.of(ctx).pop(true),
            child: const Text('删除'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    try {
      await ref.read(skillControllerProvider.notifier).remove(s.id);
    } on Object catch (e) {
      messenger?.showSnackBar(SnackBar(content: Text('$e')));
    }
  }
}

/// 一整页只说一句话时用的那块。
class _Note extends StatelessWidget {
  const _Note({
    required this.icon,
    required this.title,
    required this.body,
    this.action,
  });

  final IconData icon;
  final String title;
  final String body;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 380),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 30, color: theme.cortex.foregroundTertiary),
            const SizedBox(height: 12),
            Text(title, style: theme.textTheme.titleSmall),
            const SizedBox(height: 6),
            Text(
              body,
              textAlign: TextAlign.center,
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
            if (action != null) ...[const SizedBox(height: 14), action!],
          ],
        ),
      ),
    );
  }
}

// ───────────────────────── 编辑器 ─────────────────────────

/// 打开编辑器。[existing] 为空 = 新建。
Future<void> showSkillEditor(
  BuildContext context,
  WidgetRef ref,
  Skill? existing,
) => showDialog<void>(
  context: context,
  builder: (_) => _SkillEditor(ref: ref, existing: existing),
);

class _SkillEditor extends StatefulWidget {
  const _SkillEditor({required this.ref, required this.existing});

  final WidgetRef ref;
  final Skill? existing;

  @override
  State<_SkillEditor> createState() => _SkillEditorState();
}

class _SkillEditorState extends State<_SkillEditor> {
  late final _name = TextEditingController(text: widget.existing?.name ?? '');
  late final _description = TextEditingController(
    text: widget.existing?.description ?? '',
  );
  late final _instructions = TextEditingController(
    text: widget.existing?.instructions ?? '',
  );

  bool _saving = false;
  String? _error;

  @override
  void dispose() {
    _name.dispose();
    _description.dispose();
    _instructions.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    if (_name.text.trim().isEmpty) {
      setState(() => _error = '得给它起个名字 —— 模型就是靠名字取正文的');
      return;
    }
    setState(() {
      _saving = true;
      _error = null;
    });
    final ctrl = widget.ref.read(skillControllerProvider.notifier);
    try {
      final existing = widget.existing;
      if (existing == null) {
        await ctrl.create(
          Skill(
            id: '',
            name: _name.text,
            description: _description.text,
            instructions: _instructions.text,
          ),
        );
      } else {
        await ctrl.update(
          existing.id,
          name: _name.text,
          description: _description.text,
          instructions: _instructions.text,
        );
      }
      if (mounted) Navigator.of(context).pop();
    } on Object catch (e) {
      if (!mounted) return;
      // 服务端那句话**原样显示**：重名那句（「已经有一个叫 X 的技能了」）
      // 比我们能编的具体得多
      setState(() {
        _saving = false;
        _error = '$e';
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return AlertDialog(
      title: Text(widget.existing != null ? '编辑技能' : '新建技能'),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 560),
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              TextField(
                key: const ValueKey('skill:name'),
                controller: _name,
                autofocus: true,
                maxLength: 100,
                buildCounter: _noCounter,
                decoration: const InputDecoration(
                  labelText: '名字',
                  // 说清它是标识符：重名会被拒，而用户得先知道为什么
                  helperText: '模型用它取正文，所以得唯一。例如：周报',
                  hintText: '周报',
                ),
              ),
              const SizedBox(height: 14),
              TextField(
                key: const ValueKey('skill:description'),
                controller: _description,
                maxLength: 500,
                buildCounter: _noCounter,
                maxLines: 2,
                minLines: 1,
                decoration: const InputDecoration(
                  labelText: '一句话说明',
                  // ⚠️ 这一栏是整个功能的成败：留空的话模型永远不取正文，
                  // 而那次失败没有任何征兆
                  helperText: '每一轮都在模型眼前。它就靠这句判断要不要把下面的正文取回来 —— 写具体点',
                  helperMaxLines: 3,
                  hintText: '按公司模板写周报：本周进展 / 下周计划 / 风险',
                ),
              ),
              const SizedBox(height: 14),
              TextField(
                key: const ValueKey('skill:instructions'),
                controller: _instructions,
                minLines: 6,
                maxLines: 14,
                maxLength: 100000,
                buildCounter: _noCounter,
                decoration: const InputDecoration(
                  labelText: '正文',
                  alignLabelWithHint: true,
                  helperText: '真正的做法。只在模型取回它之后才进上下文 —— 所以可以写长',
                  hintText: '写周报时按三段来：\n1. 本周进展 …',
                ),
              ),
              if (_error case final e?) ...[
                const SizedBox(height: 10),
                Text(
                  e,
                  key: const ValueKey('skill:error'),
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.error,
                  ),
                ),
              ],
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: _saving ? null : () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          key: const ValueKey('skill:save'),
          onPressed: _saving ? null : _save,
          child: Text(_saving ? '保存中…' : '保存'),
        ),
      ],
    );
  }
}

/// 不画那个「12/100」计数器：三个框都有上限，三行计数器只是噪音。
Widget? _noCounter(
  BuildContext _, {
  required int currentLength,
  required bool isFocused,
  int? maxLength,
}) => null;
