import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../../core/theme.dart';
import 'neumorphic.dart';

/// Design.md §7.3 rules out standard spinners outright. Everything in this file
/// is a replacement: pulsing dots inside concave containers, shimmer sweeps
/// across extruded cards, and a filling accent ring for determinate progress.

/// Three dots breathing inside a concave well — the "agent is working" state.
class NeuPulsingDots extends StatefulWidget {
  const NeuPulsingDots({super.key, this.size = 6, this.color = VonColors.accent});

  final double size;
  final Color color;

  @override
  State<NeuPulsingDots> createState() => _NeuPulsingDotsState();
}

class _NeuPulsingDotsState extends State<NeuPulsingDots>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1200),
  )..repeat();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _controller,
      builder: (context, _) => Row(
        mainAxisSize: MainAxisSize.min,
        children: List.generate(3, (i) {
          // Stagger each dot by a third of the cycle so the group reads as a
          // travelling wave rather than a synchronised blink.
          final phase = (_controller.value - i * 0.18) % 1.0;
          final opacity = 0.35 + 0.65 * (0.5 + 0.5 * math.sin(phase * 2 * math.pi));
          return Padding(
            padding: EdgeInsets.only(right: i < 2 ? widget.size * 0.8 : 0),
            child: Container(
              width: widget.size,
              height: widget.size,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: widget.color.withOpacity(opacity),
              ),
            ),
          );
        }),
      ),
    );
  }
}

/// Horizontal accent shimmer for the top of an active workspace
/// (Design.md §6, Screen 5).
class NeuShimmerBar extends StatefulWidget {
  const NeuShimmerBar({super.key, this.height = 2, this.active = true});

  final double height;
  final bool active;

  @override
  State<NeuShimmerBar> createState() => _NeuShimmerBarState();
}

class _NeuShimmerBarState extends State<NeuShimmerBar>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: VonMotion.shimmer,
  );

  @override
  void initState() {
    super.initState();
    if (widget.active) _controller.repeat();
  }

  @override
  void didUpdateWidget(NeuShimmerBar old) {
    super.didUpdateWidget(old);
    // Stopping the controller when idle matters: a repeating animation keeps
    // the whole app rendering at 120fps and drains the battery for nothing.
    if (widget.active && !_controller.isAnimating) {
      _controller.repeat();
    } else if (!widget.active && _controller.isAnimating) {
      _controller.stop();
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (!widget.active) return SizedBox(height: widget.height);

    return SizedBox(
      height: widget.height,
      child: AnimatedBuilder(
        animation: _controller,
        builder: (context, _) => ShaderMask(
          shaderCallback: (bounds) {
            final t = _controller.value;
            return LinearGradient(
              begin: Alignment.centerLeft,
              end: Alignment.centerRight,
              colors: const [
                Colors.transparent,
                VonColors.accent,
                Colors.transparent,
              ],
              // Sweep from fully off-screen left to fully off-screen right.
              stops: [
                (t - 0.25).clamp(0.0, 1.0),
                t.clamp(0.0, 1.0),
                (t + 0.25).clamp(0.0, 1.0),
              ],
            ).createShader(bounds);
          },
          child: Container(color: VonColors.accent),
        ),
      ),
    );
  }
}

/// Diagonal shimmer across a card, for list placeholders while data loads.
class NeuShimmerCard extends StatefulWidget {
  const NeuShimmerCard({super.key, this.height = 96, this.radius = VonRadius.large});

  final double height;
  final double radius;

  @override
  State<NeuShimmerCard> createState() => _NeuShimmerCardState();
}

class _NeuShimmerCardState extends State<NeuShimmerCard>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: VonMotion.shimmer,
  )..repeat();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return NeuSurface(
      radius: widget.radius,
      height: widget.height,
      child: AnimatedBuilder(
        animation: _controller,
        builder: (context, _) => DecoratedBox(
          decoration: BoxDecoration(
            gradient: LinearGradient(
              begin: Alignment(-1 - 2 * (1 - _controller.value), -1),
              end: Alignment(1 + 2 * (1 - _controller.value), 1),
              colors: const [
                VonColors.bgSurface,
                VonColors.bgElevated,
                VonColors.bgSurface,
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Determinate accent ring — the Device Profiler's benchmark progress and the
/// download button's fill state.
class NeuProgressRing extends StatelessWidget {
  const NeuProgressRing({
    super.key,
    required this.value,
    this.size = 120,
    this.strokeWidth = 6,
    this.child,
  });

  /// 0..1, or null for an indeterminate sweep.
  final double? value;
  final double size;
  final double strokeWidth;
  final Widget? child;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      width: size,
      height: size,
      child: Stack(
        alignment: Alignment.center,
        children: [
          value == null
              ? _IndeterminateRing(size: size, strokeWidth: strokeWidth)
              : CustomPaint(
                  size: Size.square(size),
                  painter: _RingPainter(
                    value: value!.clamp(0.0, 1.0),
                    strokeWidth: strokeWidth,
                  ),
                ),
          if (child != null) child!,
        ],
      ),
    );
  }
}

class _IndeterminateRing extends StatefulWidget {
  const _IndeterminateRing({required this.size, required this.strokeWidth});

  final double size;
  final double strokeWidth;

  @override
  State<_IndeterminateRing> createState() => _IndeterminateRingState();
}

class _IndeterminateRingState extends State<_IndeterminateRing>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1400),
  )..repeat();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _controller,
      builder: (context, _) => Transform.rotate(
        angle: _controller.value * 2 * math.pi,
        child: CustomPaint(
          size: Size.square(widget.size),
          painter: _RingPainter(value: 0.28, strokeWidth: widget.strokeWidth),
        ),
      ),
    );
  }
}

class _RingPainter extends CustomPainter {
  const _RingPainter({required this.value, required this.strokeWidth});

  final double value;
  final double strokeWidth;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    final centre = rect.center;
    final radius = (size.shortestSide - strokeWidth) / 2;

    // The unfilled track reads as a groove pressed into the surface.
    canvas.drawCircle(
      centre,
      radius,
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = strokeWidth
        ..color = VonColors.shadowDark,
    );

    if (value <= 0) return;

    canvas.drawArc(
      Rect.fromCircle(center: centre, radius: radius),
      -math.pi / 2,
      value * 2 * math.pi,
      false,
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = strokeWidth
        ..strokeCap = StrokeCap.round
        ..color = VonColors.accent
        ..maskFilter = const MaskFilter.blur(BlurStyle.solid, 1.2),
    );
  }

  @override
  bool shouldRepaint(_RingPainter old) =>
      old.value != value || old.strokeWidth != strokeWidth;
}

/// Solid accent block that blinks every 500ms (Design.md §7.4), shown at the
/// end of streaming text.
class NeuTypingCursor extends StatefulWidget {
  const NeuTypingCursor({super.key, this.height = 14, this.width = 7});

  final double height;
  final double width;

  @override
  State<NeuTypingCursor> createState() => _NeuTypingCursorState();
}

class _NeuTypingCursorState extends State<NeuTypingCursor>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: VonMotion.cursorBlink,
  )..repeat(reverse: true);

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return FadeTransition(
      // A hard on/off blink, not a fade — a terminal cursor doesn't dim.
      opacity: _controller.drive(
        Tween(begin: 1.0, end: 0.0).chain(CurveTween(curve: Curves.easeInExpo)),
      ),
      child: Container(
        width: widget.width,
        height: widget.height,
        margin: const EdgeInsets.only(left: 2, bottom: 1),
        color: VonColors.accent,
      ),
    );
  }
}

/// Opacity pulse for the active agent bubble (Design.md §6, Screen 5).
class NeuPulse extends StatefulWidget {
  const NeuPulse({super.key, required this.child, this.active = true});

  final Widget child;
  final bool active;

  @override
  State<NeuPulse> createState() => _NeuPulseState();
}

class _NeuPulseState extends State<NeuPulse> with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1100),
    lowerBound: 0.6,
    upperBound: 1.0,
  );

  @override
  void initState() {
    super.initState();
    if (widget.active) _controller.repeat(reverse: true);
  }

  @override
  void didUpdateWidget(NeuPulse old) {
    super.didUpdateWidget(old);
    if (widget.active && !_controller.isAnimating) {
      _controller.repeat(reverse: true);
    } else if (!widget.active) {
      _controller
        ..stop()
        ..value = 1.0;
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) =>
      FadeTransition(opacity: _controller, child: widget.child);
}

/// Empty-state block: a concave well with an icon, title, and optional action.
class NeuEmptyState extends StatelessWidget {
  const NeuEmptyState({
    super.key,
    required this.icon,
    required this.title,
    this.message,
    this.action,
  });

  final IconData icon;
  final String title;
  final String? message;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(VonSpace.xl),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            NeuSurface(
              depth: NeuDepth.concave,
              radius: VonRadius.large,
              padding: const EdgeInsets.all(VonSpace.lg),
              child: Icon(icon, size: 30, color: VonColors.textDisabled),
            ),
            const SizedBox(height: VonSpace.lg),
            Text(title, style: VonText.heading, textAlign: TextAlign.center),
            if (message != null) ...[
              const SizedBox(height: VonSpace.sm),
              Text(
                message!,
                style: VonText.secondary,
                textAlign: TextAlign.center,
              ),
            ],
            if (action != null) ...[
              const SizedBox(height: VonSpace.lg),
              action!,
            ],
          ],
        ),
      ),
    );
  }
}

/// Inline error strip. Transient failures get a Retry affordance; permanent
/// ones don't, so the user isn't invited to bang on a wall.
class NeuErrorBanner extends StatelessWidget {
  const NeuErrorBanner({
    super.key,
    required this.message,
    this.onRetry,
    this.onDismiss,
  });

  final String message;
  final VoidCallback? onRetry;
  final VoidCallback? onDismiss;

  @override
  Widget build(BuildContext context) {
    return NeuSurface(
      depth: NeuDepth.concave,
      radius: VonRadius.medium,
      padding: const EdgeInsets.all(VonSpace.md),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Icon(Icons.error_outline_rounded, size: 18, color: VonColors.danger),
          const SizedBox(width: VonSpace.sm),
          Expanded(
            child: Text(
              message,
              style: VonText.secondary.copyWith(color: VonColors.textPrimary),
            ),
          ),
          if (onRetry != null)
            TextButton(
              onPressed: onRetry,
              child: Text('Retry', style: VonText.button.copyWith(fontSize: 13)),
            ),
          if (onDismiss != null)
            NeuIconButton(
              icon: Icons.close_rounded,
              size: 28,
              iconSize: 14,
              tone: VonColors.textSecondary,
              onPressed: onDismiss,
            ),
        ],
      ),
    );
  }
}
