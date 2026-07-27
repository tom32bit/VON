import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'core/providers.dart';
import 'core/theme.dart';
import 'ui/screens/splash_screen.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();

  // The design is a single dark surface edge to edge; a tinted system bar
  // would break the illusion that the UI is part of the device.
  SystemChrome.setSystemUIOverlayStyle(
    const SystemUiOverlayStyle(
      statusBarColor: Colors.transparent,
      statusBarIconBrightness: Brightness.light,
      systemNavigationBarColor: VonColors.bgPrimary,
      systemNavigationBarIconBrightness: Brightness.light,
    ),
  );
  SystemChrome.setPreferredOrientations([
    DeviceOrientation.portraitUp,
    DeviceOrientation.landscapeLeft,
    DeviceOrientation.landscapeRight,
  ]);

  runApp(const ProviderScope(child: VonApp()));
}

class VonApp extends ConsumerStatefulWidget {
  const VonApp({super.key});

  @override
  ConsumerState<VonApp> createState() => _VonAppState();
}

class _VonAppState extends ConsumerState<VonApp>
    with WidgetsBindingObserver {
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState lifecycle) {
    // Backgrounding releases the resident model weights and every preview
    // server. Those are the two things that make this process the obvious
    // candidate for Android's low-memory killer.
    if (lifecycle == AppLifecycleState.paused ||
        lifecycle == AppLifecycleState.detached) {
      ref.read(coreApiProvider).onPaused().ignore();
    }
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'VON',
      debugShowCheckedModeBanner: false,
      theme: buildVonTheme(),
      home: const SplashScreen(),
      builder: (context, child) => MediaQuery.withClampedTextScaling(
        // The neomorphic containers have fixed padding; unbounded scaling
        // overflows them. This keeps large-text accessibility working within
        // a range the layout actually supports.
        minScaleFactor: 0.9,
        maxScaleFactor: 1.3,
        child: child ?? const SizedBox.shrink(),
      ),
    );
  }
}
