import 'dart:io';

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
  // ⚠️ **flutter test 里绝不落盘。**
  //
  // 这个文件存在的全部理由是给真实「被踢」事故凑一条跨进程时间线。
  // auth 的测试大量触发 _fallBackToGate —— 不拦的话，每跑一遍测试就往
  // 真诊断文件里灌几十条假「被踢」，将来查真事故的人分不出哪条是真的：
  // 诊断设施被自己的测试污染（评审在真机上数到 48 条才发现）。
  // FLUTTER_TEST 是 flutter_test 绑定恒设的环境变量，桌面端真跑时没有。
  if (Platform.environment['FLUTTER_TEST'] == 'true') return;
  final dir = stateDir();
  if (dir == null) return;
  AgentLaunchLog.inStateDir(
    dir,
  ).authGate(reason: reason, detail: detail, wasReady: wasReady);
}

/// 启动时续会话的结果，落进同一份诊断文件。守卫与 [logAuthGate] 相同。
void logAuthRestore({required String outcome, required String detail}) {
  if (Platform.environment['FLUTTER_TEST'] == 'true') return;
  final dir = stateDir();
  if (dir == null) return;
  AgentLaunchLog.inStateDir(dir).authRestore(outcome: outcome, detail: detail);
}
