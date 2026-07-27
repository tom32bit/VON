import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:intl/intl.dart';

import '../../core/errors.dart';
import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../services/core_api.dart';
import '../sheets/create_project_sheet.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';
import 'model_hub_screen.dart';
import 'workspace_screen.dart';

/// PRD Page 3: the VON grid.
class DashboardScreen extends ConsumerWidget {
  const DashboardScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final projects = ref.watch(projectsProvider);
    // Two columns on a phone, three on a tablet (Design.md §6, Screen 2).
    final columns = MediaQuery.sizeOf(context).width >= 720 ? 3 : 2;

    return Scaffold(
      body: SafeArea(
        child: Column(
          children: [
            const _DashboardHeader(),
            Expanded(
              child: RefreshIndicator(
                color: VonColors.accent,
                backgroundColor: VonColors.bgSurface,
                onRefresh: () async => ref.invalidate(projectsProvider),
                child: switch (projects) {
                  AsyncLoading() => const _ProjectGridSkeleton(),
                  AsyncError(:final error) => ListView(
                      padding: const EdgeInsets.symmetric(
                        horizontal: VonSpace.gutter,
                        vertical: VonSpace.md,
                      ),
                      children: [
                        NeuErrorBanner(
                          message: 'Could not load your VONs: ${describeError(error)}',
                          onRetry: () => ref.invalidate(projectsProvider),
                        ),
                      ],
                    ),
                  AsyncData(value: final list) when list.isEmpty =>
                    _EmptyDashboard(onCreate: () => _create(context, ref)),
                  AsyncData(value: final list) => GridView.builder(
                      padding: const EdgeInsets.fromLTRB(
                        VonSpace.gutter,
                        VonSpace.sm,
                        VonSpace.gutter,
                        // Room for the FAB to float clear of the last row.
                        96,
                      ),
                      gridDelegate:
                          SliverGridDelegateWithFixedCrossAxisCount(
                        crossAxisCount: columns,
                        crossAxisSpacing: VonSpace.md,
                        mainAxisSpacing: VonSpace.md,
                        childAspectRatio: 0.76,
                      ),
                      itemCount: list.length,
                      itemBuilder: (context, i) => _ProjectCard(
                        project: list[i],
                        onOpen: () => _open(context, list[i]),
                        onRename: () => _rename(context, ref, list[i]),
                        onDelete: () => _delete(context, ref, list[i]),
                      ),
                    ),
                  _ => const SizedBox.shrink(),
                },
              ),
            ),
          ],
        ),
      ),
      floatingActionButton: NeuIconButton(
        icon: Icons.add_rounded,
        size: 62,
        iconSize: 28,
        glow: true,
        tooltip: 'Create a VON',
        onPressed: () => _create(context, ref),
      ),
    );
  }

  void _open(BuildContext context, ProjectDto project) {
    Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => WorkspaceScreen(projectId: project.id),
      ),
    );
  }

  Future<void> _create(BuildContext context, WidgetRef ref) async {
    final created = await showNeuSheet<ProjectDto>(
      context: context,
      builder: (_) => const CreateProjectSheet(),
    );
    if (created != null && context.mounted) _open(context, created);
  }

  Future<void> _rename(
    BuildContext context,
    WidgetRef ref,
    ProjectDto project,
  ) async {
    final controller = TextEditingController(text: project.name);
    final name = await showNeuSheet<String>(
      context: context,
      builder: (sheetContext) => Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // Only the name changes — the VON number is permanent.
          Text('Rename VON ${project.ordinal}', style: VonText.title),
          const SizedBox(height: VonSpace.lg),
          NeuField(controller: controller, autofocus: true),
          const SizedBox(height: VonSpace.lg),
          NeuButton(
            label: 'Save',
            expand: true,
            onPressed: () =>
                Navigator.of(sheetContext).pop(controller.text.trim()),
          ),
        ],
      ),
    );
    if (name != null && name.isNotEmpty) {
      await ref.read(projectsProvider.notifier).rename(project.id, name);
    }
  }

  Future<void> _delete(
    BuildContext context,
    WidgetRef ref,
    ProjectDto project,
  ) async {
    final confirmed = await showNeuSheet<bool>(
      context: context,
      builder: (sheetContext) => Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text(
            'Delete VON ${project.ordinal}?',
            style: VonText.title,
          ),
          const SizedBox(height: VonSpace.sm),
          Text(
            'This removes "${project.name}" — the conversation and every file '
            'inside it (${_formatBytes(project.sizeBytes.toInt())}). It cannot '
            'be undone, and VON ${project.ordinal} will not be reused. Export '
            'it first if you want to keep the work.',
            style: VonText.secondary,
          ),
          const SizedBox(height: VonSpace.lg),
          Row(
            children: [
              Expanded(
                child: NeuButton(
                  label: 'Cancel',
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
      await ref.read(projectsProvider.notifier).delete(project.id);
    }
  }
}

class _DashboardHeader extends ConsumerWidget {
  const _DashboardHeader();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final stats = ref.watch(deviceStatsProvider);

    return Padding(
      padding: const EdgeInsets.fromLTRB(
        VonSpace.gutter,
        VonSpace.md,
        VonSpace.gutter,
        VonSpace.sm,
      ),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text('VONs', style: VonText.display),
                const SizedBox(height: 2),
                stats.maybeWhen(
                  data: (value) => _ThermalLine(stats: value),
                  orElse: () =>
                      Text('Isolated on-device sandboxes', style: VonText.caption),
                ),
              ],
            ),
          ),
          NeuIconButton(
            icon: Icons.memory_rounded,
            tooltip: 'Model Hub',
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute(builder: (_) => const ModelHubScreen()),
            ),
          ),
        ],
      ),
    );
  }
}

/// One-line device readout. Stays neutral until the governor actually has
/// something to report — a permanent temperature badge trains people to ignore it.
class _ThermalLine extends StatelessWidget {
  const _ThermalLine({required this.stats});

  final DeviceStatsDto stats;

  @override
  Widget build(BuildContext context) {
    final (label, tone) = switch (stats.thermal) {
      'critical' => ('Device very hot — local models paused', VonColors.danger),
      'severe' => ('Cooling down — local models paused', VonColors.danger),
      'light' => ('Device warm — generating more slowly', VonColors.warn),
      _ => (
          '${(stats.availableRamMb / 1024).toStringAsFixed(1)}GB free · '
              '${stats.cpuCores} cores',
          VonColors.textSecondary,
        ),
    };
    return Text(label, style: VonText.caption.copyWith(color: tone));
  }
}

class _ProjectCard extends StatelessWidget {
  const _ProjectCard({
    required this.project,
    required this.onOpen,
    required this.onRename,
    required this.onDelete,
  });

  final ProjectDto project;
  final VoidCallback onOpen;
  final VoidCallback onRename;
  final VoidCallback onDelete;

  @override
  Widget build(BuildContext context) {
    final offline = project.provider == 'local';

    return NeuPressable(
      onTap: onOpen,
      radius: VonRadius.large,
      padding: const EdgeInsets.all(VonSpace.md),
      semanticLabel: 'Open VON ${project.ordinal}, ${project.name}',
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              // The VON number is the stable handle; the name is the label the
              // user gave it. Number first so the eye can index the grid.
              Expanded(
                child: Text(
                  'VON ${project.ordinal}',
                  style: VonText.caption.copyWith(
                    color: VonColors.accent,
                    letterSpacing: 0.8,
                  ),
                ),
              ),
              _CardMenu(onRename: onRename, onDelete: onDelete),
            ],
          ),
          const SizedBox(height: 3),
          Text(
            project.name,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
            style: VonText.heading,
          ),
          const SizedBox(height: VonSpace.xs),
          Text(
            _kindLabel(project.kind),
            style: VonText.caption.copyWith(color: VonColors.textDisabled),
          ),
          const Spacer(),
          // Model in the mono face (Design.md §6, Screen 2).
          Row(
            children: [
              Icon(
                offline ? Icons.smartphone_rounded : Icons.cloud_outlined,
                size: 12,
                color: offline ? VonColors.success : VonColors.accentText,
              ),
              const SizedBox(width: 5),
              Expanded(
                child: Text(
                  project.modelId.isEmpty ? 'No model set' : project.modelId,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: VonText.mono.copyWith(
                    color: offline ? VonColors.success : VonColors.accentText,
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: VonSpace.sm),
          Row(
            children: [
              Expanded(
                child: Text(
                  _relativeTime(project.updatedAtMs.toInt()),
                  style: VonText.caption,
                ),
              ),
              Text(
                '${project.fileCount} · ${_formatBytes(project.sizeBytes.toInt())}',
                style: VonText.caption,
              ),
            ],
          ),
        ],
      ),
    );
  }

  static String _kindLabel(String kind) => switch (kind) {
        'web' => 'Web',
        'data' => 'Data & simulation',
        'docs' => 'Documents',
        _ => 'General',
      };
}

class _CardMenu extends StatelessWidget {
  const _CardMenu({required this.onRename, required this.onDelete});

  final VoidCallback onRename;
  final VoidCallback onDelete;

  @override
  Widget build(BuildContext context) {
    return PopupMenuButton<String>(
      padding: EdgeInsets.zero,
      color: VonColors.bgElevated,
      icon: const Icon(
        Icons.more_vert_rounded,
        size: 18,
        color: VonColors.textSecondary,
      ),
      onSelected: (value) => value == 'rename' ? onRename() : onDelete(),
      itemBuilder: (context) => [
        PopupMenuItem(
          value: 'rename',
          child: Text('Rename', style: VonText.body),
        ),
        PopupMenuItem(
          value: 'delete',
          child: Text(
            'Delete',
            style: VonText.body.copyWith(color: VonColors.danger),
          ),
        ),
      ],
    );
  }
}

class _EmptyDashboard extends StatelessWidget {
  const _EmptyDashboard({required this.onCreate});

  final VoidCallback onCreate;

  @override
  Widget build(BuildContext context) {
    return ListView(
      // Keeps pull-to-refresh working on an otherwise unscrollable screen.
      physics: const AlwaysScrollableScrollPhysics(),
      children: [
        SizedBox(height: MediaQuery.sizeOf(context).height * 0.18),
        NeuEmptyState(
          icon: Icons.auto_awesome_mosaic_outlined,
          title: 'No VONs yet',
          message: 'A VON is an isolated sandbox with its own agent. It can '
              'only touch files inside itself — never your other VONs, and '
              'never the rest of your phone.',
          action: NeuButton(
            label: 'Create your first VON',
            icon: Icons.add_rounded,
            glow: true,
            onPressed: onCreate,
          ),
        ),
      ],
    );
  }
}

class _ProjectGridSkeleton extends StatelessWidget {
  const _ProjectGridSkeleton();

  @override
  Widget build(BuildContext context) {
    final columns = MediaQuery.sizeOf(context).width >= 720 ? 3 : 2;
    return GridView.builder(
      padding: const EdgeInsets.symmetric(
        horizontal: VonSpace.gutter,
        vertical: VonSpace.md,
      ),
      gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
        crossAxisCount: columns,
        crossAxisSpacing: VonSpace.md,
        mainAxisSpacing: VonSpace.md,
        childAspectRatio: 0.76,
      ),
      itemCount: 4,
      itemBuilder: (_, __) => const NeuShimmerCard(height: 180),
    );
  }
}

String _formatBytes(int bytes) {
  if (bytes < 1024) return '${bytes}B';
  if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(0)}KB';
  if (bytes < 1024 * 1024 * 1024) {
    return '${(bytes / (1024 * 1024)).toStringAsFixed(1)}MB';
  }
  return '${(bytes / (1024 * 1024 * 1024)).toStringAsFixed(2)}GB';
}

String _relativeTime(int epochMs) {
  final then = DateTime.fromMillisecondsSinceEpoch(epochMs);
  final delta = DateTime.now().difference(then);
  if (delta.inMinutes < 1) return 'Just now';
  if (delta.inMinutes < 60) return '${delta.inMinutes}m ago';
  if (delta.inHours < 24) return '${delta.inHours}h ago';
  if (delta.inDays < 7) return '${delta.inDays}d ago';
  return DateFormat.MMMd().format(then);
}
