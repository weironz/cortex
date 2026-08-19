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

/// 用户选的模型 —— **型号 + 它属于哪条来源**。
///
/// 两段缺一不可：同一个型号名可以在两条来源上都有（两个 OpenAI 兼容网关），
/// 而它们用的是不同的 key、不同的端点、不同的账单。只存型号名的话，
/// 服务端只能猜，而猜错的表现是「我选的是自己那把 key，账单却记在配额上」。
class ModelPick {
  const ModelPick({this.source, this.model});

  /// 部署那条 = [`kDeploymentSource`]；`null` = 没选过。
  final String? source;

  /// `null` = 跟随部署；[`kAutoModel`] = 自动档；别的 = 指名一个。
  final String? model;

  bool get isDefault => model == null;
  bool get isAuto => model == kAutoModel;

  /// 存进设置里的那个字符串。
  ///
  /// 分隔符用 `|` 而不是冒号：**ollama 的型号名本身带冒号**
  /// （`llama3:8b`），按冒号拆会把型号拆断。来源 id 只可能是 ULID
  /// 或字面量 `deployment`，两者都不含 `|`。
  String encode() => model == null ? '' : '${source ?? ''}|$model';

  static ModelPick decode(String? raw) {
    final v = raw?.trim();
    // 空串按「没选过」处理，不是按「一个叫空串的模型」——
    // 服务端那侧也是这么判的（`ModelChoice::from`），两侧一致
    if (v == null || v.isEmpty) return const ModelPick();
    final i = v.indexOf('|');
    // 没有分隔符 = 旧格式（只有型号名）。当成部署那条，别丢掉用户的选择
    if (i < 0) return ModelPick(model: v);
    final source = v.substring(0, i);
    final model = v.substring(i + 1);
    if (model.isEmpty) return const ModelPick();
    return ModelPick(source: source.isEmpty ? null : source, model: model);
  }

  @override
  bool operator ==(Object other) =>
      other is ModelPick && other.source == source && other.model == model;

  @override
  int get hashCode => Object.hash(source, model);

  @override
  String toString() => 'ModelPick($source, $model)';
}

/// 用户选的模型。**逐轮带**，与 `permissionModeProvider` 完全同构。
///
/// # 存在客户端，不是服务端
///
/// 「账号级默认」就是这台设备上的设置（桌面端 settings.json、Web
/// localStorage）。存服务端要多一次同步，而那次同步失败时用户看到的是
/// 「我明明换了模型」—— 而权限档从第一天起就是这么做的，两者语义完全一样：
/// 在对话框里改一下，**下一句**就按新的走。
class SelectedModelNotifier extends Notifier<ModelPick> {
  static const String _key = 'selected_model';

  @override
  ModelPick build() {
    Future.microtask(_restore);
    return const ModelPick();
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    final v = ModelPick.decode(saved[_key]);
    // ⚠️ **没存过就别写回去。**
    //
    // 恢复是异步的，而用户（或者一段代码）可能在它落地之前就选了一个。
    // 无条件赋值的话，「设置里是空的」会把那个选择抹掉 —— 表现是
    // 「我刚选了 Flash，一秒之后自己变回默认了」。
    //
    // 旧版是提前 return 躲过这一条的；改成两段式选择之后这个分支必须
    // 显式写出来，不然就成了一次真的回归。
    if (v.isDefault) return;
    if (v != state) state = v;
  }

  /// 选一个。`const ModelPick()` = 回到部署默认。
  void select(ModelPick pick) {
    if (state == pick) return;
    state = pick;
    // 存空串表示「回到默认」——`decode` 那侧把空串当成没选过
    unawaited(ref.read(settingsPatcherProvider)(_key, pick.encode()));
  }
}

final selectedModelProvider =
    NotifierProvider<SelectedModelNotifier, ModelPick>(
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
  final pick = ref.watch(selectedModelProvider);
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
    loading: () => ModelLabel(text: pick.model ?? '默认模型'),
    error: (e, _) => ModelLabel(
      text: pick.model ?? '默认模型',
      unsupported: e is CortexApiException && e.isUnsupported,
    ),
    data: (c) {
      if (pick.isDefault) {
        final d = c.byId(c.defaultModel);
        return ModelLabel(
          text:
              d?.displayName ??
              (c.defaultModel.isEmpty ? '默认模型' : c.defaultModel),
        );
      }
      if (pick.isAuto) return const ModelLabel(text: '自动');
      final m = c.pick(pick.source, pick.model!);
      if (m == null) {
        return ModelLabel(
          text: pick.model!,
          warning:
              '这个模型已经不在可选列表里了（那条来源被删了或关了），'
              '下一轮会回落到默认。换一个吧。',
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
