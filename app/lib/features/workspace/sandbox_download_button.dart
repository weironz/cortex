/// 「下载工作区」—— 云沙箱**唯一**的文件出口。
///
/// # 为什么必须有
///
/// 没有它的话，agent 在容器卷里写出来的东西用户永远拿不到：对话流里工具行
/// 写着「写了 report.md」，然后就没有然后了。四家云 agent 各有各的出口
/// （Codex / Devin push 回 git 远端，Claude web 在对话里给下载），
/// 我们至少得有一个。
///
/// # 为什么是整包 tar 而不是文件树
///
/// 文件树要么在容器里跑一次 `ls`（那是不可信侧），要么在宿主侧把整个卷导出
/// 再解 tar 头 —— 后者与直接给整包的代价一样。而用户真正要的通常是
/// 「把这次的产物拿走」，不是「浏览」。先把出口打通。
library;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../core/local_agent.dart';
import '../../core/save_file.dart';
import '../../state/app_providers.dart';

class SandboxDownloadButton extends ConsumerStatefulWidget {
  const SandboxDownloadButton({super.key});

  @override
  ConsumerState<SandboxDownloadButton> createState() =>
      _SandboxDownloadButtonState();
}

class _SandboxDownloadButtonState extends ConsumerState<SandboxDownloadButton> {
  bool _busy = false;

  Future<void> _download() async {
    setState(() => _busy = true);
    String? error;
    Uint8List? bytes;
    try {
      bytes = await ref.read(cortexApiProvider).sandboxWorkspaceTar();
    } on CortexApiException catch (e) {
      // 服务端对「容器被回收了」有一句专门的话（文件还在卷里，发条消息把它
      // 拉起来）。**原样透出**，不要在这里改写成「下载失败」—— 那两件事
      // 在用户那儿的下一步完全不同
      error = e.message;
    } on Object catch (e) {
      error = '$e';
    }
    if (!mounted) return;
    setState(() => _busy = false);

    if (bytes != null) {
      final saved = await saveBytesAs(bytes, 'workspace.tar');
      if (!mounted) return;
      if (!saved) return; // 用户自己取消了，不该弹提示
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('已下载 workspace.tar（${_size(bytes.length)}）')),
      );
      return;
    }
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(SnackBar(content: Text(error ?? '下载失败')));
  }

  static String _size(int bytes) {
    if (bytes < 1024) return '$bytes B';
    if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)} KB';
    return '${(bytes / 1024 / 1024).toStringAsFixed(1)} MB';
  }

  @override
  Widget build(BuildContext context) {
    // 判据是能力而不是平台。桌面端 agent 动的就是用户自己的目录，
    // 「下载工作区」在那儿没有意义
    if (kLocalAgentSupported) return const SizedBox.shrink();

    return OutlinedButton.icon(
      onPressed: _busy ? null : _download,
      icon: _busy
          ? const SizedBox(
              width: 13,
              height: 13,
              child: CircularProgressIndicator(strokeWidth: 2),
            )
          : const Icon(Icons.download_rounded, size: 15),
      label: Text(_busy ? '打包中…' : '下载工作区'),
    );
  }
}
