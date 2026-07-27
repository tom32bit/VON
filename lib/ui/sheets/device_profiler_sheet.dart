import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/errors.dart';
import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../services/core_api.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';

/// PRD Page 4: the Device Profiler.
///
/// Runs a five-second benchmark and assigns a tier. When a `blockedReason` is
/// supplied the sheet is being used as a gate — the action that opened it is
/// disabled and the cloud alternative is offered instead of a Continue button.
class DeviceProfilerSheet extends ConsumerStatefulWidget {
  const DeviceProfilerSheet({
    super.key,
    this.blockedReason,
    this.onUseCloud,
    this.autoStart = true,
  });

  /// Why the task was blocked, when this sheet is acting as a gate.
  final String? blockedReason;

  /// Offered in place of Continue when blocked.
  final VoidCallback? onUseCloud;

  final bool autoStart;

  @override
  ConsumerState<DeviceProfilerSheet> createState() =>
      _DeviceProfilerSheetState();
}

class _DeviceProfilerSheetState extends ConsumerState<DeviceProfilerSheet>
    with SingleTickerProviderStateMixin {
  static const _durationMs = 5000;

  late final AnimationController _ring = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: _durationMs),
  );

  bool _running = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    if (widget.autoStart) {
      // After the sheet has settled, so the ring animation isn't competing
      // with the slide-up transition for frames.
      WidgetsBinding.instance.addPostFrameCallback((_) => _run());
    }
  }

  @override
  void dispose() {
    _ring.dispose();
    super.dispose();
  }

  Future<void> _run() async {
    if (_running) return;
    setState(() {
      _running = true;
      _error = null;
    });
    _ring.forward(from: 0);

    try {
      await ref
          .read(deviceProfileProvider.notifier)
          .measure(durationMs: _durationMs);
    } catch (e) {
      if (mounted) setState(() => _error = describeError(e));
    } finally {
      if (mounted) setState(() => _running = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final profile = ref.watch(deviceProfileProvider);

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text('Device Profiler', style: VonText.title),
        const SizedBox(height: VonSpace.xs),
        Text(
          'Measures CPU throughput, memory bandwidth and available RAM to work '
          'out which models this phone can actually run.',
          style: VonText.secondary,
        ),
        const SizedBox(height: VonSpace.lg),

        Center(
          child: AnimatedBuilder(
            animation: _ring,
            builder: (context, _) => NeuProgressRing(
              value: _running ? _ring.value : (profile.hasValue ? 1.0 : 0.0),
              size: 132,
              child: _RingCentre(
                running: _running,
                profile: profile.valueOrNull,
              ),
            ),
          ),
        ),

        const SizedBox(height: VonSpace.lg),

        if (_error != null) ...[
          NeuErrorBanner(message: _error!, onRetry: _run),
          const SizedBox(height: VonSpace.md),
        ],

        if (profile.valueOrNull case final result?) ...[
          _ProfileDetails(profile: result),
          const SizedBox(height: VonSpace.md),
        ],

        if (widget.blockedReason != null) ...[
          NeuSurface(
            depth: NeuDepth.concave,
            radius: VonRadius.medium,
            padding: const EdgeInsets.all(VonSpace.md),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                // Design.md §6 Screen 3: the warning icon glows accent.
                Container(
                  decoration: const BoxDecoration(
                    shape: BoxShape.circle,
                    boxShadow: [
                      BoxShadow(color: VonColors.accentGlow, blurRadius: 12),
                    ],
                  ),
                  child: const Icon(
                    Icons.warning_amber_rounded,
                    size: 20,
                    color: VonColors.accent,
                  ),
                ),
                const SizedBox(width: VonSpace.sm),
                Expanded(
                  child: Text(widget.blockedReason!, style: VonText.secondary),
                ),
              ],
            ),
          ),
          const SizedBox(height: VonSpace.md),
        ],

        Row(
          children: [
            Expanded(
              child: NeuButton(
                label: _running ? 'Measuring…' : 'Run again',
                tone: VonColors.textSecondary,
                expand: true,
                onPressed: _running ? null : _run,
              ),
            ),
            const SizedBox(width: VonSpace.md),
            Expanded(
              child: widget.blockedReason != null && widget.onUseCloud != null
                  ? NeuButton(
                      label: 'Use a cloud model',
                      icon: Icons.cloud_outlined,
                      expand: true,
                      glow: true,
                      onPressed: () {
                        Navigator.of(context).pop();
                        widget.onUseCloud!();
                      },
                    )
                  : NeuButton(
                      label: 'Done',
                      expand: true,
                      glow: profile.hasValue,
                      onPressed: _running
                          ? null
                          : () => Navigator.of(context).pop(profile.valueOrNull),
                    ),
            ),
          ],
        ),
      ],
    );
  }
}

class _RingCentre extends StatelessWidget {
  const _RingCentre({required this.running, required this.profile});

  final bool running;
  final DeviceProfileDto? profile;

  @override
  Widget build(BuildContext context) {
    if (running) {
      return Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          const NeuPulsingDots(),
          const SizedBox(height: VonSpace.sm),
          Text('Benchmarking', style: VonText.caption),
        ],
      );
    }
    if (profile == null) {
      return Text('Not measured', style: VonText.caption);
    }
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          'TIER ${profile!.tier}',
          style: VonText.caption.copyWith(color: VonColors.accent),
        ),
        const SizedBox(height: 2),
        Text(
          profile!.score.round().toString(),
          style: VonText.display.copyWith(fontSize: 26),
        ),
        Text('score', style: VonText.caption),
      ],
    );
  }
}

class _ProfileDetails extends StatelessWidget {
  const _ProfileDetails({required this.profile});

  final DeviceProfileDto profile;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // The tier badge sits in a concave well (Design.md §6, Screen 3).
        Center(
          child: NeuSurface(
            depth: NeuDepth.concave,
            radius: VonRadius.small,
            padding: const EdgeInsets.symmetric(
              horizontal: VonSpace.md,
              vertical: VonSpace.sm,
            ),
            child: Text(
              profile.tierLabel,
              style: VonText.heading.copyWith(
                fontSize: 14,
                color: VonColors.accent,
              ),
            ),
          ),
        ),
        const SizedBox(height: VonSpace.md),
        Row(
          children: [
            Expanded(
              child: _Metric(
                label: 'CPU',
                value: '${profile.cores} cores',
              ),
            ),
            Expanded(
              child: _Metric(
                label: 'RAM',
                value:
                    '${(profile.totalRamMb / 1024).toStringAsFixed(1)}GB',
              ),
            ),
            Expanded(
              child: _Metric(
                label: 'Max local',
                value: profile.maxLocalParamsB < 1
                    ? 'cloud only'
                    : '${profile.maxLocalParamsB.toStringAsFixed(0)}B',
              ),
            ),
          ],
        ),
        if (profile.notes.isNotEmpty) ...[
          const SizedBox(height: VonSpace.md),
          for (final note in profile.notes)
            Padding(
              padding: const EdgeInsets.only(bottom: VonSpace.xs),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  const Padding(
                    padding: EdgeInsets.only(top: 5),
                    child: Icon(
                      Icons.circle,
                      size: 4,
                      color: VonColors.textDisabled,
                    ),
                  ),
                  const SizedBox(width: VonSpace.sm),
                  Expanded(child: Text(note, style: VonText.secondary)),
                ],
              ),
            ),
        ],
      ],
    );
  }
}

class _Metric extends StatelessWidget {
  const _Metric({required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        Text(value, style: VonText.heading.copyWith(fontSize: 15)),
        const SizedBox(height: 2),
        Text(label.toUpperCase(), style: VonText.caption),
      ],
    );
  }
}
