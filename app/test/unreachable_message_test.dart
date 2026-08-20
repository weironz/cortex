/// 连不上时那句话**必须说得出「这个地址是谁」**。
///
/// # 现场（2026-08-20）
///
/// 用户截图：设置页「部署入口地址」写着 `https://cortex.cloudcele.com/api`，
/// 而模型服务那一页报「连不上 cortexd（http://127.0.0.1:9826）」。
/// 他的结论是「串台了，我配的地址没持久化」。
///
/// 实际上地址好好地存着。`127.0.0.1:9826` 是**桌面端自己拉起的本机 agent**
/// ——内核随机分的端口，用户从没配过它。那句话有三处不对：那不是 cortexd、
/// 用户没有「daemon」可启动、切 Mock 也解决不了。
library;

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:flutter_test/flutter_test.dart';

/// 逼出那条「连不上」的消息：指到一个必然没人听的端口。
Future<String> _failureText({String? fronts}) async {
  final api = HttpCortexApi(
    // 9 是 discard 端口，本机上不会有人监听
    baseUrl: 'http://127.0.0.1:9',
    token: 't',
    frontsDeployment: fronts,
  );
  addTearDown(api.dispose);
  try {
    await api.health();
    return '（居然连上了）';
  } on Object catch (e) {
    return '$e';
  }
}

void main() {
  test('打本机 agent 时，说清楚它是谁、你配的又是哪个', () async {
    final msg = await _failureText(fronts: 'https://cortex.cloudcele.com/api');

    expect(
      msg,
      contains('本机 agent'),
      reason:
          '不说的话，用户看到一个自己没配过的 127.0.0.1 端口，'
          '只会以为地址串台了 —— 那正是这条测试的来历',
    );
    expect(
      msg,
      contains('https://cortex.cloudcele.com/api'),
      reason: '要把他**真正配的**那个地址报出来，否则他没法确认配置没丢',
    );
    expect(
      msg,
      isNot(contains('确认 daemon 已启动')),
      reason:
          '本机 agent 不是用户能去启动的 daemon —— '
          '这句话会让他去找一个根本不存在的东西',
    );
  });

  test('直连用户配的地址时，还是原来那句', () async {
    final msg = await _failureText();

    expect(msg, contains('cortexd'), reason: '这种情况下那个地址就是他配的，原来那句话是对的，不该改');
    expect(msg, isNot(contains('本机 agent')));
  });
}
