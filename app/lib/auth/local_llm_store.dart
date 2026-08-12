/// 本机 LLM 配置的落盘 —— 与 [`token_store`] 同一条平台缝。
///
/// 存的是一把**明文 API key**，所以位置的选择与 refresh token 是同一个
/// 安全问题，答案也一样：桌面端进系统凭据库（Windows 凭据管理器 /
/// macOS 钥匙串 / libsecret），Web 端不存（那里根本没有本地 agent，
/// 这份配置无处可用）。
library;

export 'local_llm_store_io.dart'
    if (dart.library.js_interop) 'local_llm_store_web.dart';
