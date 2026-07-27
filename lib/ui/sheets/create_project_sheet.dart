import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/errors.dart';
import '../../core/providers.dart';
import '../../core/theme.dart';
import '../screens/model_hub_screen.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';
import 'device_profiler_sheet.dart';
import 'model_picker_sheet.dart';

/// Creates a VON, choosing its type and model.
///
/// The Device Profiler gate (PRD Page 4) fires here rather than at run time:
/// finding out the phone can't run the model *after* writing a prompt is the
/// frustrating version of this.
class CreateProjectSheet extends ConsumerStatefulWidget {
  const CreateProjectSheet({super.key});

  @override
  ConsumerState<CreateProjectSheet> createState() => _CreateProjectSheetState();
}

class _CreateProjectSheetState extends ConsumerState<CreateProjectSheet> {
  final _name = TextEditingController();
  final _description = TextEditingController();

  String _kind = 'general';
  String _provider = 'local';
  String _modelId = '';
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _name.dispose();
    _description.dispose();
    super.dispose();
  }

  /// Weight of the project type, used by the profiler gate.
  String get _weight => switch (_kind) {
        'data' => 'heavy',
        'web' => 'moderate',
        _ => 'light',
      };

  Future<void> _submit() async {
    final name = _name.text.trim();
    if (name.isEmpty) {
      setState(() => _error = 'Give this VON a name.');
      return;
    }
    if (_modelId.isEmpty) {
      setState(() => _error = 'Pick a model for this VON.');
      return;
    }

    setState(() {
      _busy = true;
      _error = null;
    });

    try {
      // Ask Rust whether this hardware can take the job before committing.
      final blocked = await ref
          .read(coreApiProvider)
          .checkTaskAllowed(weight: _weight, provider: _provider);

      if (blocked.isNotEmpty && mounted) {
        setState(() => _busy = false);
        await showNeuSheet<void>(
          context: context,
          builder: (_) => DeviceProfilerSheet(
            blockedReason: blocked,
            // The cloud alternative: switch the project rather than abandon it.
            onUseCloud: () => setState(() {
              _provider = 'anthropic';
              _modelId = '';
              _error = 'Pick a cloud model to continue.';
            }),
          ),
        );
        return;
      }

      final project = await ref.read(projectsProvider.notifier).create(
            name: name,
            description: _description.text.trim(),
            kind: _kind,
            provider: _provider,
            modelId: _modelId,
          );
      if (mounted) Navigator.of(context).pop(project);
    } catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = describeError(e);
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return SingleChildScrollView(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text('New VON', style: VonText.title),
          const SizedBox(height: VonSpace.lg),

          NeuField(
            controller: _name,
            // A short label, not a brief — the description field below asks
            // for the task.
            hint: 'Name this VON',
            autofocus: true,
          ),
          const SizedBox(height: VonSpace.md),
          NeuField(
            controller: _description,
            hint: 'What should the agent build? (optional)',
            maxLines: 3,
            minLines: 2,
          ),

          const SizedBox(height: VonSpace.lg),
          Text('TYPE', style: VonText.caption),
          const SizedBox(height: VonSpace.sm),
          _KindPicker(
            value: _kind,
            onChanged: (value) => setState(() => _kind = value),
          ),

          const SizedBox(height: VonSpace.lg),
          Text('MODEL', style: VonText.caption),
          const SizedBox(height: VonSpace.sm),
          _ModelPicker(
            provider: _provider,
            modelId: _modelId,
            onChanged: (provider, modelId) => setState(() {
              _provider = provider;
              _modelId = modelId;
              _error = null;
            }),
          ),

          if (_error != null) ...[
            const SizedBox(height: VonSpace.md),
            NeuErrorBanner(
              message: _error!,
              onDismiss: () => setState(() => _error = null),
            ),
          ],

          const SizedBox(height: VonSpace.lg),
          NeuButton(
            label: _busy ? 'Initializing VON…' : 'Create VON',
            icon: Icons.auto_awesome_rounded,
            expand: true,
            glow: !_busy,
            onPressed: _busy ? null : _submit,
          ),
        ],
      ),
    );
  }
}

class _KindPicker extends StatelessWidget {
  const _KindPicker({required this.value, required this.onChanged});

  final String value;
  final ValueChanged<String> onChanged;

  static const _kinds = [
    ('general', 'General', Icons.category_outlined),
    ('web', 'Web', Icons.language_rounded),
    ('data', 'Data', Icons.insights_rounded),
    ('docs', 'Docs', Icons.article_outlined),
  ];

  @override
  Widget build(BuildContext context) {
    return Wrap(
      spacing: VonSpace.sm,
      runSpacing: VonSpace.sm,
      children: [
        for (final (id, label, icon) in _kinds)
          NeuPressable(
            onTap: () => onChanged(id),
            selected: value == id,
            radius: VonRadius.medium,
            intensity: 0.8,
            padding: const EdgeInsets.symmetric(
              horizontal: VonSpace.md,
              vertical: 10,
            ),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  icon,
                  size: 15,
                  color: value == id ? VonColors.accent : VonColors.textSecondary,
                ),
                const SizedBox(width: 6),
                Text(
                  label,
                  style: VonText.button.copyWith(
                    fontSize: 13,
                    color:
                        value == id ? VonColors.accent : VonColors.textSecondary,
                  ),
                ),
              ],
            ),
          ),
      ],
    );
  }
}

/// Lists downloaded offline models and cloud models with a configured key.
/// Anything unavailable is shown with the reason and a route to fix it, rather
/// than hidden — otherwise the list looks arbitrarily short.
/// Shows the chosen model and opens the full picker.
///
/// A summary row rather than an inline list: the list is now everything the
/// provider offers, which is dozens of rows, and this sheet already has a name,
/// a description and a project type above it.
class _ModelPicker extends ConsumerWidget {
  const _ModelPicker({
    required this.provider,
    required this.modelId,
    required this.onChanged,
  });

  final String provider;
  final String modelId;
  final void Function(String provider, String modelId) onChanged;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final chosen = modelId.isNotEmpty;
    final offline = ref.watch(offlineModelsProvider).valueOrNull ?? const [];
    final anyLocal = offline.any((m) => m.downloaded);
    final anyKey = (ref.watch(cloudModelsProvider).valueOrNull ?? const [])
        .any((m) => m.keyConfigured);

    if (!anyLocal && !anyKey) {
      return NeuSurface(
        depth: NeuDepth.concave,
        radius: VonRadius.medium,
        padding: const EdgeInsets.all(VonSpace.md),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('No models available yet', style: VonText.heading),
            const SizedBox(height: VonSpace.xs),
            Text(
              'Download an on-device model, or add an API key for a cloud '
              'provider.',
              style: VonText.secondary,
            ),
            const SizedBox(height: VonSpace.md),
            NeuButton(
              label: 'Open Model Hub',
              icon: Icons.memory_rounded,
              compact: true,
              onPressed: () => Navigator.of(context).push(
                MaterialPageRoute(builder: (_) => const ModelHubScreen()),
              ),
            ),
          ],
        ),
      );
    }

    return NeuPressable(
      radius: VonRadius.medium,
      intensity: 0.8,
      padding: const EdgeInsets.symmetric(
        horizontal: VonSpace.md,
        vertical: 12,
      ),
      onTap: () async {
        final choice = await showNeuSheet<ModelChoice>(
          context: context,
          builder: (_) => ModelPickerSheet(
            currentProvider: provider,
            currentModelId: modelId,
          ),
        );
        if (choice != null) onChanged(choice.provider, choice.modelId);
      },
      child: Row(
        children: [
          Icon(
            provider == 'local'
                ? Icons.smartphone_rounded
                : Icons.cloud_outlined,
            size: 16,
            color: provider == 'local'
                ? VonColors.success
                : VonColors.accentText,
          ),
          const SizedBox(width: VonSpace.sm),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  chosen ? modelId : 'Choose a model',
                  style: VonText.body.copyWith(fontWeight: FontWeight.w600),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
                Text(
                  chosen
                      ? (provider == 'local' ? 'On-device' : provider)
                      : 'On-device weights, or anything your keys can reach',
                  style: VonText.caption,
                ),
              ],
            ),
          ),
          const Icon(
            Icons.chevron_right_rounded,
            size: 18,
            color: VonColors.textSecondary,
          ),
        ],
      ),
    );
  }
}
