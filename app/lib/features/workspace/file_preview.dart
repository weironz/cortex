import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import '../../core/theme.dart';

import '../../models/workspace.dart';
import '../../widgets/markdown/cortex_markdown.dart';

/// 超过这个大小就不预览，只给下载。
///
/// 预览的全部意义是「扫一眼这是不是我要的那个文件」，而那件事不需要把一个
/// 200 MB 的日志读进内存 —— 在 Web 上那还意味着把它读进浏览器的堆里。
///
/// 512 KiB 覆盖了几乎所有源码、markdown、配置与小数据文件；再大的那些，
/// 用户真正想做的是下载下来用自己的编辑器打开。
const int kPreviewMaxBytes = 512 * 1024;

/// 一个文件预览面板。**只读、可选中、不编辑。**
///
/// # 为什么点一下是预览而不是下载
///
/// 从前点一个文件直接触发下载。那对「我要把产物拿走」是对的，对更常见的那件
/// 事——「agent 说它写了 report.md，写了什么？」——则是绕远路：下载、找到下载
/// 目录、用别的程序打开，只为看三行字。
///
/// 下载没有消失，它挪到了这个面板的标题栏上（以及每一行悬停时那个图标）。
///
/// # markdown 直接渲染
///
/// 复用聊天气泡那个 [CortexMarkdown]，于是标题、列表、代码块的样式在两处
/// 一模一样。**不给它加编辑能力**：一个只能看的预览与一个半吊子编辑器之间，
/// 后者要处理与正在写同一个文件的 agent 抢改的问题，而收益只是省一次
/// 「打开我的编辑器」。
class FilePreview extends StatelessWidget {
  const FilePreview({
    super.key,
    required this.node,
    required this.bytes,
    required this.onClose,
    this.onDownload,
    this.error,
    this.loading = false,
  });

  final FileNode node;

  /// 文件内容。`null` = 还在读、读失败、或者根本不该预览（太大 / 是二进制）。
  final Uint8List? bytes;

  final VoidCallback onClose;
  final VoidCallback? onDownload;

  /// 读失败时那句话。**原样显示** —— 服务端那侧的 409「容器不在了」是写给
  /// 用户看的，替换成「加载失败」会把一句可操作的话变成一句废话。
  final String? error;

  final bool loading;

  /// 这个名字看起来像 markdown 吗。
  ///
  /// 按扩展名判而不是嗅探内容：一个 `.txt` 里恰好有几个 `#` 不该被渲染成标题，
  /// 而那正是嗅探会干的事。
  static bool looksMarkdown(String name) {
    final lower = name.toLowerCase();
    return lower.endsWith('.md') || lower.endsWith('.markdown');
  }

  /// 这堆字节像文本吗。
  ///
  /// 判据是「UTF-8 解得开，而且没有 NUL」。**两条都要**：
  /// 单看 UTF-8 的话，一个 PNG 的前几个字节恰好合法就会被当成文本，
  /// 屏幕上出现一屏乱码；而 NUL 是二进制格式里最普遍的那个特征。
  ///
  /// 解不开就当二进制处理，这是**保守**的方向：一个被误判成二进制的文本
  /// 文件只是少了预览（还能下载），而反过来是一屏垃圾。
  static String? decodeText(Uint8List bytes) {
    if (bytes.contains(0)) return null;
    try {
      return utf8.decode(bytes);
    } on FormatException {
      return null;
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Container(
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: scheme.outlineVariant)),
        color: scheme.surface,
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // ── 标题栏 ──
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 6, 4, 6),
            child: Row(
              children: [
                Icon(
                  Icons.description_outlined,
                  size: 14,
                  color: scheme.onSurfaceVariant,
                ),
                const SizedBox(width: 6),
                Expanded(
                  child: Tooltip(
                    // 完整路径放 tooltip：同名文件在不同目录下很常见，
                    // 而标题栏放不下一条长路径
                    message: node.path,
                    child: Text(
                      node.name,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.labelMedium,
                    ),
                  ),
                ),
                if (node.sizeBytes case final n?)
                  Padding(
                    padding: const EdgeInsets.only(right: 4),
                    child: Text(
                      _size(n),
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ),
                if (onDownload != null)
                  IconButton(
                    onPressed: onDownload,
                    iconSize: 15,
                    tooltip: '下载',
                    icon: const Icon(Icons.download_rounded),
                  ),
                IconButton(
                  onPressed: onClose,
                  iconSize: 15,
                  tooltip: '关闭预览',
                  icon: const Icon(Icons.close_rounded),
                ),
              ],
            ),
          ),
          const Divider(height: 1),
          Flexible(child: _body(context)),
        ],
      ),
    );
  }

  Widget _body(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    Widget note(String text) => Padding(
      padding: const EdgeInsets.fromLTRB(14, 16, 14, 20),
      child: Text(
        text,
        style: theme.textTheme.labelSmall?.copyWith(
          color: scheme.onSurfaceVariant,
        ),
      ),
    );

    if (loading) {
      return const Padding(
        padding: EdgeInsets.symmetric(vertical: 18),
        child: Center(
          child: SizedBox(
            width: 16,
            height: 16,
            child: CircularProgressIndicator(strokeWidth: 2),
          ),
        ),
      );
    }
    if (error case final e?) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(14, 16, 14, 20),
        child: Text(
          e,
          style: theme.textTheme.labelSmall?.copyWith(color: scheme.error),
        ),
      );
    }
    final data = bytes;
    if (data == null) return note('这个文件没能读出来。');

    if ((node.sizeBytes ?? data.length) > kPreviewMaxBytes) {
      return note('文件超过 ${_size(kPreviewMaxBytes)}，这里不展开 —— 下载下来看。');
    }
    final text = decodeText(data);
    if (text == null) {
      return note('这是个二进制文件，没法当文本看 —— 下载下来用别的程序打开。');
    }
    if (text.trim().isEmpty) return note('空文件。');

    return Scrollbar(
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(14, 10, 14, 16),
        child: looksMarkdown(node.name)
            ? CortexMarkdown(text)
            // 非 markdown 一律等宽 + 可选中。**不做语法高亮**：那要按扩展名
            // 挑语言，而挑错的表现是一屏颜色错乱的代码 —— 比没有颜色更糟
            : SelectableText(
                text,
                style: theme.textTheme.bodySmall?.copyWith(
                  fontFamily: 'JetBrains Mono',
                  fontFamilyFallback: CortexTheme.monoFallback,
                  height: 1.5,
                ),
              ),
      ),
    );
  }

  static String _size(int bytes) {
    if (bytes < 1024) return '$bytes B';
    if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)} KB';
    return '${(bytes / 1024 / 1024).toStringAsFixed(1)} MB';
  }
}
