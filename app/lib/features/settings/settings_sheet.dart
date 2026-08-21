import 'package:flutter/material.dart';

import 'pages/about_page.dart';
import 'pages/appearance_page.dart';
import 'pages/usage_page.dart';
import 'pages/connection_page.dart';
import 'pages/data_page.dart';
import 'pages/mcp_page.dart';
import 'pages/model_page.dart';
import 'pages/model_roles_page.dart';
import '../../core/theme.dart';

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
/// 「模型服务」（有哪些来源、每条开放哪些型号）与「默认模型」（每种活儿
/// 默认用哪个）分成两页，照 Cherry Studio 的分法：前者是**配置**、一次性
/// 的；后者是**偏好**、会反复改。堆在一页里的下场是想换个默认模型的人
/// 得先滚过一屏 key 与端点。
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
      label: '模型服务',
      hint: '供应商 · key · 型号',
      icon: Icons.psychology_outlined,
      page: ModelPage(),
    ),
    (
      label: '默认模型',
      hint: '主 · 快速 · 绘画',
      icon: Icons.tune_rounded,
      page: ModelRolesPage(),
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
      label: '用量',
      hint: 'token · 花费 · 配额',
      icon: Icons.receipt_long_outlined,
      page: UsagePage(),
    ),
    // 排在「关于」前面：它是**偏好**，与上面几项同类；
    // 「关于」是这一列里唯一一个不改任何东西的，压在最后
    (
      label: '外观',
      hint: '主题 · 动效',
      icon: Icons.palette_outlined,
      page: AppearancePage(),
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
    // 窗口跟着屏幕走：小屏上写死尺寸会让内容区被裁掉一半。
    //
    // **两个方向都按比例，然后各自封顶**（上一版宽度直接 `clamp(320, 940)`
    // —— 那不是「跟着屏幕走」，是「除非屏幕比 940 还窄，否则永远 940」）。
    // 1080p 上量出来只占半个屏宽，而「模型服务」那一页里面自己还是两栏，
    // 于是右边的型号列表挤成一条缝。
    //
    // 封顶不是怕大，是**行长**：正文超过一定宽度就难读了。而这里最宽的
    // 东西是两栏加起来（左边导航 208 + 来源列表 190 + 详情），
    // 1280 之后再宽就只是在拉长空白。
    final size = MediaQuery.sizeOf(context);
    final width = (size.width * 0.78).clamp(320.0, 1280.0);
    final height = (size.height * 0.86).clamp(360.0, 860.0);

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
                    // **中性**（规范第九节）。此前是 `primaryContainer` 的
                    // 一层，于是这一列常年挂着一块紫 —— 与侧栏、模型服务
                    // 那两处被拆掉的是同一个东西，这里是最后一处。
                    // 「我现在在看哪一页」是位置，不是动作
                    selectedTileColor: theme.cortex.sidebarAccent,
                    // 连**文字与图标**一起收回中性。ListTile 的
                    // `selectedColor` 默认是 `colorScheme.primary`，
                    // 只换底色的话，选中项仍然是一行紫字配一个紫图标 ——
                    // 那一块紫只是从背景挪到了前景
                    selectedColor: theme.colorScheme.onSurface,
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(
                        CortexTokens.radiusMd,
                      ),
                    ),
                    leading: Icon(s.icon, size: 20),
                    title: Text(
                      s.label,
                      // 底色转中性之后，「哪一项被选中」只剩底色一个信号。
                      // 加一档字重把它补回来 —— 与「外观」页那组三档选择器
                      // 同一个做法（`SettingsChoice`）
                      style: theme.textTheme.bodyMedium?.copyWith(
                        fontWeight: selected ? FontWeight.w600 : null,
                      ),
                    ),
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
