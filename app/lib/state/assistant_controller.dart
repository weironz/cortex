/// 智能体 —— 列表、增删改，以及「这条会话在用哪个」。
///
/// # 「当前用哪个」为什么存在客户端，而不是会话上
///
/// 与 `model` / `permission_mode` 完全同构（见 `ChatRequest` 那几个字段的
/// 文档）：逐轮带，客户端自己记。存服务端要多一次同步，而那次同步失败时
/// 用户看到的是「我明明换了智能体」。
///
/// ⚠️ **换智能体 = 开一条新对话。** 系统提示词是可缓存前缀的第一段
/// （CLAUDE.md 约束 4），在一条会话里逐轮换人设等于每一轮都在打穿
/// prompt caching —— 那是这套系统里最贵的一样东西。所以这里记的是
/// 「**这条会话**当初是用哪个智能体开的」，一条会话认定之后就不再改。
library;

import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../models/assistant.dart';
import 'app_providers.dart';

class AssistantState {
  const AssistantState({
    this.assistants = const [],
    this.loading = true,
    this.error,
    this.unsupported = false,
  });

  final List<Assistant> assistants;
  final bool loading;
  final Object? error;

  /// 这个后端**没有** `/assistants`。
  ///
  /// 与 [error] 分得很开，因为两者要求的反应相反：错误该显示、该给重试；
  /// 「没有这个功能」只该让这一页安静地说清是后端旧了。与项目那边一致。
  final bool unsupported;

  AssistantState copyWith({
    List<Assistant>? assistants,
    bool? loading,
    Object? error,
    bool clearError = false,
    bool? unsupported,
  }) => AssistantState(
    assistants: assistants ?? this.assistants,
    loading: loading ?? this.loading,
    error: clearError ? null : (error ?? this.error),
    unsupported: unsupported ?? this.unsupported,
  );

  Assistant? byId(String? id) =>
      id == null ? null : assistants.where((a) => a.id == id).firstOrNull;
}

class AssistantController extends Notifier<AssistantState> {
  @override
  AssistantState build() {
    // 换后端要重来一遍：智能体是跟着账号走的
    ref.watch(cortexApiProvider);
    Future.microtask(refresh);
    return const AssistantState();
  }

  Future<void> refresh() async {
    state = state.copyWith(loading: true, clearError: true);
    try {
      final list = await ref.read(cortexApiProvider).assistants();
      if (!ref.mounted) return;
      state = AssistantState(assistants: list, loading: false);
    } on CortexApiException catch (e) {
      if (!ref.mounted) return;
      // 老服务端没有这条路。**说「这个后端没有智能体」，不说「出错了」** ——
      // 后者会让人去查网络、去重试，而重试永远不会成功
      state = AssistantState(
        loading: false,
        unsupported: e.isUnsupported,
        error: e.isUnsupported ? null : e.message,
      );
    } on Object catch (e) {
      if (!ref.mounted) return;
      state = AssistantState(loading: false, error: e);
    }
  }

  /// 新建。返回建好的那个；失败时抛 —— 调用方在对话框里显示。
  Future<Assistant> create(Assistant draft) async {
    final made = await ref.read(cortexApiProvider).createAssistant(draft);
    if (ref.mounted) {
      state = state.copyWith(assistants: [made, ...state.assistants]);
    }
    return made;
  }

  Future<Assistant> update(
    String id, {
    String? name,
    String? description,
    String? instructions,
    String? icon,
  }) async {
    final updated = await ref
        .read(cortexApiProvider)
        .updateAssistant(
          id,
          name: name,
          description: description,
          instructions: instructions,
          icon: icon,
        );
    if (ref.mounted) {
      state = state.copyWith(
        assistants: [
          for (final a in state.assistants)
            if (a.id == id) updated else a,
        ],
      );
    }
    return updated;
  }

  Future<void> remove(String id) async {
    await ref.read(cortexApiProvider).deleteAssistant(id);
    if (!ref.mounted) return;
    state = state.copyWith(
      assistants: state.assistants.where((a) => a.id != id).toList(),
    );
  }
}

final assistantControllerProvider =
    NotifierProvider<AssistantController, AssistantState>(
      AssistantController.new,
    );

/// 每条会话当初是用哪个智能体开的。
///
/// # 为什么按会话记，而不是一个全局「当前智能体」
///
/// 全局那一个的表现是：用大厨聊了半天，切回一条旧的技术会话，那条会话
/// 突然也变成大厨了 —— 而它的历史里模型一直自称 Cortex。前后不一致，
/// 且用户没做过任何会导致这件事的操作。
///
/// # 为什么只活在内存里
///
/// 跨重启丢掉的代价很小（回到默认人设，历史一个字不变），而存下来要么
/// 跟着会话进服务端（多一张表、多一次同步），要么塞进 settings.json
/// （那张表会跟着会话数无限长）。等到有人真的抱怨再说。
class SessionAssistantNotifier extends Notifier<Map<String, String>> {
  @override
  Map<String, String> build() => const {};

  String? of(String? sessionId) => sessionId == null ? null : state[sessionId];

  void bind(String sessionId, String? assistantId) {
    final next = {...state};
    if (assistantId == null) {
      next.remove(sessionId);
    } else {
      next[sessionId] = assistantId;
    }
    state = next;
  }
}

final sessionAssistantProvider =
    NotifierProvider<SessionAssistantNotifier, Map<String, String>>(
      SessionAssistantNotifier.new,
    );
