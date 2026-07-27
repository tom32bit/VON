import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:share_plus/share_plus.dart';

import '../../core/errors.dart';
import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../models/chat.dart';
import '../../services/core_api.dart';
import '../sheets/github_push_sheet.dart';
import '../sheets/model_picker_sheet.dart';
import '../tabs/chat_tab.dart';
import '../tabs/files_tab.dart';
import '../tabs/preview_tab.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';

/// PRD Page 5: the Project Workspace.
class WorkspaceScreen extends ConsumerStatefulWidget {
  const WorkspaceScreen({super.key, required this.projectId});

  final String projectId;

  @override
  ConsumerState<WorkspaceScreen> createState() => _WorkspaceScreenState();
}

class _WorkspaceScreenState extends ConsumerState<WorkspaceScreen>
    with WidgetsBindingObserver {
  int _tab = 0;
  bool _exporting = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState lifecycle) {
    if (lifecycle != AppLifecycleState.resumed) return;
    // Coming back from the background, re-read the transcript from SQLite.
    // The foreground service should have kept the process alive, but Android
    // is allowed to kill it anyway — and a run that finished while we were
    // away has its result on disk and nowhere on screen. Skipped while a run
    // is live, because the stream is still the better source.
    final chat = ref.read(chatProvider(widget.projectId));
    if (chat.running) return;
    ref.read(chatProvider(widget.projectId).notifier).load().ignore();
    ref
      ..invalidate(fileTreeProvider(widget.projectId))
      ..invalidate(sandboxUsageProvider(widget.projectId));
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    // Free the port as soon as the workspace closes (SWE §3.2). Reads the
    // singleton directly rather than through `ref` — the container may already
    // be tearing down by the time dispose runs.
    CoreApi.instance.stopPreview(widget.projectId).ignore();
    super.dispose();
  }

  Future<void> _export() async {
    setState(() => _exporting = true);
    try {
      final path = await ref.read(coreApiProvider).exportProject(widget.projectId);
      if (!mounted) return;
      final project = ref.read(projectProvider(widget.projectId)).valueOrNull;
      final label =
          project == null ? 'VON' : 'VON ${project.ordinal} · ${project.name}';
      await Share.shareXFiles(
        [XFile(path, mimeType: 'application/zip')],
        subject: '$label export',
      );
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            backgroundColor: VonColors.bgElevated,
            content: Text(
              'Export failed: ${describeError(e)}',
              style: VonText.body,
            ),
          ),
        );
      }
    } finally {
      if (mounted) setState(() => _exporting = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final project = ref.watch(projectProvider(widget.projectId));
    final chat = ref.watch(chatProvider(widget.projectId));
    final isWeb = project.valueOrNull?.kind == 'web';

    final tabs = ['Chat', 'Files', if (isWeb) 'Preview'];
    final icons = [
      Icons.forum_outlined,
      Icons.folder_outlined,
      if (isWeb) Icons.public_rounded,
    ];
    // Guard against the tab index going stale if the project kind changes.
    final activeTab = _tab.clamp(0, tabs.length - 1);

    return Scaffold(
      body: SafeArea(
        child: Column(
          children: [
            // Accent shimmer sweeps the top of an active workspace
            // (Design.md §6, Screen 5).
            NeuShimmerBar(active: chat.running),
            _WorkspaceHeader(
              projectId: widget.projectId,
              project: project.valueOrNull,
              chat: chat,
              exporting: _exporting,
              onExport: _export,
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(
                VonSpace.gutter,
                0,
                VonSpace.gutter,
                VonSpace.md,
              ),
              child: NeuPillTabs(
                labels: tabs,
                icons: icons,
                index: activeTab,
                onChanged: (i) => setState(() => _tab = i),
              ),
            ),
            Expanded(
              child: IndexedStack(
                index: activeTab,
                children: [
                  ChatTab(projectId: widget.projectId),
                  FilesTab(projectId: widget.projectId),
                  if (isWeb) PreviewTab(projectId: widget.projectId),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _WorkspaceHeader extends ConsumerWidget {
  const _WorkspaceHeader({
    required this.projectId,
    required this.project,
    required this.chat,
    required this.exporting,
    required this.onExport,
  });

  final String projectId;
  final ProjectDto? project;
  final ChatState chat;
  final bool exporting;
  final VoidCallback onExport;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return Padding(
      // The stack here is dense by nature — shimmer bar, title, model id, tab
      // pills — and four rows with hairline gaps between them read as one
      // block of noise. The spacing below is what separates them into things.
      padding: const EdgeInsets.fromLTRB(
        VonSpace.sm,
        VonSpace.md,
        VonSpace.gutter,
        VonSpace.sm,
      ),
      child: Row(
        children: [
          NeuIconButton(
            icon: Icons.arrow_back_rounded,
            size: 38,
            iconSize: 18,
            tone: VonColors.textSecondary,
            onPressed: () => Navigator.of(context).pop(),
          ),
          const SizedBox(width: VonSpace.sm),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    if (project != null) ...[
                      Text(
                        'VON ${project!.ordinal}',
                        style: VonText.caption.copyWith(
                          color: VonColors.accent,
                          letterSpacing: 0.8,
                        ),
                      ),
                      Text(' · ', style: VonText.caption),
                    ],
                    Flexible(
                      child: Text(
                        project?.name ?? 'Initializing VON…',
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: VonText.title.copyWith(fontSize: 17),
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 3),
                Row(
                  children: [
                    Icon(
                      project?.provider == 'local'
                          ? Icons.smartphone_rounded
                          : Icons.cloud_outlined,
                      size: 11,
                      color: VonColors.textDisabled,
                    ),
                    const SizedBox(width: 4),
                    Flexible(
                      child: Text(
                        project?.modelId ?? '',
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: VonText.mono.copyWith(fontSize: 10),
                      ),
                    ),
                  ],
                ),
              ],
            ),
          ),
          const SizedBox(width: VonSpace.sm),
          _SwitchModelButton(projectId: projectId, project: project),
          const SizedBox(width: VonSpace.xs),
          _PublishButton(projectId: projectId, project: project),
          const SizedBox(width: VonSpace.xs),
          // Concave (disabled) until the agent has produced something, then it
          // pops out with an accent glow (Design.md §6, Screen 5).
          _DownloadButton(
            armed: chat.hasDeliverables && !chat.running && !exporting,
            busy: exporting,
            onPressed: onExport,
          ),
        ],
      ),
    );
  }
}

/// Syncs the VON with GitHub.
///
/// Hidden entirely until an account is connected rather than shown disabled: an
/// icon that does nothing invites tapping, and the place to fix it (the Model
/// Hub) is two screens away.
///
/// Unlike the export button next to it, this stays live for an empty VON — pull
/// is how you start *from* a repository, so requiring files first would lock
/// you out of the one case that needs it most.
class _PublishButton extends ConsumerWidget {
  const _PublishButton({required this.projectId, required this.project});

  final String projectId;
  final ProjectDto? project;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final connected = ref.watch(githubConnectedProvider).valueOrNull ?? false;
    if (!connected || project == null) return const SizedBox.shrink();

    return NeuIconButton(
      icon: Icons.sync_rounded,
      size: 38,
      iconSize: 18,
      tone: VonColors.textSecondary,
      tooltip: 'Sync this VON with GitHub',
      onPressed: () => showNeuSheet<void>(
        context: context,
        builder: (_) => GitHubPushSheet(
          projectId: projectId,
          suggestedRepo: repoSlug(project!.name, project!.ordinal.toInt()),
          suggestedMessage: project!.name.trim().isEmpty
              ? 'Update from VON'
              : project!.name.trim(),
        ),
      ),
    );
  }
}

class _DownloadButton extends StatelessWidget {
  const _DownloadButton({
    required this.armed,
    required this.busy,
    required this.onPressed,
  });

  final bool armed;
  final bool busy;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    if (busy) {
      return const NeuSurface(
        depth: NeuDepth.concave,
        radius: 19,
        width: 38,
        height: 38,
        child: Center(child: NeuPulsingDots(size: 4)),
      );
    }
    return NeuIconButton(
      icon: Icons.archive_outlined,
      size: 38,
      iconSize: 18,
      glow: armed,
      selected: !armed,
      tooltip: armed
          ? 'Export this VON as a zip'
          : 'Nothing to export yet — the agent has not created any files',
      onPressed: armed ? onPressed : null,
    );
  }
}

/// Switching provider mid-project is the hybrid-online/offline story: the
/// conversation and sandbox are untouched, so the next turn resumes with full
/// context on the new model.
class _SwitchModelButton extends ConsumerWidget {
  const _SwitchModelButton({required this.projectId, required this.project});

  final String projectId;
  final ProjectDto? project;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return NeuIconButton(
      icon: Icons.swap_horiz_rounded,
      size: 38,
      iconSize: 18,
      tone: VonColors.textSecondary,
      tooltip: 'Switch model',
      onPressed: project == null
          ? null
          : () async {
              // Straight to the picker rather than an explanatory sheet that
              // then opens one. Two stacked sheets on a phone leaves no room
              // for the list, and popping the first to make space means
              // reaching for a context that no longer exists.
              final choice = await showNeuSheet<ModelChoice>(
                context: context,
                builder: (_) => ModelPickerSheet(
                  currentProvider: project!.provider,
                  currentModelId: project!.modelId,
                ),
              );
              if (choice == null) return;
              // The conversation and the sandbox are untouched, so the next
              // turn resumes with full context on the new model.
              await ref
                  .read(projectsProvider.notifier)
                  .setModel(projectId, choice.provider, choice.modelId);
            },
    );
  }
}
