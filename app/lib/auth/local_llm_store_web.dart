library;

import '../core/local_llm.dart';

/// Web 上**不存** —— 那里没有本地 agent，这份配置无处可用。
///
/// 不是「存不安全所以不存」，而是「存了也没有任何东西会读它」。
/// 浏览器里的对话经 cortexd，模型是服务端的事。
const bool kCanStoreLocalLlm = false;

Future<LocalLlmConfig> readLocalLlm() async => LocalLlmConfig.empty;

Future<void> writeLocalLlm(LocalLlmConfig config) async {}

Future<void> clearLocalLlm() async {}
