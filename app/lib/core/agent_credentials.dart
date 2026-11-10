/// 本机控制与远端账号是两条认证路径；控制凭据不发送给远端。
class AgentLaunchCredentials {
  AgentLaunchCredentials({required this.inboundToken, this.userToken}) {
    if (inboundToken.trim().isEmpty) {
      throw ArgumentError('本机控制凭据不能为空');
    }
    if (inboundToken == userToken) {
      throw ArgumentError('本机控制凭据与远端账号凭据必须独立');
    }
  }

  final String inboundToken;
  final String? userToken;

  Map<String, String> environment(Map<String, String>? extra) => {
    // 显式空值覆盖父进程环境，未登录时不继承其他账号的出站 token。
    'CORTEX_TOKEN': userToken ?? '',
    'CORTEX_INBOUND_TOKEN': inboundToken,
    if (extra != null)
      for (final entry in extra.entries)
        if (!const {
          'CORTEX_TOKEN',
          'CORTEX_INBOUND_TOKEN',
        }.contains(entry.key.toUpperCase()))
          entry.key: entry.value,
  };
}
