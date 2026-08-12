/// 第五道平台 seam：这份构建能不能给自己装一个新版本。
///
/// ## 为什么是 seam 而不是 `kIsWeb`
///
/// 与 `local_agent.dart` 同一个理由：下载落盘要 `File`、拉起安装程序要
/// `Process`，两者都在 `dart:io` 里，而 Web 上这个 import **编译期**就失败，
/// 运行期的 `kIsWeb` 拦不住它。
///
/// ## 为什么 Web 的答案不是「缺个功能」
///
/// 浏览器里的这份客户端是服务端发下来的，刷新一下就是最新的 ——
/// 它压根没有「装在本机、需要被替换掉」的那个东西。
///
/// 两侧暴露同名的东西，所以调用方只有一种形状，也没有任何 widget 判 `kIsWeb`。
library;

export 'updater_io.dart' if (dart.library.js_interop) 'updater_web.dart';
