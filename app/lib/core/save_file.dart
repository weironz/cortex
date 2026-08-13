/// 把一段字节交给用户 —— 第五个平台接缝。
///
/// Web 上「保存文件」是浏览器的下载动作（Blob + 一次点击），桌面上是往磁盘写
/// 一个真文件。两侧同名，调用方因此不需要任何 `kIsWeb`（同 `local_agent.dart`
/// 那一段的论证）。
library;

export 'save_file_io.dart' if (dart.library.js_interop) 'save_file_web.dart';
