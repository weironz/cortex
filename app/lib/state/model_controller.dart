import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../models/model_source.dart';
import '../models/model_option.dart';
import 'app_providers.dart';

/// 这个部署能用哪些模型。
///
/// `autoDispose` + 每次进设置页重新拉：这份列表跟着部署配置走，而用户
/// 打开设置页正是想看**现在**能选什么。缓存住的话，运维刚加了一家供应商，
/// 用户要重启客户端才看得见。
final modelCatalogProvider = FutureProvider.autoDispose<ModelCatalog>(
  (ref) => ref.watch(cortexApiProvider).llmModels(),
  // ⚠️ **404 不重试。**
  //
  // Riverpod 3 默认对出错的 provider 排指数退避重试。对「服务端暂时抽风」
  // 那是对的；对「这个部署根本没有 /llm/models」（老服务端）则是**永远
  // 不会成功的重试**，只会一直发请求、一直排定时器。
  //
  // 判据与界面那一侧一致：`isUnsupported` 是「没有这个功能」，不是故障。
  retry: (count, error) {
    if (error is CortexApiException && error.isUnsupported) return null;
    // 其余按退避重试，封在半分钟 —— 这份列表不着急，
    // 而一个连不上的后端不该被我们越问越勤
    final secs = [1, 3, 8, 20, 30];
    return Duration(seconds: secs[count.clamp(0, secs.length - 1)]);
  },
);

/// 全部模型来源（`GET /settings/model-sources`）。
///
/// # 为什么不是从 `modelCatalogProvider` 里拿
///
/// 那条路（`/llm/models`）开头就要 `st.llm()` —— 部署自己那把 key 配坏时
/// 它直接报错。而「部署的模型用不了」恰恰是最需要加一条自己的来源的时刻，
/// 那时这份列表不能跟着一起死。
final modelSourcesProvider = FutureProvider.autoDispose<ModelSources>(
  (ref) => ref.watch(cortexApiProvider).modelSources(),
  // 与模型目录同一个判据：没有这条路不是故障，重试永远不会成功
  retry: (count, error) {
    if (error is CortexApiException && error.isUnsupported) return null;
    final secs = [1, 3, 8, 20, 30];
    return Duration(seconds: secs[count.clamp(0, secs.length - 1)]);
  },
);

/// 用户选的模型。**逐轮带**，与 `permissionModeProvider` 完全同构。
///
/// # 存在客户端，不是服务端
///
/// 「账号级默认」就是这台设备上的设置（桌面端 settings.json、Web
/// localStorage）。存服务端要多一次同步，而那次同步失败时用户看到的是
/// 「我明明换了模型」—— 而权限档从第一天起就是这么做的，两者语义完全一样：
/// 在对话框里改一下，**下一句**就按新的走。
///
/// # 三种取值
///
/// - `null` —— 用部署配的那个（默认）
/// - [`kAutoModel`] —— 自动档
/// - 别的字符串 —— 指定一个
class SelectedModelNotifier extends Notifier<String?> {
  static const String _key = 'selected_model';

  @override
  String? build() {
    Future.microtask(_restore);
    return null;
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final v = saved[_key]?.trim();
    // 空串按「没选过」处理，不是按「一个叫空串的模型」——
    // 服务端那侧也是这么判的（`ModelChoice::from`），两侧一致
    if (v == null || v.isEmpty) return;
    if (v != state) state = v;
  }

  /// 选一个。`null` = 回到部署默认。
  void select(String? id) {
    final next = (id == null || id.trim().isEmpty) ? null : id.trim();
    if (state == next) return;
    state = next;
    // 存空串表示「回到默认」——`_restore` 那侧把空串当成没选过
    unawaited(ref.read(settingsPatcherProvider)(_key, next ?? ''));
  }
}

final selectedModelProvider = NotifierProvider<SelectedModelNotifier, String?>(
  SelectedModelNotifier.new,
);

/// 当前选择在界面上该显示成什么。
///
/// 需要目录才说得出名字，所以是个派生 provider 而不是一个 getter。
class ModelLabel {
  const ModelLabel({
    required this.text,
    this.warning,
    this.unsupported = false,
  });

  final String text;

  /// 一句要提醒的话。`null` = 没问题。
  final String? warning;

  /// 这个部署没有模型列表（老服务端）。
  final bool unsupported;
}

/// 当前选择的显示形态，含**该不该提醒**。
///
/// 三种提醒，对应三件真事：
///
/// | 情况 | 提醒 | 为什么 |
/// |---|---|---|
/// | 选的模型 `tool_call == false` | 拦 | agent 会流畅地回答而一个工具都不调 |
/// | 目录里查不到（三个字段全 null） | 提醒 | 不知道不等于不行，但用户该知道我们不知道 |
/// | 选的模型不在列表里了 | 提醒 | 运维下掉了它，下一轮会被服务端拒绝 |
final modelLabelProvider = Provider.autoDispose<ModelLabel>((ref) {
  final selected = ref.watch(selectedModelProvider);
  final catalog = ref.watch(modelCatalogProvider);

  return catalog.when(
    // ⚠️ **重拉时不要退回转圈。**
    //
    // `cortexApiProvider` 在认证落定时会重建一次，那会让这份列表
    // 重拉。Riverpod 那时给的是「带着上一次结果的 AsyncLoading」，
    // 而朴素的 `.when` 会匹配到 loading —— 表现是每次重拉都闪一下
    // 「正在看这个部署能用哪些…」，而上一次的结果明明还在手里。
    skipLoadingOnReload: true,
    skipLoadingOnRefresh: true,
    loading: () => ModelLabel(text: selected ?? '默认模型'),
    error: (e, _) => ModelLabel(
      text: selected ?? '默认模型',
      unsupported: e is CortexApiException && e.isUnsupported,
    ),
    data: (c) {
      if (selected == null) {
        final d = c.byId(c.defaultModel);
        return ModelLabel(
          text:
              d?.displayName ??
              (c.defaultModel.isEmpty ? '默认模型' : c.defaultModel),
        );
      }
      if (selected == kAutoModel) {
        return const ModelLabel(text: '自动');
      }
      final m = c.byId(selected);
      if (m == null) {
        return ModelLabel(
          text: selected,
          warning: '这个部署已经没有开放这个模型了，下一轮会被拒绝。换一个吧。',
        );
      }
      return ModelLabel(
        text: m.displayName,
        warning: switch (m.toolCall) {
          false =>
            '这个模型不支持工具调用 —— 它会照常回答，但读不了文件、跑不了命令，'
                '而界面上看不出区别。',
          null => '服务端的模型目录里没有它，所以不知道它支不支持工具调用。',
          true => null,
        },
      );
    },
  );
});
