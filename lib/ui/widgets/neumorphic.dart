import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:smooth_corner/smooth_corner.dart';

import '../../core/theme.dart';

/// Whether an element sits proud of the surface or is pressed into it.
///
/// This is the whole vocabulary of the design language: extruded means
/// "actionable", concave means "receptive or active", flat means "structural".
enum NeuDepth { extruded, concave, flat }

/// The base neomorphic surface (Design.md §5.1).
///
/// Wrapped in a [RepaintBoundary] because two blurred shadows are expensive to
/// rasterise, and the vast majority of these never change between frames — the
/// boundary lets Flutter reuse the cached layer instead of re-blurring on every
/// scroll tick (SWE §5.1).
class NeuSurface extends StatelessWidget {
  const NeuSurface({
    super.key,
    required this.child,
    this.depth = NeuDepth.extruded,
    this.radius = VonRadius.medium,
    this.padding,
    this.margin,
    this.width,
    this.height,
    this.color,
    this.glow = false,
    this.borderColor,
    this.intensity = 1.0,
  });

  final Widget child;
  final NeuDepth depth;
  final double radius;
  final EdgeInsetsGeometry? padding;
  final EdgeInsetsGeometry? margin;
  final double? width;
  final double? height;
  final Color? color;

  /// Ambient accent halo — used for the active AI process and the armed
  /// "Download Project" button.
  final bool glow;

  /// Draws a 2px accent outline. Design.md uses this for the selected model
  /// card, which stays extruded but gains a border.
  final Color? borderColor;

  /// Scales shadow offset and blur. Small controls need shallower shadows or
  /// they read as smudges rather than edges.
  final double intensity;

  @override
  Widget build(BuildContext context) {
    final shape = SmoothRectangleBorder(
      smoothness: 0.6, // Apple's superellipse constant.
      borderRadius: BorderRadius.circular(radius),
      side: borderColor != null
          ? BorderSide(color: borderColor!, width: 2)
          : BorderSide.none,
    );

    final surfaceColor =
        color ?? (depth == NeuDepth.concave ? VonColors.bgPrimary : VonColors.bgSurface);

    Widget content = Container(
      width: width,
      height: height,
      padding: padding,
      decoration: ShapeDecoration(
        color: surfaceColor,
        shape: shape,
        shadows: switch (depth) {
          // Light from the top-left, dark to the bottom-right: the object is
          // lifted off the surface.
          NeuDepth.extruded => [
              BoxShadow(
                color: VonColors.shadowDark,
                offset: Offset(4 * intensity, 4 * intensity),
                blurRadius: 10 * intensity,
              ),
              BoxShadow(
                color: VonColors.shadowLight,
                offset: Offset(-4 * intensity, -4 * intensity),
                blurRadius: 10 * intensity,
              ),
              if (glow)
                const BoxShadow(
                  color: VonColors.accentGlow,
                  blurRadius: 20,
                  spreadRadius: 1,
                ),
            ],
          // Concave gets its depth from the *inner* shadow painter below; an
          // outer shadow here would contradict it.
          NeuDepth.concave => [
              if (glow)
                const BoxShadow(
                  color: VonColors.accentGlow,
                  blurRadius: 16,
                  spreadRadius: 1,
                ),
            ],
          NeuDepth.flat => const [],
        },
      ),
      child: child,
    );

    if (depth == NeuDepth.concave) {
      content = CustomPaint(
        painter: _InnerShadowPainter(radius: radius, intensity: intensity),
        child: content,
      );
    }

    content = RepaintBoundary(child: content);
    return margin != null ? Padding(padding: margin!, child: content) : content;
  }
}

/// Paints the intruded look for concave surfaces (Design.md §5.3).
///
/// Flutter has no inset `BoxShadow`, so this builds the region *outside* the
/// squircle, offsets and blurs it, and clips the result to the inside — the
/// standard way to fake an inner shadow without a shader.
class _InnerShadowPainter extends CustomPainter {
  const _InnerShadowPainter({required this.radius, required this.intensity});

  final double radius;
  final double intensity;

  @override
  void paint(Canvas canvas, Size size) {
    final bounds = Offset.zero & size;
    final shape = SmoothRectangleBorder(
      smoothness: 0.6,
      borderRadius: BorderRadius.circular(radius),
    ).getOuterPath(bounds);

    // Inflated so the blurred edge never reveals the rectangle's own boundary.
    final outside = Path.combine(
      PathOperation.difference,
      Path()..addRect(bounds.inflate(radius * 3)),
      shape,
    );

    canvas.save();
    canvas.clipPath(shape);
    for (final (color, offset) in [
      (VonColors.shadowDark, Offset(2 * intensity, 2 * intensity)),
      (VonColors.shadowLight, Offset(-2 * intensity, -2 * intensity)),
    ]) {
      canvas.drawPath(
        outside.shift(offset),
        Paint()
          ..color = color
          ..maskFilter = ui.MaskFilter.blur(ui.BlurStyle.normal, 4 * intensity),
      );
    }
    canvas.restore();
  }

  @override
  bool shouldRepaint(_InnerShadowPainter old) =>
      old.radius != radius || old.intensity != intensity;
}

/// A surface that inverts extruded → concave while held (Design.md §7.1).
///
/// The animation is `easeInOut` over 150ms with no spring: the illusion is a
/// rigid plastic key, and overshoot would make it feel like rubber.
class NeuPressable extends StatefulWidget {
  const NeuPressable({
    super.key,
    required this.child,
    this.onTap,
    this.onLongPress,
    this.radius = VonRadius.medium,
    this.padding,
    this.width,
    this.height,
    this.glow = false,
    this.borderColor,
    this.enabled = true,
    this.selected = false,
    this.intensity = 1.0,
    this.semanticLabel,
  });

  final Widget child;
  final VoidCallback? onTap;
  final VoidCallback? onLongPress;
  final double radius;
  final EdgeInsetsGeometry? padding;
  final double? width;
  final double? height;
  final bool glow;
  final Color? borderColor;
  final bool enabled;

  /// A selected control stays concave when released — the state *is* "pressed
  /// in", which is how active tabs and toggles read.
  final bool selected;
  final double intensity;
  final String? semanticLabel;

  @override
  State<NeuPressable> createState() => _NeuPressableState();
}

class _NeuPressableState extends State<NeuPressable> {
  bool _held = false;

  bool get _interactive =>
      widget.enabled && (widget.onTap != null || widget.onLongPress != null);

  @override
  Widget build(BuildContext context) {
    final pressedIn = widget.selected || (_held && _interactive);

    return Semantics(
      button: _interactive,
      enabled: widget.enabled,
      selected: widget.selected,
      label: widget.semanticLabel,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: _interactive ? (_) => setState(() => _held = true) : null,
        onTapUp: _interactive ? (_) => setState(() => _held = false) : null,
        onTapCancel: _interactive ? () => setState(() => _held = false) : null,
        onTap: widget.enabled ? widget.onTap : null,
        onLongPress: widget.enabled ? widget.onLongPress : null,
        child: AnimatedScale(
          // A whisper of scale sells the press without the layout shifting.
          scale: pressedIn && !widget.selected ? 0.985 : 1.0,
          duration: VonMotion.press,
          curve: VonMotion.pressCurve,
          child: AnimatedOpacity(
            opacity: widget.enabled ? 1.0 : 0.45,
            duration: VonMotion.press,
            child: NeuSurface(
              depth: pressedIn ? NeuDepth.concave : NeuDepth.extruded,
              radius: widget.radius,
              padding: widget.padding,
              width: widget.width,
              height: widget.height,
              glow: widget.glow && widget.enabled,
              borderColor: widget.borderColor,
              intensity: widget.intensity,
              child: widget.child,
            ),
          ),
        ),
      ),
    );
  }
}

/// Primary action button (Design.md §5.2): extruded surface, accent label.
class NeuButton extends StatelessWidget {
  const NeuButton({
    super.key,
    required this.label,
    this.onPressed,
    this.icon,
    this.expand = false,
    this.glow = false,
    this.tone = VonColors.accent,
    this.compact = false,
  });

  final String label;
  final VoidCallback? onPressed;
  final IconData? icon;
  final bool expand;
  final bool glow;
  final Color tone;
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final enabled = onPressed != null;
    final color = enabled ? tone : VonColors.textDisabled;

    return NeuPressable(
      onTap: onPressed,
      enabled: enabled,
      glow: glow,
      radius: VonRadius.medium,
      semanticLabel: label,
      padding: EdgeInsets.symmetric(
        horizontal: compact ? VonSpace.md : VonSpace.lg,
        vertical: compact ? 10 : 14,
      ),
      child: Row(
        mainAxisSize: expand ? MainAxisSize.max : MainAxisSize.min,
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          if (icon != null) ...[
            Icon(icon, size: compact ? 16 : 18, color: color),
            const SizedBox(width: VonSpace.sm),
          ],
          Flexible(
            child: Text(
              label,
              overflow: TextOverflow.ellipsis,
              style: VonText.button.copyWith(
                color: color,
                fontSize: compact ? 13 : 14,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// Circular extruded icon button — the FAB and the card overflow menus.
class NeuIconButton extends StatelessWidget {
  const NeuIconButton({
    super.key,
    required this.icon,
    this.onPressed,
    this.size = 44,
    this.iconSize = 20,
    this.tone = VonColors.accent,
    this.glow = false,
    this.selected = false,
    this.tooltip,
  });

  final IconData icon;
  final VoidCallback? onPressed;
  final double size;
  final double iconSize;
  final Color tone;
  final bool glow;
  final bool selected;
  final String? tooltip;

  @override
  Widget build(BuildContext context) {
    final button = NeuPressable(
      onTap: onPressed,
      enabled: onPressed != null,
      // A perfect circle is a squircle whose radius is half its side.
      radius: size / 2,
      width: size,
      height: size,
      glow: glow,
      selected: selected,
      intensity: size < 40 ? 0.7 : 1.0,
      semanticLabel: tooltip,
      child: Center(
        child: Icon(
          icon,
          size: iconSize,
          color: onPressed != null ? tone : VonColors.textDisabled,
        ),
      ),
    );
    return tooltip != null ? Tooltip(message: tooltip!, child: button) : button;
  }
}

/// Concave input field (Design.md §5.3).
class NeuField extends StatefulWidget {
  const NeuField({
    super.key,
    this.controller,
    this.hint,
    this.obscure = false,
    this.maxLines = 1,
    this.minLines,
    this.onSubmitted,
    this.onChanged,
    this.keyboardType,
    this.textStyle,
    this.enabled = true,
    this.autofocus = false,
    this.suffix,
  });

  final TextEditingController? controller;
  final String? hint;
  final bool obscure;
  final int? maxLines;
  final int? minLines;
  final ValueChanged<String>? onSubmitted;
  final ValueChanged<String>? onChanged;
  final TextInputType? keyboardType;
  final TextStyle? textStyle;
  final bool enabled;
  final bool autofocus;
  final Widget? suffix;

  @override
  State<NeuField> createState() => _NeuFieldState();
}

class _NeuFieldState extends State<NeuField> {
  late final FocusNode _focus = FocusNode()..addListener(_onFocusChange);
  bool _focused = false;

  void _onFocusChange() => setState(() => _focused = _focus.hasFocus);

  @override
  void dispose() {
    _focus
      ..removeListener(_onFocusChange)
      ..dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return NeuSurface(
      depth: NeuDepth.concave,
      radius: VonRadius.medium,
      // Focus is a 1px accent ring plus the ambient glow, per Design.md §5.3.
      borderColor: _focused ? VonColors.accent.withOpacity(0.9) : null,
      glow: _focused,
      padding: const EdgeInsets.symmetric(
        horizontal: VonSpace.md,
        vertical: VonSpace.sm + 2,
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          Expanded(
            child: TextField(
              controller: widget.controller,
              focusNode: _focus,
              enabled: widget.enabled,
              autofocus: widget.autofocus,
              obscureText: widget.obscure,
              maxLines: widget.maxLines,
              minLines: widget.minLines,
              keyboardType: widget.keyboardType,
              onSubmitted: widget.onSubmitted,
              onChanged: widget.onChanged,
              style: widget.textStyle ?? VonText.body,
              cursorWidth: 2,
              cursorRadius: const Radius.circular(1),
              decoration: InputDecoration(
                isDense: true,
                border: InputBorder.none,
                enabledBorder: InputBorder.none,
                focusedBorder: InputBorder.none,
                contentPadding: EdgeInsets.zero,
                hintText: widget.hint,
                hintStyle: VonText.body.copyWith(color: VonColors.textDisabled),
              ),
            ),
          ),
          if (widget.suffix != null) ...[
            const SizedBox(width: VonSpace.sm),
            widget.suffix!,
          ],
        ],
      ),
    );
  }
}

/// Toggle (Design.md §5.4): concave track, extruded thumb that turns solid
/// accent with an ambient glow when on.
class NeuToggle extends StatelessWidget {
  const NeuToggle({super.key, required this.value, this.onChanged});

  final bool value;
  final ValueChanged<bool>? onChanged;

  @override
  Widget build(BuildContext context) {
    const trackWidth = 52.0;
    const trackHeight = 30.0;
    const thumb = 22.0;

    return Semantics(
      toggled: value,
      child: GestureDetector(
        onTap: onChanged == null ? null : () => onChanged!(!value),
        child: NeuSurface(
          depth: NeuDepth.concave,
          radius: trackHeight / 2,
          width: trackWidth,
          height: trackHeight,
          intensity: 0.8,
          child: AnimatedAlign(
            duration: VonMotion.press,
            curve: VonMotion.pressCurve,
            alignment: value ? Alignment.centerRight : Alignment.centerLeft,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 4),
              child: Container(
                width: thumb,
                height: thumb,
                decoration: BoxDecoration(
                  shape: BoxShape.circle,
                  color: value ? VonColors.accent : VonColors.bgSurface,
                  boxShadow: [
                    const BoxShadow(
                      color: VonColors.shadowDark,
                      offset: Offset(2, 2),
                      blurRadius: 4,
                    ),
                    const BoxShadow(
                      color: VonColors.shadowLight,
                      offset: Offset(-2, -2),
                      blurRadius: 4,
                    ),
                    if (value)
                      const BoxShadow(color: VonColors.accentGlow, blurRadius: 10),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Small extruded chip for tags and metadata.
class NeuChip extends StatelessWidget {
  const NeuChip({
    super.key,
    required this.label,
    this.icon,
    this.tone,
    this.concave = false,
  });

  final String label;
  final IconData? icon;
  final Color? tone;
  final bool concave;

  @override
  Widget build(BuildContext context) {
    final color = tone ?? VonColors.textSecondary;
    return NeuSurface(
      depth: concave ? NeuDepth.concave : NeuDepth.extruded,
      radius: VonRadius.small,
      intensity: 0.55,
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (icon != null) ...[
            Icon(icon, size: 11, color: color),
            const SizedBox(width: 5),
          ],
          Text(label, style: VonText.caption.copyWith(color: color)),
        ],
      ),
    );
  }
}

/// The workspace tab bar (Design.md §6, Screen 4): extruded pills where the
/// active one goes concave with accent text.
class NeuPillTabs extends StatelessWidget {
  const NeuPillTabs({
    super.key,
    required this.labels,
    required this.index,
    required this.onChanged,
    this.icons,
  });

  final List<String> labels;
  final List<IconData>? icons;
  final int index;
  final ValueChanged<int> onChanged;

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        for (var i = 0; i < labels.length; i++) ...[
          if (i > 0) const SizedBox(width: VonSpace.sm),
          Expanded(
            child: NeuPressable(
              onTap: () => onChanged(i),
              selected: index == i,
              radius: VonRadius.medium,
              intensity: 0.8,
              semanticLabel: labels[i],
              padding: const EdgeInsets.symmetric(vertical: 11),
              child: Row(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  if (icons != null) ...[
                    Icon(
                      icons![i],
                      size: 15,
                      color: index == i ? VonColors.accent : VonColors.textSecondary,
                    ),
                    const SizedBox(width: 6),
                  ],
                  Flexible(
                    child: Text(
                      labels[i],
                      overflow: TextOverflow.ellipsis,
                      style: VonText.button.copyWith(
                        fontSize: 13,
                        color:
                            index == i ? VonColors.accent : VonColors.textSecondary,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ],
      ],
    );
  }
}

/// Large squircle bottom sheet (Design.md §6, Screen 3).
///
/// Overshoots ~2% before settling, so it reads as a drawer being pulled open
/// rather than a panel snapping into place.
Future<T?> showNeuSheet<T>({
  required BuildContext context,
  required WidgetBuilder builder,
  bool dismissible = true,
}) {
  return showModalBottomSheet<T>(
    context: context,
    isScrollControlled: true,
    isDismissible: dismissible,
    enableDrag: dismissible,
    backgroundColor: Colors.transparent,
    barrierColor: Colors.black.withOpacity(0.6),
    transitionAnimationController: null,
    builder: (context) => Padding(
      padding: EdgeInsets.only(
        bottom: MediaQuery.viewInsetsOf(context).bottom,
      ),
      child: SmoothClipRRect(
        smoothness: 0.6,
        borderRadius: const BorderRadius.vertical(
          top: Radius.circular(VonRadius.extraLarge),
        ),
        child: Container(
          color: VonColors.bgSurface,
          padding: const EdgeInsets.fromLTRB(
            VonSpace.lg,
            VonSpace.md,
            VonSpace.lg,
            VonSpace.lg,
          ),
          child: SafeArea(
            top: false,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                // Grab handle.
                Container(
                  width: 38,
                  height: 4,
                  margin: const EdgeInsets.only(bottom: VonSpace.md),
                  decoration: BoxDecoration(
                    color: VonColors.shadowDark,
                    borderRadius: BorderRadius.circular(2),
                  ),
                ),
                Flexible(child: builder(context)),
              ],
            ),
          ),
        ),
      ),
    ),
  );
}
