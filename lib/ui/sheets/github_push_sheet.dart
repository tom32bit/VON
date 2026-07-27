import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../services/core_api.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';

/// Syncs a VON with a GitHub branch.
///
/// "Sync" rather than "push" is the honest word: the merge can change the VON
/// as well as the repository, and a sheet that only ever said "publish" would
/// be hiding half of what it does.
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
  bool _busy = false;
  String _stage = '';
  String? _error;

  /// Set when the failure was a merge conflict, which has its own advice.
  bool _conflicted = false;
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

  String get _branchOrMain =>
      _branch.text.trim().isEmpty ? 'main' : _branch.text.trim();

  void _run({required bool pullOnly}) {
    final repo = _repo.text.trim();
    if (repo.isEmpty || _busy) return;

    setState(() {
      _busy = true;
      _error = null;
      _conflicted = false;
      _result = null;
      _stage = 'Starting';
    });

    final api = ref.read(coreApiProvider);
    final stream = pullOnly
        ? api.pullFromGithub(
            projectId: widget.projectId,
            repo: repo,
            branch: _branchOrMain,
          )
        : api.pushToGithub(
            projectId: widget.projectId,
            repo: repo,
            branch: _branchOrMain,
            message: _message.text.trim(),
            private: _private,
          );

    _subscription = stream.listen(
      (event) {
        if (!mounted) return;
        switch (event.kind) {
          case 'progress':
            setState(() => _stage = event.detail);
          case 'done':
            setState(() {
              _result = event;
              _busy = false;
            });
            // A sync writes into the sandbox, so the Files tab is stale.
            if (event.pulled.isNotEmpty || event.removedLocally.isNotEmpty) {
              ref
                ..invalidate(fileTreeProvider(widget.projectId))
                ..invalidate(sandboxUsageProvider(widget.projectId));
            }
          case 'failed':
            setState(() {
              _error = event.detail;
              _conflicted = event.errorKind == 'conflict';
              _busy = false;
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
            _busy = false;
          });
        }
      },
      onDone: () {
        if (mounted && _busy) setState(() => _busy = false);
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
          result == null ? 'Sync with GitHub' : 'Synced',
          style: VonText.title,
        ),
        const SizedBox(height: VonSpace.sm),

        if (result != null)
          _Success(result: result)
        else ...[
          Text(
            'This is a merge, not an overwrite. Files added on GitHub since '
            'the last sync are brought into this VON; files deleted here are '
            'deleted there. Secrets and caches are left out automatically.',
            style: VonText.secondary,
          ),
          const SizedBox(height: VonSpace.lg),

          const _Label('Repository'),
          NeuField(
            controller: _repo,
            hint: 'my-project or owner/my-project',
            enabled: !_busy,
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
                    NeuField(controller: _branch, enabled: !_busy),
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
                              _busy ? null : (v) => setState(() => _private = v),
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
            enabled: !_busy,
          ),
          const SizedBox(height: VonSpace.lg),

          if (_busy)
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
            Row(
              children: [
                // Pull alone is the way in to an existing repository: it takes
                // the branch without committing anything, so there is no way to
                // publish a VON by accident while exploring.
                Expanded(
                  child: NeuButton(
                    label: 'Pull only',
                    icon: Icons.south_rounded,
                    tone: VonColors.textSecondary,
                    expand: true,
                    onPressed: () => _run(pullOnly: true),
                  ),
                ),
                const SizedBox(width: VonSpace.md),
                Expanded(
                  child: NeuButton(
                    label: 'Sync',
                    icon: Icons.sync_rounded,
                    expand: true,
                    glow: true,
                    onPressed: () => _run(pullOnly: false),
                  ),
                ),
              ],
            ),

          if (_error != null) ...[
            const SizedBox(height: VonSpace.md),
            if (_conflicted)
              _ConflictNotice(
                message: _error!,
                branch: _branchOrMain,
                onUseBranch: (name) {
                  _branch.text = name;
                  _run(pullOnly: false);
                },
              )
            else
              NeuErrorBanner(
                message: _error!,
                onRetry: () => _run(pullOnly: false),
              ),
          ],
        ],
      ],
    );
  }
}

/// Shown when the same file changed on both sides.
///
/// A retry button would be actively wrong here — the same call would fail the
/// same way. What helps is a way out, so this offers the branch: both versions
/// survive and GitHub's own review tools handle the reconciliation.
class _ConflictNotice extends StatelessWidget {
  const _ConflictNotice({
    required this.message,
    required this.branch,
    required this.onUseBranch,
  });

  final String message;
  final String branch;
  final ValueChanged<String> onUseBranch;

  @override
  Widget build(BuildContext context) {
    final suggestion = 'von/$branch-${DateTime.now().millisecondsSinceEpoch % 100000}';

    return NeuSurface(
      depth: NeuDepth.concave,
      radius: VonRadius.medium,
      borderColor: VonColors.warn,
      borderWidth: 1,
      padding: const EdgeInsets.all(VonSpace.md),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              const Icon(
                Icons.call_split_rounded,
                size: 16,
                color: VonColors.warn,
              ),
              const SizedBox(width: VonSpace.sm),
              Text('Changed in both places', style: VonText.heading),
            ],
          ),
          const SizedBox(height: VonSpace.sm),
          Text(message, style: VonText.secondary),
          const SizedBox(height: VonSpace.md),
          NeuButton(
            label: 'Push to $suggestion instead',
            icon: Icons.alt_route_rounded,
            compact: true,
            expand: true,
            onPressed: () => onUseBranch(suggestion),
          ),
        ],
      ),
    );
  }
}

class _Success extends StatelessWidget {
  const _Success({required this.result});

  final GitHubPushEventDto result;

  @override
  Widget build(BuildContext context) {
    final commitShown = result.commitUrl.isNotEmpty && !result.upToDate;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(result.detail, style: VonText.secondary),
        if (result.createdRepo) ...[
          const SizedBox(height: VonSpace.xs),
          Text(
            'The repository did not exist, so VON created it.',
            style: VonText.caption.copyWith(color: VonColors.accent),
          ),
        ],

        if (commitShown) ...[
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
        ],

        // A sync changes the VON too. Saying which files, rather than counting
        // them, is what stops that being a surprise the next time the Files tab
        // is opened.
        _PathList(
          title: 'Brought into this VON',
          tone: VonColors.accent,
          paths: result.pulled,
        ),
        _PathList(
          title: 'Deleted on GitHub, so removed here',
          tone: VonColors.danger,
          paths: result.removedLocally,
        ),
        _PathList(
          title: 'Left out on purpose',
          tone: VonColors.textSecondary,
          paths: result.skipped,
        ),

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

class _PathList extends StatelessWidget {
  const _PathList({
    required this.title,
    required this.tone,
    required this.paths,
  });

  final String title;
  final Color tone;
  final List<String> paths;

  @override
  Widget build(BuildContext context) {
    if (paths.isEmpty) return const SizedBox.shrink();
    return Padding(
      padding: const EdgeInsets.only(top: VonSpace.md),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(title, style: VonText.caption.copyWith(color: tone)),
          const SizedBox(height: VonSpace.xs),
          for (final path in paths)
            Padding(
              padding: const EdgeInsets.only(bottom: 2),
              child: Text('· $path', style: VonText.mono.copyWith(fontSize: 10)),
            ),
        ],
      ),
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
