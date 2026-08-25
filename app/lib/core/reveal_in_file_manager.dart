/// 「在资源管理器中打开」—— 又一个平台接缝。
///
/// 桌面端把一个**本机目录**交给系统的文件管理器（Windows: explorer /
/// macOS: open / Linux: xdg-open）。Web 上既没有本机目录也没有文件管理器，
/// 所以那侧 `kCanRevealInFileManager` 恒 false，按钮整个不画（约束 2）。
///
/// 两侧同名导出，调用方不写 `kIsWeb` —— 与 `save_file.dart` 同一套论证。
library;

export 'reveal_in_file_manager_io.dart'
    if (dart.library.js_interop) 'reveal_in_file_manager_web.dart';
