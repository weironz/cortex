/// 连接页「本机 agent」那一节。
///
/// # 这一节存在的理由
///
/// 桌面端**不是一个进程，是两个**：界面 + 它拉起的 `cortex-local`
/// （工具、文件、命令执行全在后者），后者端口每次由内核随机分。
///
/// 而在这一节之前，**产品里没有任何一个地方承认它存在**。2026-08-20
/// 用户报「串台了」：设置页显示 `https://…/api`，报错却说 `127.0.0.1:9826`
/// —— 那个端口他从没配过，除了「串台」得不出别的结论。
library;

import 'package:cortex_app/models/health_status.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('health 要认得出答话的是谁', () {
    test('agent 答的那份认得出来，并带上上游状态', () {
      final h = HealthStatus.fromJson(const {
        'status': 'ok',
        'version': '0.1.14',
        'role': 'local-agent',
        'server': {
          'remote': 'https://cortex.cloudcele.com/api',
          'reachable': true,
          'backlog': 3,
        },
      });
      expect(h.isLocalAgent, isTrue);
      expect(h.server?.remote, 'https://cortex.cloudcele.com/api');
      expect(h.server?.reachable, isTrue);
      expect(h.server?.backlog, 3, reason: '积压条数要带出来 —— 用户否则不知道自己有多少话还没同步上去');
    });

    /// **老服务端不报 role，必须当成 cortexd。**
    ///
    /// 猜成 local-agent 的话，界面会把一个远端部署的健康说成「本机 agent
    /// 在跑」，而这台机器上可能根本没有那个进程。
    test('不报 role 的按 cortexd 处理', () {
      final h = HealthStatus.fromJson(const {
        'status': 'ok',
        'version': '0.1.11',
      });
      expect(h.isLocalAgent, isFalse);
      expect(h.role, HealthStatus.roleCortexd);
      expect(h.server, isNull, reason: '部署本身没有上游');
    });

    test('server 那一块缺字段也不崩', () {
      final h = HealthStatus.fromJson(const {
        'status': 'ok',
        'version': '0.1.14',
        'role': 'local-agent',
        'server': <String, dynamic>{},
      });
      expect(h.server, isNotNull);
      expect(h.server!.reachable, isFalse, reason: '没说通就是没通，不猜');
      expect(h.server!.backlog, 0);
    });

    /// ⚠️ **这条盯的是一个会静默出错的陷阱。**
    ///
    /// agent 不在跑时，`/health` 那份来自**远端部署**（版本号是部署的）。
    /// 拿它去跟界面版本比，比的是两个无关的数字 —— 而结论会是一个
    /// 根本不存在的「版本不一致」，把用户支去重装一个没问题的东西。
    ///
    /// 所以界面里那句版本比对被 `health.isLocalAgent` 挡着。
    test('部署答的那份不能拿来当 agent 版本比', () {
      final deployment = HealthStatus.fromJson(const {
        'status': 'ok',
        'version': '0.1.11',
        'role': 'cortexd',
      });
      expect(
        deployment.isLocalAgent,
        isFalse,
        reason:
            '这一位就是那道闸：为 false 时界面不做版本比对。'
            '去掉它的话，连着一个老部署就会报「agent 版本不一致」',
      );
    });
  });
}
