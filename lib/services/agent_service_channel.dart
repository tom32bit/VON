import 'dart:io';

import 'package:flutter/services.dart';

/// Keeps Android from killing the process mid-run.
///
/// The agent works on Rust threads inside this process, which Android knows
/// nothing about. Once the activity is backgrounded the process is cached, and a
/// cached process holding a 1.7GB mmapped model is the first thing the
/// low-memory killer takes — from the user's side the run vanished. A foreground
/// service is the only supported way to say the work is still happening.
///
/// Reference counted rather than a plain start/stop: two projects can be running
/// at once, and the second one finishing must not pull the process priority out
/// from under the first.
class AgentServiceChannel {
  AgentServiceChannel._();

  static final instance = AgentServiceChannel._();

  static const _channel = MethodChannel('com.von.app/agent_service');

  int _running = 0;

  /// Marks a run as started. Safe to call when the platform has no such
  /// service — on anything but Android this is a no-op.
  Future<void> acquire({required String projectLabel}) async {
    _running++;
    if (_running != 1 || !Platform.isAndroid) return;
    try {
      await _channel.invokeMethod<void>('start', {
        'title': 'VON is working',
        'text': projectLabel,
      });
    } on PlatformException {
      // A missing notification permission is the common case, and it costs the
      // user nothing to keep working — the run just becomes killable again.
      // Failing the run over it would be far worse than the risk.
    } on MissingPluginException {
      // Running against an engine without the channel, e.g. a widget test.
    }
  }

  /// Marks a run as finished, stopping the service once none are left.
  Future<void> release() async {
    if (_running > 0) _running--;
    if (_running != 0 || !Platform.isAndroid) return;
    try {
      await _channel.invokeMethod<void>('stop');
    } on PlatformException {
      // Nothing to do: the service either stopped or was never up.
    } on MissingPluginException {
      // As above.
    }
  }
}
