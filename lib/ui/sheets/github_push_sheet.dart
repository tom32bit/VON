import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../services/core_api.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';

/// Publishes a VON to GitHub as a single commit.
///
/// Everything the push needs is on one screen — repository, branch, message,
/// visibility — because the alternative is a wizard, and this is three fields
/// with sensible defaults. The user can press Publish without touching any of
/// them.
class GitHubPushSheet extends ConsumerStatefulWidget {
  const GitHubPushSheet({
    super.key,
    required this.projectId,
    required this.suggestedRepo,
    required this.suggestedMessage,
  });

  final String projectId;

  /// Slug derived from the VON's name, so the common case is one tap.
  final String suggestedRepo;
  final String suggestedMessage;

  @override
  ConsumerState<GitHubPushSheet> createState() => _GitHubPushSheetState();
}

class _GitHubPushSheetState extends ConsumerState<GitHubPushSheet> {
  late final _repo = TextEditingController(text: widget.suggestedRepo);
  late final _branch = TextEditingController(text: 'main');
  late final _message = TextEditingController(text: widget.suggestedMessage);

  bool _private = true;
  bool _pushing = false;
  String _stage = '';
  String? _error;
  GitHubPushEventDto? _result;
  StreamSubscription<GitHubPushEventDto>? _subscription;

  @override
  void dispose() {
    _subscription?.cancel();
    _repo.dispose();
    _branch.dispose();
    _message.dispose();
    super.dispose();
  }

  void _push() {
    final repo = _repo.text.trim();
    if (repo.isEmpty || _pushing) return;

    setState(() {
      _pushing = true;
      _error = null;
      _result = null;
      _stage = 'Starting';
    });

    _subscription = ref
        .read(coreApiProvider)
        .pushToGithub(
          projectId: widget.projectId,
          repo: repo,
          branch: _branch.text.trim().isEmpty ? 'main' : _branch.text.trim(),
          message: _message.text.trim(),
          private: _private,
        )
        .listen(
      (event) {
        if (!mounted) return;
        switch (event.kind) {
          case 'progress':
            setState(() => _stage = event.detail);
          case 'done':
            setState(() {
              _result = event;
              _pushing = false;
            });
          case 'failed':
            setState(() {
              _error = event.detail;
              _pushing = false;
            });
        }
      },
      // The stream also carries the failure as an error; the `failed` event has
      // already set a better message, so this only catches the case where the
      // bridge itself gave up.
      onError: (Object e) {
        if (mounted && _error == null) {
          setState(() {
            _error = e.toString();
            _pushing = false;
          });
        }
      },
      onDone: () {
        if (mounted && _pushing) setState(() => _pushing = false);
      },
    );
  }

  @override
  Widget build(BuildContext context) {
    final result = _result;

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          result == null ? 'Publish to GitHub' : 'Published',
          style: VonText.title,
        ),
        const SizedBox(height: VonSpace.sm),

        if (result != null)
          _Success(result: result)
        else ...[
          Text(
            'Everything in this VON goes up as one commit. Secrets, '
            '`node_modules` and caches are left behind automatically.',
            style: VonText.secondary,
          ),
          const SizedBox(height: VonSpace.lg),

          const _Label('Repository'),
          NeuField(
            controller: _repo,
            hint: 'my-project or owner/my-project',
            enabled: !_pushing,
          ),
          const SizedBox(height: VonSpace.xs),
          Text(
            'A bare name is created under your account if it does not exist. '
            'Use owner/name for one that already does.',
            style: VonText.caption,
          ),
          const SizedBox(height: VonSpace.md),

          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                flex: 2,
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const _Label('Branch'),
                    NeuField(controller: _branch, enabled: !_pushing),
                  ],
                ),
              ),
              const SizedBox(width: VonSpace.md),
              Expanded(
                flex: 3,
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const _Label('Private'),
                    SizedBox(
                      height: 44,
                      child: Align(
                        alignment: Alignment.centerLeft,
                        child: NeuToggle(
                          value: _private,
                          onChanged:
                              _pushing ? null : (v) => setState(() => _private = v),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ],
          ),
          const SizedBox(height: VonSpace.md),

          const _Label('Commit message'),
          NeuField(
            controller: _message,
            maxLines: 3,
            minLines: 1,
            enabled: !_pushing,
          ),
          const SizedBox(height: VonSpace.lg),

          if (_pushing)
            Row(
              children: [
                const NeuPulsingDots(size: 5),
                const SizedBox(width: VonSpace.sm),
                Expanded(
                  child: Text(
                    '$_stage…',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: VonText.caption,
                  ),
                ),
              ],
            )
          else
            NeuButton(
              label: 'Publish',
              icon: Icons.cloud_upload_outlined,
              expand: true,
              glow: true,
              onPressed: _push,
            ),

          if (_error != null) ...[
            const SizedBox(height: VonSpace.md),
            NeuErrorBanner(message: _error!, onRetry: _push),
          ],
        ],
      ],
    );
  }
}

class _Success extends StatelessWidget {
  const _Success({required this.result});

  final GitHubPushEventDto result;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          '${result.filesPushed} file${result.filesPushed == 1 ? '' : 's'} '
          'on ${result.repoFullName} · ${result.branch}',
          style: VonText.secondary,
        ),
        if (result.createdRepo) ...[
          const SizedBox(height: VonSpace.xs),
          Text(
            'The repository did not exist, so VON created it.',
            style: VonText.caption.copyWith(color: VonColors.accent),
          ),
        ],
        const SizedBox(height: VonSpace.md),

        // The URL is shown in full and copyable rather than opened: a phone
        // browser handoff mid-flow loses the sheet, and the commit is the one
        // thing worth keeping.
        NeuSurface(
          depth: NeuDepth.concave,
          radius: VonRadius.medium,
          padding: const EdgeInsets.all(VonSpace.md),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  result.commitUrl,
                  style: VonText.mono.copyWith(fontSize: 11),
                ),
              ),
              const SizedBox(width: VonSpace.sm),
              NeuIconButton(
                icon: Icons.copy_rounded,
                size: 34,
                iconSize: 15,
                tooltip: 'Copy the commit URL',
                onPressed: () {
                  Clipboard.setData(ClipboardData(text: result.commitUrl));
                  ScaffoldMessenger.of(context).showSnackBar(
                    SnackBar(
                      backgroundColor: VonColors.bgElevated,
                      content: Text('Copied.', style: VonText.body),
                    ),
                  );
                },
              ),
            ],
          ),
        ),

        // Named rather than counted, so a file missing from the repo is never a
        // surprise discovered later.
        if (result.skipped.isNotEmpty) ...[
          const SizedBox(height: VonSpace.md),
          Text('Left out on purpose', style: VonText.heading),
          const SizedBox(height: VonSpace.xs),
          for (final reason in result.skipped)
            Padding(
              padding: const EdgeInsets.only(bottom: 2),
              child: Text('· $reason', style: VonText.caption),
            ),
        ],

        const SizedBox(height: VonSpace.lg),
        NeuButton(
          label: 'Done',
          expand: true,
          onPressed: () => Navigator.of(context).pop(),
        ),
      ],
    );
  }
}

class _Label extends StatelessWidget {
  const _Label(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.only(bottom: VonSpace.xs),
        child: Text(text.toUpperCase(), style: VonText.caption),
      );
}

/// Turns a VON's name into something GitHub accepts as a repository name.
///
/// Only letters, digits, dot, hyphen and underscore survive; anything else
/// becomes a hyphen, and runs collapse. An empty result falls back to the
/// ordinal, because a suggestion the user has to delete first is worse than a
/// dull one.
String repoSlug(String name, int ordinal) {
  final slug = name
      .toLowerCase()
      .replaceAll(RegExp(r'[^a-z0-9._-]+'), '-')
      .replaceAll(RegExp(r'-{2,}'), '-')
      .replaceAll(RegExp(r'^[-.]+|[-.]+$'), '');
  return slug.isEmpty ? 'von-$ordinal' : slug;
}
