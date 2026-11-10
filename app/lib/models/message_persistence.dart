/// 保存位置由宿主确认；episode id 本身不能证明远端已经接收。
enum MessagePersistence {
  localOnly,
  pending,
  remote,
  unknown;

  /// 旧宿主缺字段时保留原协议行为；新宿主的明确本机/未知状态不能冒充远端历史。
  static bool canForkRemote(MessagePersistence? status) =>
      status == null || status == remote;

  static MessagePersistence? fromWire(Object? value) => switch (value) {
    null => null,
    'local_only' => localOnly,
    'pending' => pending,
    'remote' => remote,
    _ => unknown,
  };

  String get label => switch (this) {
    localOnly => '已存本机',
    pending => '已存本机，待同步',
    remote => '已同步',
    unknown => '保存位置待确认',
  };
}
