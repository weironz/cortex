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
          ),
        );
      } else {
        await ctrl.update(
          existing.id,
          name: _name.text,
          icon: _icon.text,
          description: _description.text,
          instructions: _instructions.text,
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
