import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/providers.dart';
import '../../core/theme.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';
import 'dashboard_screen.dart';

/// PRD Page 1, in Local-Only mode.
///
/// No account, no sign-in, no network call on launch — the splash exists only
/// to cover the native library loading and the SQLite migration. It routes
/// straight to the dashboard, which is what "local-first, privacy-first" means
/// in practice: nothing stands between opening the app and using it.
class SplashScreen extends ConsumerStatefulWidget {
  const SplashScreen({super.key});

  @override
  ConsumerState<SplashScreen> createState() => _SplashScreenState();
}

class _SplashScreenState extends ConsumerState<SplashScreen> {
  bool _navigated = false;

  void _goToDashboard() {
    if (_navigated || !mounted) return;
    _navigated = true;
    Navigator.of(context).pushReplacement(
      PageRouteBuilder(
        transitionDuration: const Duration(milliseconds: 420),
        pageBuilder: (_, __, ___) => const DashboardScreen(),
        transitionsBuilder: (_, animation, __, child) => FadeTransition(
          opacity: CurvedAnimation(parent: animation, curve: Curves.easeOut),
          child: child,
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    ref.listen(coreInitProvider, (_, next) {
      if (next is AsyncData) _goToDashboard();
    });
    final init = ref.watch(coreInitProvider);

    return Scaffold(
      body: Center(
        child: Padding(
          padding: const EdgeInsets.all(VonSpace.xl),
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              const _VonMark(),
              const SizedBox(height: VonSpace.xl),
              Text('VON', style: VonText.display),
              const SizedBox(height: VonSpace.sm),
              Text(
                'Local-first AI sandboxes',
                style: VonText.secondary,
                textAlign: TextAlign.center,
              ),
              const SizedBox(height: VonSpace.xl * 1.5),
              switch (init) {
                AsyncError(:final error) => _StartupFailure(
                    message: error.toString(),
                    onRetry: () => ref.invalidate(coreInitProvider),
                  ),
                // "Initializing VON" reads as the app booting *and* as a
                // sandbox coming up — which is exactly what is happening.
                _ => const _BootingIndicator(),
              },
            ],
          ),
        ),
      ),
    );
  }
}

class _BootingIndicator extends StatelessWidget {
  const _BootingIndicator();

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        const NeuSurface(
          depth: NeuDepth.concave,
          radius: VonRadius.medium,
          padding: EdgeInsets.symmetric(
            horizontal: VonSpace.lg,
            vertical: 14,
          ),
          child: NeuPulsingDots(),
        ),
        const SizedBox(height: VonSpace.md),
        Text('Initializing VON…', style: VonText.caption),
      ],
    );
  }
}


/// The mark: a concave well with an extruded accent core, so the logo itself
/// demonstrates the design language's two states.
class _VonMark extends StatefulWidget {
  const _VonMark();

  @override
  State<_VonMark> createState() => _VonMarkState();
}

class _VonMarkState extends State<_VonMark>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 2400),
  )..repeat(reverse: true);

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return NeuSurface(
      depth: NeuDepth.concave,
      radius: VonRadius.extraLarge,
      width: 108,
      height: 108,
      child: Center(
        child: AnimatedBuilder(
          animation: _controller,
          builder: (context, child) => NeuSurface(
            radius: VonRadius.large,
            width: 56,
            height: 56,
            glow: true,
            intensity: 0.6 + 0.4 * _controller.value,
            child: child!,
          ),
          child: const Center(
            child: Icon(
              Icons.hexagon_outlined,
              size: 26,
              color: VonColors.accent,
            ),
          ),
        ),
      ),
    );
  }
}

class _StartupFailure extends StatelessWidget {
  const _StartupFailure({required this.message, required this.onRetry});

  final String message;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        NeuErrorBanner(
          message: 'The VON core could not start.\n$message',
          onRetry: onRetry,
        ),
        const SizedBox(height: VonSpace.md),
        Text(
          'This usually means the native library for this device architecture '
          'is missing from the build.',
          style: VonText.caption,
          textAlign: TextAlign.center,
        ),
      ],
    );
  }
}
