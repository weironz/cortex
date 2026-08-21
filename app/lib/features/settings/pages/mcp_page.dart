import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/local_agent.dart';
import '../../../models/mcp.dart';
import '../../../state/app_providers.dart';
import 'mcp_add.dart';
import '../../../core/theme.dart';

/// MCP 与工具这一页。
///
/// # 四层，靠一个 `_view` 在同一块地方切
///
/// 列表 → 详情 → 粘贴添加 → 注册表。不用 `Navigator` 推新路由：这整块
/// 已经在一个对话框里了，再往上叠会得到一个盖着另一个对话框的对话框，
/// 而返回键的行为在两层 barrier 之间是说不清的。
///
/// # 云端会话不在这一页管
///
/// 这几条路由只有本机 agent 有。云端沙箱的 MCP 配置在**工作区根**的
/// `.mcp.json` 里（跟着项目走，与 Claude Code 的项目作用域同名），
/// 下次起容器时生效 —— 页尾那句话说的就是这个，不写的话云端用户会在
/// 这一页反复找一个不存在的开关。
class McpPage extends ConsumerStatefulWidget {
  const McpPage({super.key});

  @override
  ConsumerState<McpPage> createState() => _McpPageState();
}

enum _View { list, detail, add, registry }

class _McpPageState extends ConsumerState<McpPage> {
  _View _view = _View.list;
  String? _selected;

  void _go(_View v, {String? name}) => setState(() {
    _view = v;
    _selected = name;
  });

  @override
  Widget build(BuildContext context) {
    if (!kLocalAgentSupported) return const _NoLocalAgent();

    final async = ref.watch(mcpConfigProvider);
    return async.when(
      loading: () => const Center(child: CircularProgressIndicator()),
      error: (e, _) => _Failed(error: '$e'),
      data: (cfg) {
        // `path` 为空 = 这个后端根本答不了这几条路由（Web、或旧 agent）。
        // 与「答得了但一台都没配」是两回事，界面上那是两句不同的话
        if (cfg.path.isEmpty) return const _NoLocalAgent();
        return switch (_view) {
          _View.list => _ServerList(
            config: cfg,
            onOpen: (n) => _go(_View.detail, name: n),
            onAdd: () => _go(_View.add),
            onRegistry: () => _go(_View.registry),
          ),
          _View.detail => _ServerDetail(
            server: cfg.servers.firstWhere(
              (s) => s.name == _selected,
              // 详情页开着的时候那台被删了（另一个窗口、或者手编了文件）。
              // 回列表而不是崩：那不是错误，只是它不在了
              orElse: () => throw StateError('gone'),
            ),
            onBack: () => _go(_View.list),
          ),
          _View.add => McpAddPanel(
            existing: cfg.servers.map((s) => s.name).toList(),
            onDone: () => _go(_View.list),
          ),
          _View.registry => McpRegistryPanel(onDone: () => _go(_View.list)),
        };
      },
    );
  }
}

/// 没有本机 agent 时这一页说什么。
///
/// **不是一个错误框**：Web 端上这是常态。给的是「在哪能配」而不是
/// 「出错了」——后者会让人以为自己该修点什么。
class _NoLocalAgent extends StatelessWidget {
  const _NoLocalAgent();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              Icons.extension_off_outlined,
              size: 36,
              color: theme.colorScheme.outline,
            ),
            const SizedBox(height: 12),
            Text('MCP 需要本机 agent', style: theme.textTheme.titleSmall),
            const SizedBox(height: 8),
            Text(
              '第三方 MCP server 是在你自己这台机器上跑的子进程，'
              '所以只有桌面端能配。\n\n'
              '云端会话走另一条路：在工作区根目录放一个 .mcp.json'
              '（文件名与 Claude Code 的项目作用域相同），下次起沙箱时生效。',
              textAlign: TextAlign.center,
              style: theme.textTheme.bodySmall,
            ),
          ],
        ),
      ),
    );
  }
}

class _Failed extends ConsumerWidget {
  const _Failed({required this.error});

  final String error;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            error,
            textAlign: TextAlign.center,
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.error,
            ),
          ),
          const SizedBox(height: 12),
          TextButton(
            onPressed: () => ref.invalidate(mcpConfigProvider),
            child: const Text('重试'),
          ),
        ],
      ),
    );
  }
}

/// 列表页。
class _ServerList extends ConsumerStatefulWidget {
  const _ServerList({
    required this.config,
    required this.onOpen,
    required this.onAdd,
    required this.onRegistry,
  });

  final McpConfigView config;
  final void Function(String name) onOpen;
  final VoidCallback onAdd;
  final VoidCallback onRegistry;

  @override
  ConsumerState<_ServerList> createState() => _ServerListState();
}

class _ServerListState extends ConsumerState<_ServerList> {
  bool _busy = false;

  Future<void> _reload() async {
    setState(() => _busy = true);
    try {
      await ref.read(cortexApiProvider).reloadMcp();
      ref.invalidate(mcpConfigProvider);
    } on CortexApiException catch (e) {
      if (mounted) _toast(context, e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final cfg = widget.config;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                cfg.isEmpty
                    ? '还没有接任何 server'
                    : '${cfg.connectedCount} 台已连接 · '
                          '${cfg.brokenCount} 台异常 · '
                          '${cfg.toolCount} 个工具',
                style: theme.textTheme.bodySmall,
              ),
            ),
            IconButton(
              tooltip: '重新连接全部',
              onPressed: _busy ? null : _reload,
              icon: _busy
                  ? const SizedBox(
                      width: 16,
                      height: 16,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : const Icon(Icons.refresh_rounded, size: 20),
            ),
            TextButton.icon(
              onPressed: widget.onRegistry,
              icon: const Icon(Icons.travel_explore_rounded, size: 18),
              label: const Text('浏览注册表'),
            ),
            const SizedBox(width: 4),
            FilledButton.icon(
              onPressed: widget.onAdd,
              icon: const Icon(Icons.add_rounded, size: 18),
              label: const Text('添加'),
            ),
          ],
        ),
        const SizedBox(height: 12),
        Expanded(
          child: cfg.isEmpty
              ? _Empty(path: cfg.path)
              : ListView.separated(
                  itemCount: cfg.servers.length,
                  separatorBuilder: (_, _) => const SizedBox(height: 8),
                  itemBuilder: (context, i) => _ServerCard(
                    server: cfg.servers[i],
                    onOpen: () => widget.onOpen(cfg.servers[i].name),
                  ),
                ),
        ),
        const SizedBox(height: 8),
        SelectableText('配置文件：${cfg.path}', style: theme.textTheme.labelSmall),
      ],
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty({required this.path});

  final String path;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 24),
        child: Text(
          '接一台 MCP server，模型就多一组工具。\n'
          '从别处（Claude Code、某个 README）复制那段命令或 JSON，'
          '点「添加」粘进来即可 —— 配置格式是通用的。',
          textAlign: TextAlign.center,
          style: theme.textTheme.bodySmall,
        ),
      ),
    );
  }
}

/// 列表里的一张卡。
class _ServerCard extends ConsumerWidget {
  const _ServerCard({required this.server, required this.onOpen});

  final McpServer server;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Card(
      margin: EdgeInsets.zero,
      child: InkWell(
        onTap: onOpen,
        borderRadius: BorderRadius.circular(CortexTokens.radiusXl),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(14, 12, 8, 12),
          child: Row(
            children: [
              _Dot(server: server),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        Flexible(
                          child: Text(
                            server.name,
                            style: theme.textTheme.titleSmall,
                            overflow: TextOverflow.ellipsis,
                          ),
                        ),
                        const SizedBox(width: 8),
                        _Chip(text: server.transport),
                        if (server.trust != 'ask') ...[
                          const SizedBox(width: 4),
                          _Chip(text: _trustLabel(server.trust)),
                        ],
                      ],
                    ),
                    const SizedBox(height: 2),
                    Text(
                      _statusLine(server),
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: server.error == null ? null : scheme.error,
                      ),
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                    ),
                    const SizedBox(height: 4),
                    Text(
                      server.commandLine,
                      style: theme.textTheme.labelSmall?.copyWith(
                        fontFamily: 'monospace',
                        color: scheme.outline,
                      ),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ],
                ),
              ),
              // 开关直接落在卡片上：「先关掉看看是不是它的问题」是这一页
              // 最常做的一件事，进详情再关多两下
              Switch.adaptive(
                value: !server.disabled,
                onChanged: (on) => _setEnabled(context, ref, server, on),
              ),
              const Icon(Icons.chevron_right_rounded, size: 18),
            ],
          ),
        ),
      ),
    );
  }
}

String _trustLabel(String trust) => switch (trust) {
  'trusted' => '完全信任',
  'write' => '当作写操作',
  _ => '每次询问',
};

String _statusLine(McpServer s) {
  if (s.disabled) return '已关闭';
  if (s.error != null) return s.error!;
  return s.connected ? '已连接 · ${s.tools.length} 个工具' : '未连接';
}

/// 改 `disabled`。
///
/// 要把**传输配置原样带回去** —— 服务端那条是 upsert，只发一个
/// `disabled` 会把 `command` 丢掉。`env` 不必带：服务端会合并（客户端
/// 手上本来就没有那些值）。
Future<void> _setEnabled(
  BuildContext context,
  WidgetRef ref,
  McpServer server,
  bool on,
) async {
  try {
    await ref
        .read(cortexApiProvider)
        .saveMcpServer(
          name: server.name,
          config: _transportOf(server),
          trust: server.trust,
          disabled: !on,
        );
    ref.invalidate(mcpConfigProvider);
  } on CortexApiException catch (e) {
    if (context.mounted) _toast(context, e.message);
  }
}

/// 从 `command_line` 还原出传输配置。
///
/// # 为什么这样也够
///
/// 它只在「改 trust / 开关」这两条路上用 —— 那时命令本身没变，而
/// `command_line` 是服务端**用同一份配置**渲染出来的。真正要改命令时走的是
/// 编辑表单，那里的值是用户自己敲的。
///
/// 带空格的路径在这里会被切开。所以编辑表单**不走这条路**：它让用户直接
/// 编 args 列表。
Map<String, dynamic> _transportOf(McpServer s) {
  if (s.transport == 'http') return {'url': s.commandLine};
  final parts = s.commandLine.split(' ');
  return {
    'command': parts.isEmpty ? '' : parts.first,
    'args': parts.skip(1).toList(),
  };
}

class _Dot extends StatelessWidget {
  const _Dot({required this.server});

  final McpServer server;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final color = server.disabled
        ? scheme.outlineVariant
        : server.connected
        ? Colors.green
        : scheme.error;
    return Container(
      width: 8,
      height: 8,
      decoration: BoxDecoration(color: color, shape: BoxShape.circle),
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
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
      ),
      child: Text(text, style: theme.textTheme.labelSmall),
    );
  }
}

/// 详情页。
class _ServerDetail extends ConsumerStatefulWidget {
  const _ServerDetail({required this.server, required this.onBack});

  final McpServer server;
  final VoidCallback onBack;

  @override
  ConsumerState<_ServerDetail> createState() => _ServerDetailState();
}

class _ServerDetailState extends ConsumerState<_ServerDetail> {
  bool _busy = false;

  Future<void> _run(Future<void> Function() action) async {
    setState(() => _busy = true);
    try {
      await action();
      ref.invalidate(mcpConfigProvider);
    } on CortexApiException catch (e) {
      if (mounted) _toast(context, e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final s = widget.server;
    final api = ref.read(cortexApiProvider);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
            onPressed: widget.onBack,
            icon: const Icon(Icons.arrow_back_rounded, size: 18),
            label: const Text('返回列表'),
          ),
        ),
        const SizedBox(height: 4),
        Expanded(
          child: ListView(
            children: [
              Row(
                children: [
                  _Dot(server: s),
                  const SizedBox(width: 10),
                  Text(s.name, style: theme.textTheme.titleMedium),
                  const SizedBox(width: 8),
                  _Chip(text: s.transport),
                ],
              ),
              const SizedBox(height: 4),
              Text(_statusLine(s), style: theme.textTheme.bodySmall),
              const SizedBox(height: 16),
              Text('命令', style: theme.textTheme.labelMedium),
              const SizedBox(height: 4),
              SelectableText(
                s.commandLine,
                style: theme.textTheme.bodySmall?.copyWith(
                  fontFamily: 'monospace',
                ),
              ),
              if (s.envNames.isNotEmpty) ...[
                const SizedBox(height: 16),
                Text(
                  s.transport == 'http' ? '请求头' : '环境变量',
                  style: theme.textTheme.labelMedium,
                ),
                const SizedBox(height: 4),
                // **只有名字**。那些值多半是 API key，回显一次就等于让它
                // 出现在截图、录屏、和每一次「帮我看看这个配置」里
                Wrap(
                  spacing: 6,
                  runSpacing: 4,
                  children: [for (final n in s.envNames) _Chip(text: n)],
                ),
                const SizedBox(height: 4),
                Text('值不会回传到界面。要改就重新填一遍。', style: theme.textTheme.labelSmall),
              ],
              const SizedBox(height: 16),
              Text('信任档位', style: theme.textTheme.labelMedium),
              const SizedBox(height: 6),
              SegmentedButton<String>(
                segments: const [
                  ButtonSegment(value: 'ask', label: Text('每次询问')),
                  ButtonSegment(value: 'write', label: Text('当作写操作')),
                  ButtonSegment(value: 'trusted', label: Text('完全信任')),
                ],
                selected: {s.trust},
                showSelectedIcon: false,
                onSelectionChanged: _busy
                    ? null
                    : (v) => _run(
                        () => api.saveMcpServer(
                          name: s.name,
                          config: _transportOf(s),
                          trust: v.first,
                          disabled: s.disabled,
                        ),
                      ),
              ),
              const SizedBox(height: 4),
              Text(
                // 这句话是这个控件存在的全部理由：默认档为什么是「每次询问」
                '外来工具默认每次都要你点同意 —— MCP 里那个「我是只读的」'
                '标记是服务端自报的，认它等于把闸门的钥匙交给被闸的人。'
                '降档只能是你对这台 server 的信任声明。',
                style: theme.textTheme.labelSmall,
              ),
              const SizedBox(height: 20),
              Text('工具（${s.tools.length}）', style: theme.textTheme.labelMedium),
              const SizedBox(height: 6),
              if (s.tools.isEmpty)
                Text(
                  s.connected ? '这台 server 没有报出任何工具。' : '连上之后才知道它有哪些工具。',
                  style: theme.textTheme.bodySmall,
                )
              else
                for (final t in s.tools)
                  Padding(
                    padding: const EdgeInsets.only(bottom: 8),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          t.name,
                          style: theme.textTheme.bodySmall?.copyWith(
                            fontFamily: 'monospace',
                          ),
                        ),
                        Text(t.description, style: theme.textTheme.labelSmall),
                      ],
                    ),
                  ),
            ],
          ),
        ),
        const Divider(height: 20),
        Row(
          mainAxisAlignment: MainAxisAlignment.end,
          children: [
            if (_busy)
              const Padding(
                padding: EdgeInsets.only(right: 12),
                child: SizedBox(
                  width: 16,
                  height: 16,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
              ),
            TextButton(
              onPressed: _busy ? null : () => _run(api.reloadMcp),
              child: const Text('重新连接'),
            ),
            const SizedBox(width: 4),
            TextButton(
              onPressed: _busy
                  ? null
                  : () async {
                      final ok = await _confirmDelete(context, s.name);
                      if (!ok) return;
                      await _run(() => api.deleteMcpServer(s.name));
                      if (mounted) widget.onBack();
                    },
              child: Text(
                '删除',
                style: TextStyle(color: theme.colorScheme.error),
              ),
            ),
          ],
        ),
      ],
    );
  }
}

Future<bool> _confirmDelete(BuildContext context, String name) async {
  final ok = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text('删掉 $name？'),
      content: const Text('配置会从 mcp.json 里移除，这台 server 提供的工具随即消失。'),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(true),
          child: const Text('删除'),
        ),
      ],
    ),
  );
  return ok ?? false;
}

void _toast(BuildContext context, String message) {
  ScaffoldMessenger.maybeOf(
    context,
  )?.showSnackBar(SnackBar(content: Text(message)));
}
