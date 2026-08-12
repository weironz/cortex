import 'json.dart';

/// 会话的分组容器（`GET /projects`）。
///
/// ## 项目为什么没有「归档」
///
/// 会话有归档，是因为会话**装着不可再生的东西** —— 消息、附件、由它抽出来的
/// 记忆。删掉就没了，所以那里只有「归档」这一个诚实的动词。
///
/// 项目里什么也没装：它是一层引用（`sessions.project_id`）。删掉它，会话
/// 一条不少，只是回到未分组。所以这里 **新建 / 改名 / 删除** 三件事就够了，
/// 再加一个归档反而要求用户去分辨两个后果相同的动作。
class Project {
  const Project({
    required this.id,
    required this.name,
    this.createdAt,
    this.sessionCount = 0,
  });

  final String id;
  final String name;
  final DateTime? createdAt;

  /// 服务端统计的会话数，口径与 `GET /sessions?project_id=` 一致。
  ///
  /// 只用在删除确认里（「里面的 N 个会话不会被删除」）。侧边栏那一组的标题
  /// 上显示的是**本地那一组的行数**，两者故意分开：这个数来自上一次
  /// `GET /projects`，而本地那个数与它旁边的行一一对应，写一个和眼前对不上
  /// 的数字只会让人以为界面漏了东西。
  final int sessionCount;

  Project copyWith({String? name, int? sessionCount}) => Project(
    id: id,
    name: name ?? this.name,
    createdAt: createdAt,
    sessionCount: sessionCount ?? this.sessionCount,
  );

  /// 与其它模型一样宽松地读：缺字段的老服务端应当读成「没有会话的项目」，
  /// 而不是抛异常把整个侧边栏拖垮。
  factory Project.fromJson(Map<String, dynamic> json) => Project(
    id: asString(json['id']),
    name: asString(json['name'], '未命名项目'),
    createdAt: asDateOrNull(json['created_at']),
    sessionCount: asInt(json['session_count']),
  );

  /// 只为往返测试与本地调试存在 —— 客户端从不 POST 整个 ProjectDto
  /// （新建只发 `{"name": …}`）。
  ///
  /// `created_at` 按契约发 RFC3339，且**转成 UTC** 再序列化：
  /// [asDateOrNull] 解出来的是本地时间，原样写回去会把时区偏移丢掉。
  Map<String, dynamic> toJson() => {
    'id': id,
    'name': name,
    if (createdAt case final at?) 'created_at': at.toUtc().toIso8601String(),
    'session_count': sessionCount,
  };

  @override
  bool operator ==(Object other) =>
      other is Project &&
      other.id == id &&
      other.name == name &&
      other.createdAt == createdAt &&
      other.sessionCount == sessionCount;

  @override
  int get hashCode => Object.hash(id, name, createdAt, sessionCount);

  @override
  String toString() => 'Project($id, $name, $sessionCount)';
}
