/// 设计系统的唯一入口。
///
/// 界面代码只 import 这一个文件，就能拿到 token、原子件与主题扩展 ——
/// 不需要知道它们各自住在哪个文件里。这也是「换实现不换调用点」的前提：
/// 哪天 `CortexHover` 换成另一套实现，调用点一行都不用动。
library;

export '../core/theme.dart' show CortexTokens, CortexTokensX, CortexTheme;
export 'atoms.dart';
export 'tokens.dart';
