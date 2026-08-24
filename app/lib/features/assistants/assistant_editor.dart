/// 写一份人设。
///
/// # 为什么是一个对话框，而不是一整页
///
/// 它只有四个字段，而一整页要回答「怎么回去」「没保存要不要拦」。
/// 对话框那两件事是白送的。等到人设里要挂技能、挂知识库时再说。
///
/// # ⚠️ 那句提示不是装饰
///
/// 人设**替换**默认那句「你是 Cortex，一个通用 AI 助理」。不说清的话，
/// 用户会写「另外你还要注意…」这种承接上文的句子 —— 而上文并不存在。
library;

import 'package:flutter/material.dart';
import '../../core/theme.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../models/assistant.dart';
import '../../state/assistant_controller.dart';

/// 打开编辑器。[existing] 为空 = 新建。
Future<void> showAssistantEditor(
  BuildContext context,
  WidgetRef ref,
  Assistant? existing,
) => showDialog<void>(
  context: context,
  builder: (_) => _AssistantEditor(ref: ref, existing: existing),
);

class _AssistantEditor extends StatefulWidget {
  const _AssistantEditor({required this.ref, required this.existing});

  final WidgetRef ref;
  final Assistant? existing;

  @override
  State<_AssistantEditor> createState() => _AssistantEditorState();
}

class _AssistantEditorState extends State<_AssistantEditor> {
  late final _name = TextEditingController(text: widget.existing?.name ?? '');
  late final _icon = TextEditingController(text: widget.existing?.icon ?? '');
  late final _description = TextEditingController(
    text: widget.existing?.description ?? '',
  );

  /// 关掉了哪些工具。**存禁用而不是启用** —— 与服务端同一套语义，
  /// 见 `Assistant.disabledTools`
  late final Set<String> _disabled = {...?widget.existing?.disabledTools};

  late final _instructions = TextEditingController(
    text: widget.existing?.instructions ?? '',
  );

  bool _saving = false;
  String? _error;

  @override
  void dispose() {
    _name.dispose();
    _icon.dispose();
    _description.dispose();
    _instructions.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    if (_name.text.trim().isEmpty) {
      setState(() => _error = '得给它起个名字');
      return;
    }
    setState(() {
      _saving = true;
      _error = null;
    });
    final ctrl = widget.ref.read(assistantControllerProvider.notifier);
    try {
      final existing = widget.existing;
      if (existing == null) {
        await ctrl.create(
          Assistant(
            id: '',
            name: _name.text,
            icon: _icon.text,
            description: _description.text,
            instructions: _instructions.text,
            disabledTools: _disabled.toList(),
          ),
        );
      } else {
        await ctrl.update(
          existing.id,
          name: _name.text,
          icon: _icon.text,
          description: _description.text,
          instructions: _instructions.text,
          disabledTools: _disabled.toList(),
        );
      }
      if (mounted) Navigator.of(context).pop();
    } on Object catch (e) {
      if (!mounted) return;
      // 服务端那句话**原样显示**：它说得比我们能编的具体
      //（「人设最长 20000 个字符，现在是 20001 个」）
      setState(() {
        _saving = false;
        _error = '$e';
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final editing = widget.existing != null;

    return AlertDialog(
      title: Text(editing ? '编辑智能体' : '新建智能体'),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SizedBox(
                    width: 72,
                    child: TextField(
                      key: const ValueKey('assistant:icon'),
                      controller: _icon,
                      maxLength: 4,
                      textAlign: TextAlign.center,
                      buildCounter:
                          (
                            _, {
                            required currentLength,
                            required isFocused,
                            maxLength,
                          }) => null,
                      decoration: const InputDecoration(
                        labelText: '图标',
                        hintText: '🍳',
                      ),
                    ),
                  ),
                  const SizedBox(width: 12),
                  Expanded(
                    child: TextField(
                      key: const ValueKey('assistant:name'),
                      controller: _name,
                      autofocus: true,
                      maxLength: 100,
                      buildCounter:
                          (
                            _, {
                            required currentLength,
                            required isFocused,
                            maxLength,
                          }) => null,
                      decoration: const InputDecoration(
                        labelText: '名字',
                        hintText: '例如：味蕾领航员',
                      ),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 12),
              TextField(
                key: const ValueKey('assistant:description'),
                controller: _description,
                maxLength: 500,
                buildCounter:
                    (
                      _, {
                      required currentLength,
                      required isFocused,
                      maxLength,
                    }) => null,
                decoration: const InputDecoration(
                  labelText: '一句话说明',
                  hintText: '列表里显示。不填就露一段人设本身',
                ),
              ),
              const SizedBox(height: 12),
              TextField(
                key: const ValueKey('assistant:instructions'),
                controller: _instructions,
                minLines: 5,
                maxLines: 12,
                maxLength: 20000,
                buildCounter:
                    (
                      _, {
                      required currentLength,
                      required isFocused,
                      maxLength,
                    }) => null,
                decoration: const InputDecoration(
                  labelText: '人设',
                  alignLabelWithHint: true,
                  hintText: '你是一位精通全球美食、拥有 20 年烹饪经验的资深大厨…',
                ),
              ),
              const SizedBox(height: 16),
              _ToolToggles(
                disabled: _disabled,
                onChanged: (name, on) => setState(() {
                  // 存的是**禁用**清单：打开 = 从清单里去掉
                  if (on) {
                    _disabled.remove(name);
                  } else {
                    _disabled.add(name);
                  }
                }),
              ),
              const SizedBox(height: 8),
              // ⚠️ 这句必须说。不说的话，用户会写「另外你还要注意…」
              // 这种承接上文的句子 —— 而上文并不存在
              Text(
                '这段话会整个取代默认的「你是 Cortex，一个通用 AI 助理」，'
                '所以要从头写起。工具、语言这些不受影响。',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
              if (_error case final e?) ...[
                const SizedBox(height: 10),
                Text(
                  e,
                  key: const ValueKey('assistant:error'),
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
          key: const ValueKey('assistant:save'),
          onPressed: _saving ? null : _save,
          child: Text(_saving ? '保存中…' : '保存'),
        ),
      ],
    );
  }
}

/// 这个智能体能用哪些工具。
///
/// # 为什么这里只列内置的六样，不列 MCP 工具
///
/// MCP 工具是**运行时可变**的（设置页随时增删一台 server），而这份人设
/// 存在数据库里。把当时连着的那些 server 的工具名固化进去，下次那台
/// server 不在了，禁用清单里就躺着一个指向不存在工具的名字 —— 它不报错、
/// 也永远不会被清掉。要给 MCP 工具做开关，判据得是「这台 server 开不开」
/// 而不是「这个工具关不关」，那是另一件事。
///
/// 六样的名字必须与 `cortex_agent::tools::builtin_specs()` 里的**一字不差**
/// —— 服务端按名字剔，拼错的症状是开关拨了但工具照旧在（而且不报错）。
class _ToolToggles extends StatelessWidget {
  const _ToolToggles({required this.disabled, required this.onChanged});

  final Set<String> disabled;
  final void Function(String name, bool on) onChanged;

  /// (工具名, 显示名)。名字见类文档那段 ⚠️。
  static const List<(String, String)> _tools = [
    ('read_file', '读文件'),
    ('write_file', '写文件'),
    ('edit_file', '改文件'),
    ('shell', '执行命令'),
    ('tree', '看目录结构'),
    ('generate_image', '画图'),
  ];

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('能用的工具', style: theme.textTheme.titleSmall),
        const SizedBox(height: 2),
        Text(
          // 说清关掉之后会发生什么 —— 一个只写「工具开关」的标题
          // 会让人以为关掉只是"不推荐用"
          '关掉的工具不会进这个智能体的工具目录 —— 模型根本看不见它，'
          '也就不会答应去做那件事。',
          style: theme.textTheme.labelSmall?.copyWith(
            color: theme.cortex.foregroundTertiary,
            height: 1.5,
          ),
        ),
        const SizedBox(height: 8),
        Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            for (final (name, label) in _tools)
              FilterChip(
                key: ValueKey('assistant:tool:$name'),
                label: Text(label),
                selected: !disabled.contains(name),
                onSelected: (on) => onChanged(name, on),
              ),
          ],
        ),
      ],
    );
  }
}
