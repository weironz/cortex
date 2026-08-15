import 'package:flutter/material.dart';

import 'pages/about_page.dart';
import 'pages/connection_page.dart';
import 'pages/data_page.dart';
import 'pages/mcp_page.dart';
import 'pages/model_page.dart';

/// 打开设置窗。
///
/// 名字与文件名保留了旧的（`settings_sheet.dart` / `showSettingsSheet`）：
/// 唯一的调用点在账号栏，改名要动一个与这次无关的文件，而收益是零。
Future<void> showSettingsSheet(BuildContext context) {
  return showDialog<void>(
    context: context,
    builder: (_) => const _SettingsWindow(),
  );
}

/// 左导航 + 右内容。
///
/// # 为什么从平铺的单列换成分页
///
/// 上一版是一个 460px 宽的 `AlertDialog`，所有设置堆在一列里滚。加到
/// 第七八项时它已经要滚三屏，而每一项都长得一样重要 —— 找一个东西只能
/// 靠从头扫。
///
/// 真正压垮它的是 MCP：那一页自己就有列表 → 详情 → 添加三层，塞进一列
/// 等于把三个界面叠在一起。
///
/// # 分类按「我要改什么」，不按「它属于哪个模块」
///
/// 「模型」里放的是自带 key 与本机模型 —— 一个在服务端一个在本机，
/// 模块上八竿子打不着，但用户来这里想的是同一件事：**这一轮由谁付钱**。
class _SettingsWindow extends StatefulWidget {
  const _SettingsWindow();

  @override
  State<_SettingsWindow> createState() => _SettingsWindowState();
}

/// 一页。
typedef _Section = ({String label, String hint, IconData icon, Widget page});

class _SettingsWindowState extends State<_SettingsWindow> {
  int _index = 0;

  static const _sections = <_Section>[
    (
      label: '连接',
      hint: '数据源 · 后端状态',
      icon: Icons.cable_rounded,
      page: ConnectionPage(),
    ),
    (
      label: '模型',
      hint: '自己的 key · 本机模型',
      icon: Icons.psychology_outlined,
      page: ModelPage(),
    ),
    (
      label: 'MCP 与工具',
      hint: '第三方 server',
      icon: Icons.extension_outlined,
      page: McpPage(),
    ),
    (
      label: '数据',
      hint: '工作空间 · 导入',
      icon: Icons.folder_outlined,
      page: DataPage(),
    ),
    (
      label: '关于',
      hint: '版本 · 更新',
      icon: Icons.info_outline_rounded,
      page: AboutPage(),
    ),
  ];

  @override
  Widget build(BuildContext context) {
    // 窗口跟着屏幕走：小屏上写死 900×620 会让内容区被裁掉一半
    final size = MediaQuery.sizeOf(context);
    final width = size.width.clamp(320.0, 940.0);
    final height = (size.height * 0.82).clamp(360.0, 660.0);

    return Dialog(
      clipBehavior: Clip.antiAlias,
      child: SizedBox(
        width: width,
        height: height,
        // 窄屏（手机、被拖窄的窗口）放不下两列，退回一列 + 顶部下拉
        child: width < 640
            ? _narrow(context)
            : Row(
                children: [
                  _nav(context),
                  const VerticalDivider(width: 1),
                  Expanded(child: _content(context)),
                ],
              ),
      ),
    );
  }

  Widget _nav(BuildContext context) {
    final theme = Theme.of(context);
    return SizedBox(
      width: 208,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 20, 20, 12),
            child: Text('设置', style: theme.textTheme.titleMedium),
          ),
          Expanded(
            child: ListView.builder(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              itemCount: _sections.length,
              itemBuilder: (context, i) {
                final s = _sections[i];
                final selected = i == _index;
                return Padding(
                  padding: const EdgeInsets.only(bottom: 2),
                  child: ListTile(
                    dense: true,
                    selected: selected,
                    selectedTileColor: theme.colorScheme.primaryContainer
                        .withValues(alpha: 0.45),
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(8),
                    ),
                    leading: Icon(s.icon, size: 20),
                    title: Text(s.label, style: theme.textTheme.bodyMedium),
                    subtitle: Text(s.hint, style: theme.textTheme.labelSmall),
                    onTap: () => setState(() => _index = i),
                  ),
                );
              },
            ),
          ),
        ],
      ),
    );
  }

  Widget _content(BuildContext context) {
    final s = _sections[_index];
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(24, 18, 12, 6),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  s.label,
                  style: Theme.of(context).textTheme.titleLarge,
                ),
              ),
              IconButton(
                tooltip: '关闭',
                icon: const Icon(Icons.close_rounded),
                onPressed: () => Navigator.of(context).pop(),
              ),
            ],
          ),
        ),
        Expanded(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(24, 0, 24, 16),
            // 换页时把内部滚动位置也换掉：不给 key 的话，从一页很长的内容
            // 切到一页短的，滚动偏移会被沿用，看起来像那一页是空的
            child: KeyedSubtree(key: ValueKey(_index), child: s.page),
          ),
        ),
      ],
    );
  }

  Widget _narrow(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 14, 8, 6),
          child: Row(
            children: [
              Expanded(
                child: DropdownButtonHideUnderline(
                  child: DropdownButton<int>(
                    value: _index,
                    isExpanded: true,
                    items: [
                      for (var i = 0; i < _sections.length; i++)
                        DropdownMenuItem(
                          value: i,
                          child: Row(
                            children: [
                              Icon(_sections[i].icon, size: 18),
                              const SizedBox(width: 8),
                              Text(
                                _sections[i].label,
                                style: theme.textTheme.titleSmall,
                              ),
                            ],
                          ),
                        ),
                    ],
                    onChanged: (v) => setState(() => _index = v ?? 0),
                  ),
                ),
              ),
              IconButton(
                tooltip: '关闭',
                icon: const Icon(Icons.close_rounded),
                onPressed: () => Navigator.of(context).pop(),
              ),
            ],
          ),
        ),
        const Divider(height: 1),
        Expanded(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
            child: KeyedSubtree(
              key: ValueKey(_index),
              child: _sections[_index].page,
            ),
          ),
        ),
      ],
    );
  }
}
