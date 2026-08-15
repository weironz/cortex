import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../models/mcp.dart';
import '../../../state/app_providers.dart';

/// 粘贴添加。**两步：解析 → 确认。**
///
/// # 为什么不能一步装上
///
/// 加一台 MCP server = 在这台机器上跑任意进程。一次点击就装上的话，
/// 用户同意的是一个名字（「filesystem」），而不是一条命令
/// （`npx -y @某人/某包 C:/`）。中间那一屏原样显示将要执行的命令行，
/// 是这件事唯一的知情环节。
///
/// 解析在**服务端**做（`POST /local/mcp/parse`，不落盘）：引号、scope 包名
/// 这些边界情况有单测，而客户端再实现一遍必然与它漂开 —— 症状是预览的和
/// 装上的不是同一个东西。
class McpAddPanel extends ConsumerStatefulWidget {
  const McpAddPanel({
    required this.existing,
    required this.onDone,
    this.initialText,
    super.key,
  });

  /// 已经有的那些名字。只用来在本地也提示一次重名 —— 服务端也会说。
  final List<String> existing;
  final VoidCallback onDone;

  /// 从注册表跳过来时带的那一段。
  final String? initialText;

  @override
  ConsumerState<McpAddPanel> createState() => _McpAddPanelState();
}

class _McpAddPanelState extends ConsumerState<McpAddPanel> {
  late final _text = TextEditingController(text: widget.initialText ?? '');
  final _name = TextEditingController();

  List<McpParsedServer>? _parsed;
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _text.dispose();
    _name.dispose();
    super.dispose();
  }

  Future<void> _parse() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final got = await ref.read(cortexApiProvider).parseMcpPaste(_text.text);
      if (!mounted) return;
      setState(() {
        _parsed = got;
        // 单台时名字可改；多台时按解析出来的原样用（改哪一个都说不清）
        if (got.length == 1) _name.text = got.first.name;
      });
    } on CortexApiException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _install() async {
    final parsed = _parsed;
    if (parsed == null || parsed.isEmpty) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    final api = ref.read(cortexApiProvider);
    try {
      for (final p in parsed) {
        // 名字只有单台时才让改 —— 多台时哪一个对应输入框里那个说不清
        final name = parsed.length == 1 && _name.text.trim().isNotEmpty
            ? _name.text.trim()
            : p.name;
        // config **原样发回去**，不在客户端重拼：那等于把服务端刚做过的
        // 解析再做一遍，而两份不一致就是「预览的和装上的不是一个东西」
        await api.saveMcpServer(name: name, config: p.config);
      }
      ref.invalidate(mcpConfigProvider);
      if (mounted) widget.onDone();
    } on CortexApiException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final parsed = _parsed;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
            onPressed: widget.onDone,
            icon: const Icon(Icons.arrow_back_rounded, size: 18),
            label: const Text('返回列表'),
          ),
        ),
        const SizedBox(height: 4),
        Expanded(
          child: ListView(
            children: [
              Text('添加 MCP 服务器', style: theme.textTheme.titleMedium),
              const SizedBox(height: 4),
              Text(
                '从 README 或别的 agent 的配置里复制一段粘进来 —— '
                '一条 npx 命令、一个 https 地址、或者一整块 mcpServers JSON 都行。',
                style: theme.textTheme.bodySmall,
              ),
              const SizedBox(height: 12),
              TextField(
                controller: _text,
                minLines: 4,
                maxLines: 8,
                style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
                decoration: const InputDecoration(
                  border: OutlineInputBorder(),
                  hintText:
                      'npx -y @modelcontextprotocol/server-filesystem C:/work\n'
                      '或 https://example.com/mcp\n'
                      '或 {"mcpServers": {…}}',
                  hintStyle: TextStyle(fontFamily: 'monospace', fontSize: 12),
                ),
                onChanged: (_) {
                  // 改了文本就作废上一次的预览：留着的话用户会对着
                  // 「A 的预览」点确认，装上的是 A，而输入框里写的是 B
                  if (_parsed != null) setState(() => _parsed = null);
                },
              ),
              const SizedBox(height: 8),
              Align(
                alignment: Alignment.centerRight,
                child: FilledButton(
                  onPressed: _busy || _text.text.trim().isEmpty ? null : _parse,
                  child: const Text('看看会装什么'),
                ),
              ),
              if (_error != null) ...[
                const SizedBox(height: 8),
                Text(
                  _error!,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.error,
                  ),
                ),
              ],
              if (parsed != null) ...[
                const Divider(height: 28),
                Text('将要添加', style: theme.textTheme.titleSmall),
                const SizedBox(height: 8),
                if (parsed.length == 1)
                  TextField(
                    controller: _name,
                    decoration: InputDecoration(
                      labelText: '名字',
                      helperText: '工具会叫 mcp__${_name.text}__xxx',
                      errorText: widget.existing.contains(_name.text.trim())
                          ? '已经有同名的了，装上会覆盖它'
                          : null,
                    ),
                    onChanged: (_) => setState(() {}),
                  ),
                const SizedBox(height: 10),
                for (final p in parsed) _Preview(parsed: p),
                const SizedBox(height: 12),
                // 这一段是这一屏存在的理由：他同意的是下面那条命令
                Container(
                  padding: const EdgeInsets.all(10),
                  decoration: BoxDecoration(
                    color: theme.colorScheme.surfaceContainerHighest,
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      const Icon(Icons.info_outline_rounded, size: 16),
                      const SizedBox(width: 8),
                      Expanded(
                        child: Text(
                          '装上之后，上面那条命令会在你这台机器上运行。'
                          '它提供的每个工具默认都要你逐次点同意。',
                          style: theme.textTheme.labelSmall,
                        ),
                      ),
                    ],
                  ),
                ),
                const SizedBox(height: 12),
                Align(
                  alignment: Alignment.centerRight,
                  child: FilledButton.icon(
                    onPressed: _busy ? null : _install,
                    icon: _busy
                        ? const SizedBox(
                            width: 14,
                            height: 14,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : const Icon(Icons.download_rounded, size: 18),
                    label: Text(_busy ? '正在连接…' : '添加并连接'),
                  ),
                ),
                const SizedBox(height: 4),
                Text(
                  '首次安装要下载它的包，可能要等几十秒。',
                  textAlign: TextAlign.right,
                  style: theme.textTheme.labelSmall,
                ),
              ],
            ],
          ),
        ),
      ],
    );
  }
}

class _Preview extends StatelessWidget {
  const _Preview({required this.parsed});

  final McpParsedServer parsed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Card(
      margin: const EdgeInsets.only(bottom: 8),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Text(parsed.name, style: theme.textTheme.titleSmall),
                const SizedBox(width: 8),
                Text(parsed.transport, style: theme.textTheme.labelSmall),
                if (parsed.conflicts) ...[
                  const SizedBox(width: 8),
                  Text(
                    '会覆盖同名的那台',
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: theme.colorScheme.error,
                    ),
                  ),
                ],
              ],
            ),
            const SizedBox(height: 6),
            SelectableText(
              parsed.commandLine,
              style: theme.textTheme.bodySmall?.copyWith(
                fontFamily: 'monospace',
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 注册表浏览。
///
/// 只在打开这一页时才去打注册表 —— 那是一次出网请求，不该跟着设置窗
/// 一起发生。
class McpRegistryPanel extends ConsumerStatefulWidget {
  const McpRegistryPanel({required this.onDone, super.key});

  final VoidCallback onDone;

  @override
  ConsumerState<McpRegistryPanel> createState() => _McpRegistryPanelState();
}

class _McpRegistryPanelState extends ConsumerState<McpRegistryPanel> {
  final _query = TextEditingController();
  List<McpRegistryEntry>? _entries;
  String? _error;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    _search();
  }

  @override
  void dispose() {
    _query.dispose();
    super.dispose();
  }

  Future<void> _search() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final got = await ref
          .read(cortexApiProvider)
          .searchMcpRegistry(_query.text.trim());
      if (mounted) setState(() => _entries = got);
    } on CortexApiException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _install(McpRegistryEntry e, McpInstall install) async {
    setState(() => _busy = true);
    try {
      await ref
          .read(cortexApiProvider)
          .saveMcpServer(name: e.suggestedName, config: install.server);
      ref.invalidate(mcpConfigProvider);
      if (mounted) widget.onDone();
    } on CortexApiException catch (err) {
      if (mounted) setState(() => _error = err.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final entries = _entries;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
            onPressed: widget.onDone,
            icon: const Icon(Icons.arrow_back_rounded, size: 18),
            label: const Text('返回列表'),
          ),
        ),
        const SizedBox(height: 4),
        Text('官方 MCP 注册表', style: theme.textTheme.titleMedium),
        const SizedBox(height: 2),
        Text(
          '来自 registry.modelcontextprotocol.io。只在打开这一页时才去查。',
          style: theme.textTheme.labelSmall,
        ),
        const SizedBox(height: 10),
        Row(
          children: [
            Expanded(
              child: TextField(
                controller: _query,
                decoration: const InputDecoration(
                  isDense: true,
                  border: OutlineInputBorder(),
                  hintText: '搜索：filesystem、github、postgres…',
                ),
                onSubmitted: (_) => _search(),
              ),
            ),
            const SizedBox(width: 8),
            FilledButton(
              onPressed: _busy ? null : _search,
              child: const Text('搜索'),
            ),
          ],
        ),
        const SizedBox(height: 12),
        if (_error != null)
          Text(
            _error!,
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.error,
            ),
          ),
        Expanded(
          child: switch ((entries, _busy)) {
            (null, _) || (_, true) when entries == null => const Center(
              child: CircularProgressIndicator(),
            ),
            (final list?, _) when list.isEmpty => Center(
              child: Text('没搜到。', style: theme.textTheme.bodySmall),
            ),
            (final list?, _) => ListView.separated(
              itemCount: list.length,
              separatorBuilder: (_, _) => const SizedBox(height: 6),
              itemBuilder: (context, i) => _RegistryCard(
                entry: list[i],
                busy: _busy,
                onInstall: (install) => _install(list[i], install),
              ),
            ),
            _ => const SizedBox.shrink(),
          },
        ),
      ],
    );
  }
}

class _RegistryCard extends StatelessWidget {
  const _RegistryCard({
    required this.entry,
    required this.busy,
    required this.onInstall,
  });

  final McpRegistryEntry entry;
  final bool busy;
  final void Function(McpInstall install) onInstall;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final install = entry.installs.isEmpty ? null : entry.installs.first;

    return Card(
      margin: EdgeInsets.zero,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 10, 12, 10),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Flexible(
                        child: Text(
                          entry.displayTitle,
                          style: theme.textTheme.titleSmall,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      const SizedBox(width: 6),
                      Text(entry.version, style: theme.textTheme.labelSmall),
                      if (install != null) ...[
                        const SizedBox(width: 6),
                        _Chip(text: install.kind),
                      ],
                    ],
                  ),
                  Text(
                    entry.name,
                    style: theme.textTheme.labelSmall?.copyWith(
                      fontFamily: 'monospace',
                      color: theme.colorScheme.outline,
                    ),
                  ),
                  if (entry.description.isNotEmpty) ...[
                    const SizedBox(height: 4),
                    Text(
                      entry.description,
                      style: theme.textTheme.bodySmall,
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ],
                  // 要填的变量在装之前就说，别等它连不上才发现
                  if (install != null && install.env.isNotEmpty) ...[
                    const SizedBox(height: 4),
                    Text(
                      '装完还要填：'
                      '${install.env.where((v) => v.required_).map((v) => v.name).join('、')}'
                      '${install.env.any((v) => v.required_) ? '' : '（都是可选的）'}',
                      style: theme.textTheme.labelSmall,
                    ),
                  ],
                ],
              ),
            ),
            const SizedBox(width: 8),
            // 装不了的给一个**说明**而不是一个灰按钮：灰按钮不解释原因
            if (install == null)
              Tooltip(
                message: '这条的包类型我们还不支持',
                child: Text('装不了', style: theme.textTheme.labelSmall),
              )
            else
              FilledButton.tonal(
                onPressed: busy ? null : () => onInstall(install),
                child: const Text('安装'),
              ),
          ],
        ),
      ),
    );
  }
}

class _Chip extends StatelessWidget {
  const _Chip({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(4),
      ),
      child: Text(text, style: theme.textTheme.labelSmall),
    );
  }
}
