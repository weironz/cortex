/// 技能 —— 列表与增删改。
///
/// # 为什么这里没有「这条会话用哪几个技能」
///
/// 与智能体那边（`SessionAssistantNotifier`）刻意不同：**技能是全局的**，
/// 开着的那些每一轮都进目录。
///
/// 理由是两者回答的问题不一样。人设回答「你是谁」——同一条会话里换掉会让
/// 历史前后不一致，所以它按会话绑。技能回答「这件事怎么做」——多一条可用
/// 的做法不会让模型改口，也不会让上一轮说过的话变得不成立。
///
/// 而且**目录很便宜**：一条就几十个 token，贵的正文本来就要等 `load_skill`。
/// 为几十个 token 造一套「这条会话用哪几个」的绑定关系，换来的是用户每开
/// 一条对话都要先勾一遍技能 —— 那正是分层想省掉的事。
///
/// 要临时不让某一条参与，把它**关掉**（`enabled`）——那是一个全局开关，
/// 语义清楚，而且改一次就够。
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../models/skill.dart';
import 'app_providers.dart';

class SkillState {
  const SkillState({
    this.skills = const [],
    this.loading = true,
    this.error,
    this.unsupported = false,
  });

  final List<Skill> skills;
  final bool loading;
  final Object? error;

  /// 这个后端**没有** `/skills`。
  ///
  /// 与 [error] 分得很开：错误该显示、该给重试；「没有这个功能」只该安静地
  /// 说清是后端旧了。与智能体、项目那两页一致。
  final bool unsupported;

  /// 这一轮真的会进目录的那些。
  ///
  /// ⚠️ **发出去的必须是这一份**，而不是 [skills]：关掉的那些混进去的话，
  /// 用户的开关就是个摆设，而界面上它明明是灰的。
  List<Skill> get listable =>
      skills.where((s) => s.isListable).toList(growable: false);

  SkillState copyWith({
    List<Skill>? skills,
    bool? loading,
    Object? error,
    bool clearError = false,
    bool? unsupported,
  }) => SkillState(
    skills: skills ?? this.skills,
    loading: loading ?? this.loading,
    error: clearError ? null : (error ?? this.error),
    unsupported: unsupported ?? this.unsupported,
  );
}

class SkillController extends Notifier<SkillState> {
  @override
  SkillState build() {
    // 换后端要重来一遍：技能跟着账号走
    ref.watch(cortexApiProvider);
    Future.microtask(refresh);
    return const SkillState();
  }

  Future<void> refresh() async {
    state = state.copyWith(loading: true, clearError: true);
    try {
      final list = await ref.read(cortexApiProvider).skills();
      if (!ref.mounted) return;
      state = SkillState(skills: list, loading: false);
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      // 老服务端没有这条路。**说「这个后端没有技能」，不说「出错了」** ——
      // 后者会让人去查网络、去重试，而重试永远不会成功
      state = SkillState(
        loading: false,
        unsupported: e.isUnsupported,
        error: e.isUnsupported ? null : e.message,
      );
    } on Object catch (e) {
      if (!ref.mounted) return;
      state = SkillState(loading: false, error: e);
    }
  }

  /// 新建。失败时抛 —— 调用方在对话框里显示（重名就是从这里回来的）。
  Future<Skill> create(Skill draft) async {
    final made = await ref.read(cortexApiProvider).createSkill(draft);
    if (ref.mounted) {
      state = state.copyWith(skills: [made, ...state.skills]);
    }
    return made;
  }

  Future<Skill> update(
    String id, {
    String? name,
    String? description,
    String? instructions,
    bool? enabled,
  }) async {
    final updated = await ref
        .read(cortexApiProvider)
        .updateSkill(
          id,
          name: name,
          description: description,
          instructions: instructions,
          enabled: enabled,
        );
    if (ref.mounted) {
      state = state.copyWith(
        skills: [
          for (final s in state.skills)
            if (s.id == id) updated else s,
        ],
      );
    }
    return updated;
  }

  Future<void> remove(String id) async {
    await ref.read(cortexApiProvider).deleteSkill(id);
    if (!ref.mounted) return;
    state = state.copyWith(
      skills: state.skills.where((s) => s.id != id).toList(),
    );
  }

  /// 开 / 关。
  ///
  /// 单拎出来是因为它是这一页上**最常按的那个按钮**，而它与「编辑」走的是
  /// 同一条 PATCH。失败时返回一句话，成功时返回 `null` —— 一个开关不该把
  /// 异常抛回按钮的回调里（那会变成未处理的异步错误，界面上什么都不发生，
  /// 这正是置顶那个按钮踩过的坑）。
  Future<String?> setEnabled(String id, bool enabled) async {
    try {
      await update(id, enabled: enabled);
      return null;
    } on CortexApiException catch (e) {
      // 老服务端不认 `enabled`，会说「没有任何要改的字段」
      return e.isUnsupported ? '这个部署还不支持开关技能' : e.message;
    } on Object catch (e) {
      return '$e';
    }
  }
}

final skillControllerProvider = NotifierProvider<SkillController, SkillState>(
  SkillController.new,
);
