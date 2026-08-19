import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../models/chat_session.dart';
import '../models/workspace.dart';
import '../workspace/workspace_fs.dart';
import 'app_providers.dart';
import 'chat_controller.dart';

/// 一次扫描最多收多少个文件。
///
/// 不是为了省内存，是为了**有个说得出口的上限**：一个真实的仓库里
/// `node_modules` 一层就有几万个条目，无上限地走一遍会让输入框卡住几秒 ——
/// 而用户只是敲了一个 `@`。到顶时界面上会说「只列出了前 N 个」。
const kMentionScanLimit = 800;

/// 最多往下走几层。
const _maxDepth = 5;

/// 一眼就知道不该进去的目录。
///
/// 按名字跳过，不按大小：等发现一个目录有三万个文件时，那三万次
/// stat 已经做完了。这几个名字覆盖了绝大多数情况，漏掉的由 [kMentionScanLimit] 兜底。
const _skip = {
  'node_modules',
  '.git',
  'target',
  'build',
  '.dart_tool',
  '.next',
  'dist',
  'vendor',
  '__pycache__',
  '.venv',
  'venv',
};

class MentionIndex {
  const MentionIndex({
    this.paths = const [],
    this.loading = false,
    this.error,
    this.truncated = false,
    this.available = false,
  });

  /// 相对工作区根的路径，斜杠分隔。
  final List<String> paths;
  final bool loading;
  final String? error;

  /// 扫到上限就停了 —— 这份清单**不完整**，界面上要说。
  final bool truncated;

  /// 这个会话有工作区吗。没有的话 `@` 整个不该弹出来：
  /// 一个引用了文件的问题发给一个没有文件工具的 agent，
  /// 得到的回答会一本正经地跑偏。
  final bool available;

  /// 按输入的片段过滤。
  ///
  /// 子串匹配而不是模糊匹配：模糊匹配（`mc` 命中 `message_composer`）在
  /// 结果少的时候很讨喜，在一个几百文件的仓库里则会把任何两个字母都匹配到
  /// 几十条上 —— 而这里的空间只够显示 8 条。
  List<String> filter(String query) {
    final q = query.trim().toLowerCase();
    if (q.isEmpty) return paths;
    final hits = paths.where((p) => p.toLowerCase().contains(q)).toList();
    // 文件名本身命中的排前面：搜 `readme` 时，`README.md` 该在
    // `docs/readme-notes/x.txt` 前面
    hits.sort((a, b) {
      final an = a.split('/').last.toLowerCase().contains(q);
      final bn = b.split('/').last.toLowerCase().contains(q);
      if (an != bn) return an ? -1 : 1;
      return a.length.compareTo(b.length);
    });
    return hits;
  }
}

/// 当前会话工作区里的文件清单，给输入框的 `@` 用。
///
/// # 为什么按会话缓存，而不是每次敲 `@` 都重扫
///
/// 扫一遍要几百次目录读。用户在一段对话里会敲很多次 `@`，每次都重扫的话
/// 输入框会一卡一卡的。代价是这一轮新写出来的文件不在清单里 —— 所以
/// [refresh] 存在，而且每轮结束会被调一次（新产物正是最可能被引用的东西）。
class FileMentionController extends Notifier<MentionIndex> {
  String? _scannedFor;

  @override
  MentionIndex build() {
    // 换会话 = 换工作区。留着上一份的话，`@` 会列出另一个项目的文件，
    // 而那些路径在这个会话里根本不存在
    ref.listen(chatControllerProvider.select((s) => s.activeSessionId), (_, _) {
      _scannedFor = null;
      state = const MentionIndex();
    });
    // 一轮跑完就作废这份清单。**不立刻重扫** —— 用户可能压根不打算再 `@`，
    // 而重扫是几百次目录读。下次真敲 `@` 时 `ensure` 会重新扫一遍，
    // 那一轮刚写出来的文件正是最可能被引用的东西
    ref.listen(chatControllerProvider.select((s) => s.streaming != null), (
      was,
      now,
    ) {
      if (was == true && now == false) _scannedFor = null;
    });
    return const MentionIndex();
  }

  /// 确保清单是好的。已经扫过这个会话就直接返回。
  Future<void> ensure() async {
    final session = ref.read(chatControllerProvider).activeSession;
    if (session == null) {
      state = const MentionIndex();
      return;
    }
    if (_scannedFor == session.id && !state.loading) return;
    await _scan(session);
  }

  /// 强制重扫。这一轮刚写出来的文件正是最可能被引用的那些。
  Future<void> refresh() async {
    final session = ref.read(chatControllerProvider).activeSession;
    if (session == null) return;
    await _scan(session);
  }

  Future<void> _scan(ChatSession session) async {
    // 本机绑定的目录；null = 这是云端会话，根在容器里
    final local = session.workspace?.root;
    _scannedFor = session.id;
    state = const MentionIndex(loading: true, available: true);
    try {
      final root = local ?? kSandboxRoot;
      final found = <String>[];
      var truncated = false;

      Future<List<FileNode>> list(String path) async => local == null
          ? await ref
                .read(cortexApiProvider)
                .sandboxListFiles(path, sessionId: session.id)
          : await listWorkspaceDirectory(path);

      // 广度优先：顶层的文件先进清单。深度优先的话，撞上一个深目录时
      // 预算会被它吃光，而用户最常引用的恰恰是靠近根的那几个
      var frontier = <String>[root];
      for (var depth = 0; depth <= _maxDepth; depth++) {
        if (frontier.isEmpty) break;
        final next = <String>[];
        for (final dir in frontier) {
          if (found.length >= kMentionScanLimit) {
            truncated = true;
            break;
          }
          late final List<FileNode> nodes;
          try {
            nodes = await list(dir);
          } on Object {
            // 一个读不动的目录（权限、刚被删）不该让整份清单失败
            continue;
          }
          for (final n in nodes) {
            if (n.isDirectory) {
              if (!_skip.contains(n.name) && !n.name.startsWith('.')) {
                next.add(n.path);
              }
              continue;
            }
            if (found.length >= kMentionScanLimit) {
              truncated = true;
              break;
            }
            found.add(_relative(n.path, root));
          }
        }
        if (truncated) break;
        frontier = next;
      }
      if (!ref.mounted) return;
      state = MentionIndex(paths: found, truncated: truncated, available: true);
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      state = MentionIndex(error: e.message, available: true);
    } on Object catch (e) {
      if (!ref.mounted) return;
      state = MentionIndex(error: '$e', available: true);
    }
  }

  /// 把绝对路径变成相对根的、斜杠分隔的路径。
  ///
  /// 统一成斜杠：Windows 上拿到的是 `D:\codes\x\a\b.txt`，而这个字符串会被
  /// 原样发给模型，反斜杠在提示词里还会被当转义看。
  static String _relative(String path, String root) {
    final p = path.replaceAll('\\', '/');
    final r = root.replaceAll('\\', '/').replaceAll(RegExp(r'/+$'), '');
    if (p.length > r.length + 1 && p.startsWith('$r/')) {
      return p.substring(r.length + 1);
    }
    return p;
  }
}

final fileMentionProvider =
    NotifierProvider<FileMentionController, MentionIndex>(
      FileMentionController.new,
    );
