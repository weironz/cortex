part of '../mock_cortex_api.dart';

mixin _CoreMock on MockCortexApiBase {
  @override
  Future<HealthStatus> health() async {
    await _latency(90);
    return const HealthStatus(
      status: 'ok',
      version: '0.0.1-mock',
      database: 'ok',
      // There is no port to protect, so the mock reports the shape a daemon
      // reports when its operator turned authentication off. The login gate
      // therefore lets the mock straight through without a token prompt — which
      // is the behaviour the "offline demo, no daemon" mode exists for.
      auth: 'disabled',
    );
  }

  // ---------------------------------------------------------- confirmations
}
