part of '../mock_cortex_api.dart';

mixin _ImportMock on MockCortexApiBase {
  @override
  Future<ImportTarget> prepareImport(ImportSource source) async {
    throw const CortexApiException('模拟后端不支持导入 —— 切到真实后端再试。', statusCode: 501);
  }

  @override
  Future<ImportEstimate> importPreview(
    ImportTarget target, {
    int? maxConversations,
  }) async {
    throw const CortexApiException('模拟后端不支持导入。', statusCode: 501);
  }

  @override
  Stream<ImportEvent> runImport(ImportTarget target, {int? maxConversations}) =>
      Stream.error(const CortexApiException('模拟后端不支持导入。', statusCode: 501));
}
