import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/errors.dart';
import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../services/core_api.dart';
import '../sheets/device_profiler_sheet.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';

/// PRD Page 2: the Model Hub. Offline models on top, cloud keys below.
class ModelHubScreen extends ConsumerWidget {
  const ModelHubScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final offline = ref.watch(offlineModelsProvider);
    final cloud = ref.watch(cloudModelsProvider);
    final hasLocalEngine = ref.read(coreApiProvider).hasLocalInference;

    return Scaffold(
      appBar: AppBar(
        title: Text('Model Hub', style: VonText.title),
        leading: Padding(
          padding: const EdgeInsets.all(VonSpace.sm),
          child: NeuIconButton(
            icon: Icons.arrow_back_rounded,
            size: 38,
            iconSize: 18,
            tone: VonColors.textSecondary,
            onPressed: () => Navigator.of(context).pop(),
          ),
        ),
        leadingWidth: 54,
      ),
      body: ListView(
        // The top inset is not decorative. An extruded NeuSurface throws its
        // light shadow up and to the left, so it paints outside its own box —
        // and a ListView clips at the viewport edge, which sheared the flat top
        // off the first card. Every other scroll view in the app already leaves
        // this gap; this one was the outlier.
        padding: const EdgeInsets.fromLTRB(
          VonSpace.gutter,
          VonSpace.sm,
          VonSpace.gutter,
          VonSpace.xl,
        ),
        children: [
          const _DeviceCard(),
          const SizedBox(height: VonSpace.lg),

          _SectionHeader(
            title: 'On-device models',
            subtitle: hasLocalEngine
                ? 'GGUF weights stored on this phone. No network, no API cost.'
                : 'This build was compiled without local inference support.',
          ),
          const SizedBox(height: VonSpace.md),

          if (!hasLocalEngine)
            const NeuSurface(
              depth: NeuDepth.concave,
              radius: VonRadius.medium,
              padding: EdgeInsets.all(VonSpace.md),
              child: Text(
                'Local inference is disabled in this build. Use a cloud '
                'provider below.',
                style: TextStyle(color: VonColors.textSecondary),
              ),
            )
          else
            switch (offline) {
              AsyncLoading() => const Column(
                  children: [
                    NeuShimmerCard(height: 108),
                    SizedBox(height: VonSpace.md),
                    NeuShimmerCard(height: 108),
                  ],
                ),
              AsyncError(:final error) => NeuErrorBanner(
                  message: describeError(error),
                  onRetry: () => ref.invalidate(offlineModelsProvider),
                ),
              AsyncData(value: final models) => Column(
                  children: [
                    for (final model in models)
                      Padding(
                        padding: const EdgeInsets.only(bottom: VonSpace.md),
                        child: _OfflineModelCard(model: model),
                      ),
                  ],
                ),
              _ => const SizedBox.shrink(),
            },

          const SizedBox(height: VonSpace.lg),
          const _SectionHeader(
            title: 'GitHub',
            subtitle: 'Connect an account and VON can publish a project '
                'straight from its Workspace — or from the chat, by asking.',
          ),
          const SizedBox(height: VonSpace.md),
          const _GitHubCard(),

          const SizedBox(height: VonSpace.lg),
          const _SectionHeader(
            title: 'Cloud providers',
            subtitle: 'Keys are stored in this app\'s private database and are '
                'only ever sent to the provider they belong to.',
          ),
          const SizedBox(height: VonSpace.md),

          for (final provider in const [
            ('anthropic', 'Anthropic', 'sk-ant-…'),
            ('openai', 'OpenAI', 'sk-…'),
            ('gemini', 'Google Gemini', 'AIza…'),
            ('openrouter', 'OpenRouter', 'sk-or-v1-…'),
          ])
            Padding(
              padding: const EdgeInsets.only(bottom: VonSpace.md),
              child: _ApiKeyCard(
                providerId: provider.$1,
                providerName: provider.$2,
                hintFormat: provider.$3,
                models: (cloud.valueOrNull ?? const <CloudModelDto>[])
                    .where((m) => m.provider == provider.$1)
                    .toList(),
              ),
            ),
        ],
      ),
    );
  }
}

class _SectionHeader extends StatelessWidget {
  const _SectionHeader({required this.title, required this.subtitle});

  final String title;
  final String subtitle;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(title, style: VonText.heading),
        const SizedBox(height: 2),
        Text(subtitle, style: VonText.secondary),
      ],
    );
  }
}

/// Live device readout plus the entry point to the profiler.
class _DeviceCard extends ConsumerWidget {
  const _DeviceCard();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final stats = ref.watch(deviceStatsProvider).valueOrNull;
    final profile = ref.watch(deviceProfileProvider).valueOrNull;

    return NeuSurface(
      radius: VonRadius.large,
      padding: const EdgeInsets.all(VonSpace.md),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  profile?.tierLabel ?? 'Device not profiled',
                  style: VonText.heading,
                ),
              ),
              NeuButton(
                label: profile == null ? 'Measure' : 'Re-run',
                compact: true,
                onPressed: () => showNeuSheet<void>(
                  context: context,
                  builder: (_) => const DeviceProfilerSheet(),
                ),
              ),
            ],
          ),
          const SizedBox(height: VonSpace.md),
          if (stats != null)
            Row(
              children: [
                Expanded(
                  child: _Stat(
                    label: 'Free RAM',
                    value: '${(stats.availableRamMb / 1024).toStringAsFixed(1)}GB',
                  ),
                ),
                Expanded(
                  child: _Stat(
                    label: 'Temp',
                    value: stats.temperatureC > 0
                        ? '${stats.temperatureC.toStringAsFixed(0)}°C'
                        : '—',
                    tone: switch (stats.thermal) {
                      'severe' || 'critical' => VonColors.danger,
                      'light' => VonColors.warn,
                      _ => null,
                    },
                  ),
                ),
                Expanded(
                  child: _Stat(
                    label: 'Threads',
                    value: '${stats.inferenceThreads}/${stats.cpuCores}',
                  ),
                ),
                Expanded(
                  child: _Stat(
                    label: 'Battery',
                    value: stats.batteryPct >= 0 ? '${stats.batteryPct}%' : '—',
                  ),
                ),
              ],
            ),
          // The governor is doing something the user can feel — say why.
          if (stats != null && stats.paused) ...[
            const SizedBox(height: VonSpace.md),
            NeuSurface(
              depth: NeuDepth.concave,
              radius: VonRadius.small,
              padding: const EdgeInsets.all(VonSpace.sm),
              child: Row(
                children: [
                  const Icon(
                    Icons.ac_unit_rounded,
                    size: 14,
                    color: VonColors.danger,
                  ),
                  const SizedBox(width: VonSpace.sm),
                  Expanded(
                    child: Text(
                      'Local inference is paused while the device cools down.',
                      style: VonText.caption.copyWith(color: VonColors.danger),
                    ),
                  ),
                ],
              ),
            ),
          ],
        ],
      ),
    );
  }
}

class _Stat extends StatelessWidget {
  const _Stat({required this.label, required this.value, this.tone});

  final String label;
  final String value;
  final Color? tone;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          value,
          style: VonText.heading.copyWith(fontSize: 15, color: tone),
        ),
        Text(label.toUpperCase(), style: VonText.caption),
      ],
    );
  }
}

class _OfflineModelCard extends ConsumerWidget {
  const _OfflineModelCard({required this.model});

  final OfflineModelDto model;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final blocked = model.compatibility == 'red';

    return NeuSurface(
      radius: VonRadius.large,
      // Selected model keeps its extrusion but gains an accent border and glow
      // (Design.md §6, Screen 1).
      borderColor: model.isDefault ? VonColors.accent : null,
      glow: model.isDefault,
      padding: const EdgeInsets.all(VonSpace.md),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(model.name, style: VonText.heading),
                    const SizedBox(height: 2),
                    Text(
                      '${_gb(model.sizeBytes.toInt())} · ${model.quant} · '
                      'needs ${(model.ramRequiredMb / 1024).toStringAsFixed(1)}GB RAM',
                      style: VonText.mono,
                    ),
                  ],
                ),
              ),
              const SizedBox(width: VonSpace.sm),
              _DownloadControl(model: model),
            ],
          ),
          const SizedBox(height: VonSpace.sm),
          Text(model.blurb, style: VonText.secondary),
          const SizedBox(height: VonSpace.md),
          Row(
            children: [
              _CompatibilityDot(level: model.compatibility),
              const SizedBox(width: VonSpace.sm),
              Expanded(
                child: Text(model.compatibilityNote, style: VonText.caption),
              ),
            ],
          ),
          // Delete is available whenever the weights are on disk. It used to be
          // gated on `!isDefault`, which meant the moment you selected a model
          // it became impossible to remove — the one you are most likely to
          // want to reclaim gigabytes from.
          if (model.downloaded) ...[
            const SizedBox(height: VonSpace.md),
            Row(
              children: [
                if (!model.isDefault)
                  Expanded(
                    child: NeuButton(
                      label: blocked ? 'Not enough memory' : 'Use for new VONs',
                      compact: true,
                      expand: true,
                      onPressed: blocked
                          ? null
                          : () async {
                              await ref
                                  .read(coreApiProvider)
                                  .setDefaultLocalModel(model.id);
                              ref.invalidate(offlineModelsProvider);
                            },
                    ),
                  )
                else
                  Expanded(
                    child: Text(
                      'Default for new VONs',
                      style: VonText.caption.copyWith(color: VonColors.accent),
                    ),
                  ),
                const SizedBox(width: VonSpace.sm),
                NeuIconButton(
                  icon: Icons.delete_outline_rounded,
                  size: 38,
                  iconSize: 17,
                  tone: VonColors.textSecondary,
                  tooltip: 'Delete the downloaded weights',
                  onPressed: () => _confirmDelete(context, ref, model),
                ),
              ],
            ),
          ],
        ],
      ),
    );
  }

  /// Weights are gigabytes and the download is slow, so deletion asks first —
  /// the same courtesy the dashboard extends before deleting a VON.
  static Future<void> _confirmDelete(
    BuildContext context,
    WidgetRef ref,
    OfflineModelDto model,
  ) async {
    final confirmed = await showNeuSheet<bool>(
      context: context,
      builder: (sheetContext) => Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text('Delete ${model.name}?', style: VonText.title),
          const SizedBox(height: VonSpace.sm),
          Text(
            'This frees ${_gb(model.sizeBytes.toInt())} of storage. '
            '${model.isDefault ? 'VON will fall back to another downloaded model, or to a cloud provider if there is none. ' : ''}'
            'You can download it again later over the same connection it came from.',
            style: VonText.secondary,
          ),
          const SizedBox(height: VonSpace.lg),
          Row(
            children: [
              Expanded(
                child: NeuButton(
                  label: 'Keep',
                  tone: VonColors.textSecondary,
                  expand: true,
                  onPressed: () => Navigator.of(sheetContext).pop(false),
                ),
              ),
              const SizedBox(width: VonSpace.md),
              Expanded(
                child: NeuButton(
                  label: 'Delete',
                  tone: VonColors.danger,
                  expand: true,
                  onPressed: () => Navigator.of(sheetContext).pop(true),
                ),
              ),
            ],
          ),
        ],
      ),
    );
    if (confirmed ?? false) {
      await ref.read(coreApiProvider).deleteDownloadedModel(model.id);
      ref.invalidate(offlineModelsProvider);
    }
  }

  static String _gb(int bytes) =>
      '${(bytes / (1024 * 1024 * 1024)).toStringAsFixed(2)}GB';
}

/// Circular extruded button that goes concave and fills with an accent ring
/// while downloading (Design.md §6, Screen 1).
class _DownloadControl extends ConsumerWidget {
  const _DownloadControl({required this.model});

  final OfflineModelDto model;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    if (model.downloaded) {
      return const NeuSurface(
        depth: NeuDepth.concave,
        radius: 22,
        width: 44,
        height: 44,
        child: Center(
          child: Icon(
            Icons.check_rounded,
            size: 20,
            color: VonColors.success,
          ),
        ),
      );
    }

    // Only watch once the user has asked for this model — watching a
    // StreamProvider starts it, and an unconditional watch here would begin
    // downloading every model in the catalog as soon as the screen rendered.
    final requested = ref.watch(requestedDownloadsProvider).contains(model.id);
    if (!requested) {
      return NeuIconButton(
        icon: Icons.download_rounded,
        tooltip: 'Download ${model.name}',
        // A red verdict means it cannot run here — don't spend the user's data
        // on gigabytes that will only fail to load.
        onPressed: model.compatibility == 'red'
            ? null
            : () => ref
                .read(requestedDownloadsProvider.notifier)
                .request(model.id),
      );
    }

    final download = ref.watch(downloadProvider(model.id));

    return download.maybeWhen(
      data: (progress) {
        if (progress.done) {
          // Refresh once the stream reports completion.
          WidgetsBinding.instance.addPostFrameCallback((_) {
            ref.read(requestedDownloadsProvider.notifier).forget(model.id);
            ref.invalidate(offlineModelsProvider);
          });
        }
        // Same PlatformInt64 story as writeFile: normalise before arithmetic.
        final total = progress.totalBytes.toInt();
        final received = progress.receivedBytes.toInt();
        final fraction = total > 0 ? received / total : null;
        return NeuSurface(
          depth: NeuDepth.concave,
          radius: 22,
          width: 44,
          height: 44,
          child: Center(
            child: NeuProgressRing(
              value: fraction,
              size: 34,
              strokeWidth: 3,
              child: Text(
                fraction == null ? '' : '${(fraction * 100).round()}',
                style: VonText.caption.copyWith(fontSize: 9),
              ),
            ),
          ),
        );
      },
      // Queued, or the stream errored — either way the download is no longer
      // making progress, so let the user try again.
      orElse: () => NeuIconButton(
        icon: download.hasError ? Icons.refresh_rounded : Icons.download_rounded,
        tone: download.hasError ? VonColors.danger : VonColors.accent,
        tooltip: download.hasError
            ? 'Download failed — tap to resume'
            : 'Downloading ${model.name}',
        onPressed: () {
          ref.read(requestedDownloadsProvider.notifier).forget(model.id);
          ref.invalidate(downloadProvider(model.id));
          ref.read(requestedDownloadsProvider.notifier).request(model.id);
        },
      ),
    );
  }
}

class _CompatibilityDot extends StatelessWidget {
  const _CompatibilityDot({required this.level});

  final String level;

  @override
  Widget build(BuildContext context) {
    final color = switch (level) {
      'green' => VonColors.success,
      'yellow' => VonColors.warn,
      _ => VonColors.danger,
    };
    return Container(
      width: 8,
      height: 8,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: color,
        boxShadow: [BoxShadow(color: color.withOpacity(0.4), blurRadius: 6)],
      ),
    );
  }
}

/// Connects a GitHub account with a personal access token (PRD: publishing).
///
/// A token rather than OAuth: OAuth needs a redirect URI and a client secret
/// that would have to ship inside the APK, where it is not a secret at all. A
/// token the user creates themselves keeps the credential theirs, and they can
/// scope and revoke it on GitHub without going through VON.
class _GitHubCard extends ConsumerStatefulWidget {
  const _GitHubCard();

  @override
  ConsumerState<_GitHubCard> createState() => _GitHubCardState();
}

class _GitHubCardState extends ConsumerState<_GitHubCard> {
  final _controller = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _connect() async {
    final token = _controller.text.trim();
    if (token.isEmpty) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      // The core validates against GitHub before storing, so a typo never
      // becomes the saved token.
      await ref.read(coreApiProvider).connectGithub(token);
      _controller.clear();
      ref
        ..invalidate(githubConnectedProvider)
        ..invalidate(githubAccountProvider);
    } catch (e) {
      if (mounted) setState(() => _error = describeError(e));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _disconnect() async {
    await ref.read(coreApiProvider).connectGithub('');
    ref
      ..invalidate(githubConnectedProvider)
      ..invalidate(githubAccountProvider);
  }

  @override
  Widget build(BuildContext context) {
    final connected = ref.watch(githubConnectedProvider).valueOrNull ?? false;
    final account = ref.watch(githubAccountProvider);

    return NeuSurface(
      radius: VonRadius.large,
      padding: const EdgeInsets.all(VonSpace.md),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  connected ? 'Connected' : 'Not connected',
                  style: VonText.heading,
                ),
              ),
              if (connected)
                switch (account) {
                  AsyncData(value: final a) when a.connected => NeuChip(
                      label: '@${a.login}',
                      icon: Icons.check_rounded,
                      tone: VonColors.success,
                    ),
                  // A stored token that no longer resolves is worth saying out
                  // loud: the push would otherwise fail at the worst moment.
                  AsyncError() => const NeuChip(
                      label: 'Token rejected',
                      icon: Icons.error_outline_rounded,
                      tone: VonColors.danger,
                    ),
                  AsyncLoading() => const NeuPulsingDots(size: 5),
                  // Data that says "not connected" while a token is stored
                  // shouldn't happen; show nothing rather than a spinner that
                  // would never stop.
                  _ => const SizedBox.shrink(),
                },
            ],
          ),
          const SizedBox(height: VonSpace.sm),
          Text(
            connected
                ? 'VON can create repositories under this account and push a '
                    'project as a single commit.'
                : 'Paste a personal access token with the `repo` scope. On '
                    'GitHub: Settings → Developer settings → Personal '
                    'access tokens.',
            style: VonText.secondary,
          ),
          const SizedBox(height: VonSpace.md),
          NeuField(
            controller: _controller,
            hint: connected ? 'Replace token' : 'ghp_… or github_pat_…',
            obscure: true,
            suffix: NeuIconButton(
              icon: Icons.link_rounded,
              size: 32,
              iconSize: 16,
              tooltip: 'Verify and connect',
              onPressed: _busy ? null : _connect,
            ),
          ),
          if (_busy) ...[
            const SizedBox(height: VonSpace.sm),
            Row(
              children: [
                const NeuPulsingDots(size: 5),
                const SizedBox(width: VonSpace.sm),
                Text('Checking the token…', style: VonText.caption),
              ],
            ),
          ],
          if (_error != null && !_busy) ...[
            const SizedBox(height: VonSpace.sm),
            Text(
              _error!,
              style: VonText.caption.copyWith(color: VonColors.danger),
            ),
          ],
          if (connected) ...[
            const SizedBox(height: VonSpace.sm),
            Align(
              alignment: Alignment.centerLeft,
              child: TextButton(
                onPressed: _disconnect,
                child: Text(
                  'Disconnect',
                  style: VonText.caption.copyWith(color: VonColors.danger),
                ),
              ),
            ),
          ],
        ],
      ),
    );
  }
}

class _ApiKeyCard extends ConsumerStatefulWidget {
  const _ApiKeyCard({
    required this.providerId,
    required this.providerName,
    required this.hintFormat,
    required this.models,
  });

  final String providerId;
  final String providerName;
  final String hintFormat;
  final List<CloudModelDto> models;

  @override
  ConsumerState<_ApiKeyCard> createState() => _ApiKeyCardState();
}

class _ApiKeyCardState extends ConsumerState<_ApiKeyCard> {
  final _controller = TextEditingController();
  bool _testing = false;
  String? _status;
  bool _statusOk = false;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _saveAndTest() async {
    final key = _controller.text.trim();
    if (key.isEmpty) return;
    setState(() {
      _testing = true;
      _status = null;
    });

    final api = ref.read(coreApiProvider);
    final model = widget.models.isNotEmpty ? widget.models.first.id : '';

    try {
      // Test before storing, so a bad key never becomes the saved one.
      await api.testApiKey(widget.providerId, key, model);
      await api.setApiKey(widget.providerId, key);
      _controller.clear();
      ref
        ..invalidate(cloudModelsProvider)
        ..invalidate(apiKeyHintProvider(widget.providerId));
      if (mounted) {
        setState(() {
          _status = 'Connected.';
          _statusOk = true;
        });
      }
    } catch (e) {
      if (mounted) {
        setState(() {
          _status = describeError(e);
          _statusOk = false;
        });
      }
    } finally {
      if (mounted) setState(() => _testing = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final hint = ref.watch(apiKeyHintProvider(widget.providerId)).valueOrNull;
    final configured = hint != null;

    return NeuSurface(
      radius: VonRadius.large,
      padding: const EdgeInsets.all(VonSpace.md),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(child: Text(widget.providerName, style: VonText.heading)),
              if (configured)
                NeuChip(
                  label: hint,
                  icon: Icons.key_rounded,
                  tone: VonColors.success,
                ),
            ],
          ),
          if (widget.models.isNotEmpty) ...[
            const SizedBox(height: VonSpace.sm),
            Wrap(
              spacing: VonSpace.sm,
              runSpacing: VonSpace.xs,
              children: [
                for (final model in widget.models)
                  NeuChip(label: model.id, concave: true),
              ],
            ),
          ],
          const SizedBox(height: VonSpace.md),
          NeuField(
            controller: _controller,
            hint: configured ? 'Replace key' : widget.hintFormat,
            obscure: true,
            suffix: NeuIconButton(
              icon: Icons.bolt_rounded,
              size: 32,
              iconSize: 16,
              tooltip: 'Save and test',
              onPressed: _testing ? null : _saveAndTest,
            ),
          ),
          if (_testing) ...[
            const SizedBox(height: VonSpace.sm),
            Row(
              children: [
                const NeuPulsingDots(size: 5),
                const SizedBox(width: VonSpace.sm),
                Text('Testing connection…', style: VonText.caption),
              ],
            ),
          ],
          if (_status != null && !_testing) ...[
            const SizedBox(height: VonSpace.sm),
            Text(
              _status!,
              style: VonText.caption.copyWith(
                color: _statusOk ? VonColors.success : VonColors.danger,
              ),
            ),
          ],
          if (configured) ...[
            const SizedBox(height: VonSpace.sm),
            Align(
              alignment: Alignment.centerLeft,
              child: TextButton(
                onPressed: () async {
                  await ref.read(coreApiProvider).setApiKey(widget.providerId, '');
                  ref
                    ..invalidate(cloudModelsProvider)
                    ..invalidate(apiKeyHintProvider(widget.providerId));
                },
                child: Text(
                  'Remove key',
                  style: VonText.caption.copyWith(color: VonColors.danger),
                ),
              ),
            ),
          ],
        ],
      ),
    );
  }
}
