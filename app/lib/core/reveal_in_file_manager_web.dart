/// Web：没有本机目录，也没有文件管理器。见 `reveal_in_file_manager.dart`。
library;

/// 恒 false —— 按钮整个不画，而不是画一个点了没反应的。
const bool kCanRevealInFileManager = false;

/// 编译期占位。真走到这里就是判据被绕过了。
Future<bool> revealInFileManager(String path) async =>
    throw UnsupportedError('Web 端没有本机目录可打开');
