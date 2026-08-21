/// 把一张图放进**系统剪贴板** —— 第六个平台接缝。
///
/// # 为什么不能用 `Clipboard.setData`
///
/// Flutter 自带那个只认文本。「复制图片」要的是真的能在画图、Word、
/// 聊天窗口里 Ctrl+V 出一张图，而那在两个平台上是两套完全不同的东西：
/// 浏览器要 `navigator.clipboard.write` + 一个 `Blob`，Windows 要
/// 剪贴板 API + 一份 DIB 字节。
///
/// 两侧同名，调用方因此不需要任何 `kIsWeb`（同 `save_file.dart`）。
///
/// # 返回 `false` 是一个**要显示出来**的答案
///
/// 平台不支持（Linux/macOS 桌面还没写）、剪贴板被别的进程占着、图解不开 ——
/// 都回 false。调用方必须据此说一句话，而不是静默当作成功：
/// 「点了复制，粘出来还是上一次的东西」是最难自查的一类 bug。
library;

export 'copy_image_io.dart' if (dart.library.js_interop) 'copy_image_web.dart';
