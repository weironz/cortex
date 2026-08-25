import 'agent_launch_log.dart';
import 'local_agent_io.dart' show stateDir;

/// 把「回登录页」这件事连同 reason code 写进启动诊断文件。
///
/// 写失败一律吞掉（[AgentLaunchLog] 的既有契约）：诊断设施把被诊断的
/// 东西弄挂是最糟的一类 bug —— 磁盘满不该让登出流程抛异常。
void logAuthGate({
  required String reason,
  required String detail,
  required bool wasReady,
}) {
  final dir = stateDir();
  if (dir == null) return;
  AgentLaunchLog.inStateDir(
    dir,
  ).authGate(reason: reason, detail: detail, wasReady: wasReady);
}
