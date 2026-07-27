import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:share_plus/share_plus.dart';

import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../models/chat.dart';
import '../../services/core_api.dart';
import '../tabs/chat_tab.dart';
import '../tabs/files_tab.dart';
import '../sheets/github_push_sheet.dart';
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

class _WorkspaceScreenState extends ConsumerState<WorkspaceScreen> {
  int _tab = 0;
  bool _exporting = false;

  @override
  void dispose() {
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
            content: Text('Export failed: $e', style: VonText.body),
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
                VonSpace.xs,
                VonSpace.gutter,
                VonSpace.sm,
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
      padding: const EdgeInsets.fromLTRB(
        VonSpace.sm,
        VonSpace.sm,
        VonSpace.gutter,
        0,
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
                Row(
                  children: [
                    Icon(
                      project?.provider == 'local'
                          ? Icons.smartphone_rounded
                          : Icons.cloud_outlined,
                      size: 10,
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
          : () => showNeuSheet<void>(
                context: context,
                builder: (_) => _SwitchModelSheet(
                  projectId: projectId,
                  current: project!,
                ),
              ),
    );
  }
}

class _SwitchModelSheet extends ConsumerWidget {
  const _SwitchModelSheet({required this.projectId, required this.current});

  final String projectId;
  final ProjectDto current;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final offline = ref.watch(offlineModelsProvider).valueOrNull ?? const [];
    final cloud = ref.watch(cloudModelsProvider).valueOrNull ?? const [];

    Future<void> pick(String provider, String modelId) async {
      await ref
          .read(projectsProvider.notifier)
          .setModel(projectId, provider, modelId);
      if (context.mounted) Navigator.of(context).pop();
    }

    return SingleChildScrollView(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text('Switch model', style: VonText.title),
          const SizedBox(height: VonSpace.xs),
          Text(
            'This VON keeps its conversation and every file — the agent '
            'picks up exactly where it left off.',
            style: VonText.secondary,
          ),
          const SizedBox(height: VonSpace.lg),
          for (final model in offline.where((m) => m.downloaded))
            _SwitchRow(
              name: model.name,
              detail: 'On-device · ${model.quant}',
              icon: Icons.smartphone_rounded,
              tone: VonColors.success,
              selected:
                  current.provider == 'local' && current.modelId == model.id,
              onTap: () => pick('local', model.id),
            ),
          for (final model in cloud.where((m) => m.keyConfigured))
            _SwitchRow(
              name: model.name,
              detail: model.id,
              icon: Icons.cloud_outlined,
              tone: VonColors.accentText,
              selected: current.modelId == model.id,
              onTap: () => pick(model.provider, model.id),
            ),
        ],
      ),
    );
  }
}

class _SwitchRow extends StatelessWidget {
  const _SwitchRow({
    required this.name,
    required this.detail,
    required this.icon,
    required this.tone,
    required this.selected,
    required this.onTap,
  });

  final String name;
  final String detail;
  final IconData icon;
  final Color tone;
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
          vertical: 12,
        ),
        child: Row(
          children: [
            Icon(icon, size: 16, color: tone),
            const SizedBox(width: VonSpace.sm),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    name,
                    style: VonText.body.copyWith(fontWeight: FontWeight.w600),
                  ),
                  Text(detail, style: VonText.mono),
                ],
              ),
            ),
            if (selected)
              const Icon(
                Icons.check_circle_rounded,
                size: 18,
                color: VonColors.accent,
              ),
          ],
        ),
      ),
    );
  }
}
