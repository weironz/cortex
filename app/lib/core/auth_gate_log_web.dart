/// Web 侧的空实现：浏览器没有可落盘的诊断文件。
///
/// 不补一份 localStorage 版本 —— 那会引入「谁清理它」的问题，而 Web 上
/// `debugPrint` 本来就进控制台，报障截图自带现场。
void logAuthGate({
  required String reason,
  required String detail,
  required bool wasReady,
}) {}

/// 同上：Web 侧不落盘。
void logAuthRestore({required String outcome, required String detail}) {}
