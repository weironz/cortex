part of '../cortex_api.dart';

mixin CortexApiProjects {
  /// `GET /projects` —— 全部项目。
  ///
  /// 老服务端没有这条路由，答 404。调用方必须把它当作「这个部署没有项目
  /// 功能」优雅降级（[CortexApiException.isUnsupported]），而不是让整个
  /// 侧边栏变成一块错误提示 —— 会话列表本身与项目毫无关系，它照样能用。
  Future<List<Project>> projects();

  /// `POST /projects {"name": …}`
  Future<Project> createProject(String name);

  /// `PATCH /projects/{id}` —— 改名和/或置顶。
  ///
  /// 两者是**两台独立的状态机**（服务端各写一条事件），所以两个参数各自
  /// 可空：只给 `pinned` 就只改置顶，名字一个字都不动。
  Future<Project> patchProject(String id, {String? name, bool? pinned});

  /// `DELETE /projects/{id}` —— **只删这层分组**。
  ///
  /// 里面的会话、消息、附件、抽出来的记忆一概不动，它们变成未分组。
  /// 界面上的确认文案必须把这一条说清楚：用户在这里最怕的事情是
  /// 「删项目把对话也删了」，而那是这个调用唯一**不会**发生的事。
  Future<void> deleteProject(String id);
}
