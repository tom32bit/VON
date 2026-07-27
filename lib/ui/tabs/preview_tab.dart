import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:webview_flutter/webview_flutter.dart';

import '../../core/errors.dart';
import '../../core/providers.dart';
import '../../core/theme.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';

/// PRD Page 5, Tab 3: the live preview.
///
/// The WebView sits inside a large concave viewport, so the rendered page looks
/// like it's displayed on a screen set into the phone (Design.md §6, Screen 4).
class PreviewTab extends ConsumerStatefulWidget {
  const PreviewTab({super.key, required this.projectId});

  final String projectId;

  @override
  ConsumerState<PreviewTab> createState() => _PreviewTabState();
}

class _PreviewTabState extends ConsumerState<PreviewTab> {
  WebViewController? _controller;
  String? _loadedUrl;
  bool _loading = false;

  WebViewController _buildController() {
    return WebViewController()
      ..setJavaScriptMode(JavaScriptMode.unrestricted)
      // Transparent so the concave viewport shows through before the page
      // paints, instead of flashing white.
      ..setBackgroundColor(VonColors.bgPrimary)
      ..setNavigationDelegate(
        NavigationDelegate(
          onPageStarted: (_) => setState(() => _loading = true),
          onPageFinished: (_) => setState(() => _loading = false),
          // The server is bound to loopback, but a generated page could still
          // contain an external link. Keep the preview on its own origin.
          onNavigationRequest: (request) =>
              request.url.startsWith('http://127.0.0.1')
                  ? NavigationDecision.navigate
                  : NavigationDecision.prevent,
        ),
      );
  }

  void _syncUrl(String url) {
    if (_loadedUrl == url) return;
    _loadedUrl = url;
    _controller ??= _buildController();
    _controller!.loadRequest(Uri.parse(url));
  }

  @override
  Widget build(BuildContext context) {
    final preview = ref.watch(previewUrlProvider(widget.projectId));
    final notifier = ref.read(previewUrlProvider(widget.projectId).notifier);

    // The agent may start the server itself mid-run; adopt whatever URL the
    // provider is holding.
    if (preview.valueOrNull case final url? when url.isNotEmpty) {
      WidgetsBinding.instance.addPostFrameCallback((_) => _syncUrl(url));
    }

    return Padding(
      padding: const EdgeInsets.fromLTRB(
        VonSpace.md,
        0,
        VonSpace.md,
        VonSpace.md,
      ),
      child: Column(
        children: [
          _PreviewToolbar(
            url: preview.valueOrNull,
            loading: _loading || preview.isLoading,
            onStart: notifier.start,
            onReload: () => _controller?.reload(),
            onStop: () async {
              await notifier.stop();
              setState(() {
                _loadedUrl = null;
                _controller = null;
              });
            },
          ),
          const SizedBox(height: VonSpace.sm),
          Expanded(
            child: NeuSurface(
              depth: NeuDepth.concave,
              radius: VonRadius.extraLarge,
              padding: const EdgeInsets.all(6),
              child: ClipRRect(
                borderRadius: BorderRadius.circular(VonRadius.large),
                child: switch (preview) {
                  AsyncError(:final error) => Center(
                      child: Padding(
                        padding: const EdgeInsets.all(VonSpace.md),
                        child: NeuErrorBanner(
                          message: describeError(error),
                          onRetry: notifier.start,
                        ),
                      ),
                    ),
                  AsyncData(value: final url)
                      when url != null && _controller != null =>
                    Stack(
                      children: [
                        WebViewWidget(controller: _controller!),
                        if (_loading)
                          const Align(
                            alignment: Alignment.topCenter,
                            child: NeuShimmerBar(height: 2),
                          ),
                      ],
                    ),
                  AsyncLoading() => const Center(child: NeuPulsingDots()),
                  _ => NeuEmptyState(
                      icon: Icons.public_off_rounded,
                      title: 'Preview not running',
                      message: 'Start a local server to render this VON\'s '
                          'index.html on 127.0.0.1. Nothing is exposed off the '
                          'device.',
                      action: NeuButton(
                        label: 'Start preview',
                        icon: Icons.play_arrow_rounded,
                        glow: true,
                        onPressed: notifier.start,
                      ),
                    ),
                },
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _PreviewToolbar extends StatelessWidget {
  const _PreviewToolbar({
    required this.url,
    required this.loading,
    required this.onStart,
    required this.onReload,
    required this.onStop,
  });

  final String? url;
  final bool loading;
  final VoidCallback onStart;
  final VoidCallback onReload;
  final VoidCallback onStop;

  @override
  Widget build(BuildContext context) {
    final running = url != null && url!.isNotEmpty;

    return Row(
      children: [
        Expanded(
          child: NeuSurface(
            depth: NeuDepth.concave,
            radius: VonRadius.small,
            padding: const EdgeInsets.symmetric(
              horizontal: VonSpace.sm + 2,
              vertical: 9,
            ),
            child: Row(
              children: [
                Icon(
                  running ? Icons.lock_rounded : Icons.lock_open_rounded,
                  size: 12,
                  color: running ? VonColors.success : VonColors.textDisabled,
                ),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    running ? url! : 'not serving',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: VonText.mono.copyWith(fontSize: 11),
                  ),
                ),
              ],
            ),
          ),
        ),
        const SizedBox(width: VonSpace.sm),
        if (running) ...[
          NeuIconButton(
            icon: Icons.refresh_rounded,
            size: 36,
            iconSize: 16,
            tone: VonColors.textSecondary,
            tooltip: 'Reload',
            onPressed: loading ? null : onReload,
          ),
          const SizedBox(width: 6),
          NeuIconButton(
            icon: Icons.stop_rounded,
            size: 36,
            iconSize: 16,
            tone: VonColors.danger,
            tooltip: 'Stop the server',
            onPressed: onStop,
          ),
        ] else
          NeuIconButton(
            icon: Icons.play_arrow_rounded,
            size: 36,
            iconSize: 16,
            glow: true,
            tooltip: 'Start preview',
            onPressed: loading ? null : onStart,
          ),
      ],
    );
  }
}
