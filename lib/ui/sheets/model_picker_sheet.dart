import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/errors.dart';
import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../services/core_api.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';

/// What the picker hands back.
class ModelChoice {
  const ModelChoice(this.provider, this.modelId);

  final String provider;
  final String modelId;
}

/// Human labels for the provider tabs, in the order they are offered.
const _providers = <(String, String)>[
  ('local', 'On-device'),
  ('anthropic', 'Anthropic'),
  ('openai', 'OpenAI'),
  ('gemini', 'Gemini'),
  ('groq', 'Groq'),
  ('nvidia', 'NVIDIA'),
];

/// Picks a model, from the full list each provider actually offers.
///
/// Not a dropdown. Providers return dozens of models — NVIDIA NIM alone lists
/// far more than fits a menu — so this is a searchable list with the provider
/// as a tab. The curated ones float to the top with their price and blurb; the
/// rest are listed by id, because being able to reach a model VON has never
/// heard of is the entire point.
///
/// The last resort is the text field: if a provider ships something between
/// releases, or an account has access to a private deployment, typing the id
/// works without waiting for VON to catch up.
class ModelPickerSheet extends ConsumerStatefulWidget {
  const ModelPickerSheet({
    super.key,
    this.currentProvider = 'local',
    this.currentModelId = '',
  });

  final String currentProvider;
  final String currentModelId;

  @override
  ConsumerState<ModelPickerSheet> createState() => _ModelPickerSheetState();
}

class _ModelPickerSheetState extends ConsumerState<ModelPickerSheet> {
  late String _provider = widget.currentProvider;
  final _search = TextEditingController();
  final _custom = TextEditingController();
  bool _customOpen = false;

  @override
  void dispose() {
    _search.dispose();
    _custom.dispose();
    super.dispose();
  }

  void _choose(String provider, String modelId) {
    if (modelId.trim().isEmpty) return;
    Navigator.of(context).pop(ModelChoice(provider, modelId.trim()));
  }

  @override
  Widget build(BuildContext context) {
    final isLocal = _provider == 'local';

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Expanded(child: Text('Choose a model', style: VonText.title)),
            if (!isLocal)
              NeuIconButton(
                icon: Icons.refresh_rounded,
                size: 34,
                iconSize: 16,
                tone: VonColors.textSecondary,
                tooltip: 'Fetch the list again',
                onPressed: () =>
                    ref.invalidate(providerModelsProvider(_provider)),
              ),
          ],
        ),
        const SizedBox(height: VonSpace.md),

        // Horizontal because six providers do not fit across a phone, and
        // wrapping them would push the list itself off the screen.
        SizedBox(
          height: 34,
          child: ListView.separated(
            scrollDirection: Axis.horizontal,
            itemCount: _providers.length,
            separatorBuilder: (_, __) => const SizedBox(width: VonSpace.sm),
            itemBuilder: (context, i) {
              final (id, label) = _providers[i];
              return NeuPressable(
                onTap: () => setState(() => _provider = id),
                selected: _provider == id,
                radius: VonRadius.small,
                intensity: 0.7,
                padding: const EdgeInsets.symmetric(horizontal: VonSpace.md),
                child: Center(
                  child: Text(
                    label,
                    style: VonText.caption.copyWith(
                      color: _provider == id
                          ? VonColors.accentText
                          : VonColors.textSecondary,
                    ),
                  ),
                ),
              );
            },
          ),
        ),
        const SizedBox(height: VonSpace.md),

        NeuField(
          controller: _search,
          hint: 'Search models',
          onChanged: (_) => setState(() {}),
        ),
        const SizedBox(height: VonSpace.md),

        ConstrainedBox(
          // Bounded so the sheet cannot grow past the screen on a provider
          // that returns a hundred models.
          constraints: BoxConstraints(
            maxHeight: MediaQuery.sizeOf(context).height * 0.42,
          ),
          child: isLocal ? _localList() : _cloudList(),
        ),

        const SizedBox(height: VonSpace.md),
        if (!isLocal) _customEntry(),
      ],
    );
  }

  Widget _localList() {
    final models = ref.watch(offlineModelsProvider).valueOrNull ?? const [];
    final downloaded = models.where((m) => m.downloaded).where(_matchesLocal);

    if (downloaded.isEmpty) {
      return _Note(
        models.any((m) => m.downloaded)
            ? 'No on-device model matches that search.'
            : 'No weights downloaded yet. The Model Hub has the catalogue.',
      );
    }
    return ListView(
      shrinkWrap: true,
      children: [
        for (final model in downloaded)
          _ModelRow(
            title: model.name,
            subtitle: '${model.quant} · ${_compact(model.contextLen.toInt())} ctx',
            selected: widget.currentProvider == 'local' &&
                widget.currentModelId == model.id,
            onTap: () => _choose('local', model.id),
          ),
      ],
    );
  }

  bool _matchesLocal(OfflineModelDto model) {
    final query = _search.text.trim().toLowerCase();
    return query.isEmpty ||
        model.name.toLowerCase().contains(query) ||
        model.id.toLowerCase().contains(query);
  }

  Widget _cloudList() {
    final async = ref.watch(providerModelsProvider(_provider));

    return switch (async) {
      AsyncLoading() => const Padding(
          padding: EdgeInsets.symmetric(vertical: VonSpace.lg),
          child: Center(child: NeuPulsingDots()),
        ),
      // A rejected key is the common case here, and it is worth saying plainly
      // rather than showing an empty list.
      AsyncError(:final error) => _Note(
          '${describeError(error)}\n\nYou can still type a model id below.',
          tone: VonColors.danger,
        ),
      AsyncData(value: final models) => _buildRows(models),
      _ => const SizedBox.shrink(),
    };
  }

  Widget _buildRows(List<CloudModelDto> models) {
    final query = _search.text.trim().toLowerCase();
    final matches = models.where((m) {
      return query.isEmpty ||
          m.id.toLowerCase().contains(query) ||
          m.name.toLowerCase().contains(query);
    }).toList();

    if (matches.isEmpty) {
      return _Note(
        models.isEmpty
            ? 'This provider returned no models for your key.'
            : 'Nothing matches "${_search.text.trim()}". Try a shorter search, '
                'or type the id below.',
      );
    }

    return ListView.builder(
      shrinkWrap: true,
      itemCount: matches.length,
      itemBuilder: (context, i) {
        final model = matches[i];
        final priced = model.inputPrice >= 0 && model.outputPrice >= 0;
        final parts = <String>[
          if (model.name != model.id) model.id,
          if (model.contextTokens.toInt() > 0)
            '${_compact(model.contextTokens.toInt())} ctx',
          if (priced)
            '\$${model.inputPrice.toStringAsFixed(2)}/\$${model.outputPrice.toStringAsFixed(2)} per Mtok',
        ];
        return _ModelRow(
          title: model.name,
          subtitle: parts.join(' · '),
          blurb: model.blurb,
          selected: widget.currentProvider == model.provider &&
              widget.currentModelId == model.id,
          onTap: () => _choose(model.provider, model.id),
        );
      },
    );
  }

  Widget _customEntry() {
    if (!_customOpen) {
      return Align(
        alignment: Alignment.centerLeft,
        child: TextButton(
          onPressed: () => setState(() => _customOpen = true),
          child: Text(
            'Enter a model id by hand',
            style: VonText.caption.copyWith(color: VonColors.accentText),
          ),
        ),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          'Exactly as the provider spells it — VON does not check it until the '
          'first message.',
          style: VonText.caption,
        ),
        const SizedBox(height: VonSpace.sm),
        NeuField(
          controller: _custom,
          hint: 'model-id',
          autofocus: true,
          onSubmitted: (value) => _choose(_provider, value),
          suffix: NeuIconButton(
            icon: Icons.check_rounded,
            size: 32,
            iconSize: 16,
            tooltip: 'Use this id',
            onPressed: () => _choose(_provider, _custom.text),
          ),
        ),
      ],
    );
  }

  /// `i64` crosses the bridge as PlatformInt64, so callers pass `.toInt()` —
  /// the same convention the rest of the UI already follows for byte counts.
  static String _compact(int n) {
    if (n >= 1000000) {
      return '${(n / 1000000).toStringAsFixed(n % 1000000 == 0 ? 0 : 1)}M';
    }
    if (n >= 1000) return '${(n / 1000).round()}K';
    return '$n';
  }
}

class _ModelRow extends StatelessWidget {
  const _ModelRow({
    required this.title,
    required this.subtitle,
    required this.selected,
    required this.onTap,
    this.blurb = '',
  });

  final String title;
  final String subtitle;
  final String blurb;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(bottom: VonSpace.sm),
      child: NeuPressable(
        onTap: onTap,
        selected: selected,
        radius: VonRadius.medium,
        intensity: 0.8,
        padding: const EdgeInsets.symmetric(
          horizontal: VonSpace.md,
          vertical: 10,
        ),
        child: Row(
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(title, style: VonText.body, maxLines: 1),
                  if (subtitle.isNotEmpty)
                    Text(
                      subtitle,
                      style: VonText.mono.copyWith(fontSize: 10),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                  if (blurb.isNotEmpty)
                    Text(blurb, style: VonText.caption, maxLines: 2),
                ],
              ),
            ),
            if (selected)
              const Icon(
                Icons.check_rounded,
                size: 16,
                color: VonColors.accent,
              ),
          ],
        ),
      ),
    );
  }
}

class _Note extends StatelessWidget {
  const _Note(this.text, {this.tone});

  final String text;
  final Color? tone;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.symmetric(vertical: VonSpace.md),
        child: Text(
          text,
          style: VonText.secondary.copyWith(color: tone),
        ),
      );
}
