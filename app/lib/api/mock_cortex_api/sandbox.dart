part of '../mock_cortex_api.dart';

mixin _SandboxMock on MockCortexApiBase {
  @override
  Future<Uint8List> sandboxWorkspaceTar({String? sessionId}) async =>
      throw const CortexApiException('Mock 数据源没有云沙箱', statusCode: 501);

  /// 一份**平的**文件表：键是绝对路径，目录由键前缀推出来。
  ///
  /// 平表而不是嵌套结构，是为了让 [sandboxWriteFile] 只是往 Map 里塞一条 ——
  /// 写完立刻能在树上看见。一个「收下了但列目录时看不到」的夹具，会让上传
  /// 按钮在唯一不用起后端就能演示的模式里看起来是坏的。
  final Map<String, Uint8List> _sandboxFiles = {
    '$kSandboxRoot/README.md': _text('# 云沙箱工作区\n\n这里的文件跨会话保留。\n'),
    '$kSandboxRoot/notes.txt': _text('随手记：记得把 report.md 发给 Lin。\n'),
    '$kSandboxRoot/out/report.md': _text('## Q3 结论\n\n准确率 71% → 76.4%。\n'),
    '$kSandboxRoot/out/figures/latency.csv': _text('p50,p95\n12,38\n'),
    '$kSandboxRoot/src/main.py': _text('print("hello from the sandbox")\n'),
    '$kSandboxRoot/src/util/io.py': _text('def load(path):\n    ...\n'),
  };

  static Uint8List _text(String s) => Uint8List.fromList(utf8.encode(s));

  @override
  Future<List<FileNode>> sandboxListFiles(
    String path, {
    String? sessionId,
  }) async {
    await _latency(140);
    final dir = path.endsWith('/') && path.length > 1
        ? path.substring(0, path.length - 1)
        : path;
    if (!dir.startsWith(kSandboxRoot)) {
      // 与服务端同一条围栏。夹具如果什么都收，越界那条分支就只会在生产上
      // 第一次被执行 —— 而那正是 mock 存在的意义所在
      throw CortexApiException('$path 不在 $kSandboxRoot 之内', statusCode: 400);
    }

    final prefix = '$dir/';
    final dirs = <String>{};
    final files = <FileNode>[];
    for (final entry in _sandboxFiles.entries) {
      if (!entry.key.startsWith(prefix)) continue;
      final rest = entry.key.substring(prefix.length);
      final slash = rest.indexOf('/');
      if (slash >= 0) {
        dirs.add(rest.substring(0, slash));
      } else {
        files.add(
          FileNode(
            name: rest,
            path: entry.key,
            isDirectory: false,
            sizeBytes: entry.value.length,
          ),
        );
      }
    }
    if (dirs.isEmpty && files.isEmpty && dir != kSandboxRoot) {
      throw CortexApiException('$path 不存在', statusCode: 404);
    }

    final directories =
        dirs
            .map(
              (n) =>
                  FileNode(name: n, path: posixJoin(dir, n), isDirectory: true),
            )
            .toList()
          ..sort((a, b) => a.name.compareTo(b.name));
    files.sort((a, b) => a.name.compareTo(b.name));
    return [...directories, ...files];
  }

  @override
  Future<Uint8List> sandboxReadFile(String path, {String? sessionId}) async {
    await _latency(90);
    final bytes = _sandboxFiles[path];
    if (bytes == null) {
      throw CortexApiException('$path 不存在', statusCode: 404);
    }
    return bytes;
  }

  @override
  Future<SandboxWriteReceipt> sandboxWriteFile({
    required String path,
    required Uint8List bytes,
    UploadProgress? onProgress,
    String? sessionId,
  }) async {
    // 一次报完。夹具没有真实的分块，假装分块只会让界面在离线模式里
    // 演一段与真实后端对不上的动画
    onProgress?.call(bytes.length, bytes.length);
    await _latency(160);
    if (!path.startsWith('$kSandboxRoot/')) {
      throw CortexApiException('$path 不在 $kSandboxRoot 之内', statusCode: 400);
    }
    _sandboxFiles[path] = bytes;
    return (path: path, size: bytes.length);
  }
}
