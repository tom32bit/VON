import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../services/core_api.dart';
import '../widgets/code_block.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';

/// PRD Page 5, Tab 2: the Sandbox Explorer.
///
/// The tree arrives from Rust pre-flattened with a depth per row, so rendering
/// is a single `ListView.builder` pass and stays smooth on a project with
/// hundreds of files.
class FilesTab extends ConsumerWidget {
  const FilesTab({super.key, required this.projectId});

  final String projectId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final tree = ref.watch(fileTreeProvider(projectId));
    final usage = ref.watch(sandboxUsageProvider(projectId));

    return Column(
      children: [
        if (usage.valueOrNull case final quota?) _QuotaBar(usage: quota),
        Expanded(
          child: switch (tree) {
            AsyncLoading() => const Center(child: NeuPulsingDots()),
            AsyncError(:final error) => Padding(
                padding: const EdgeInsets.all(VonSpace.md),
                child: NeuErrorBanner(
                  message: '$error',
                  onRetry: () => ref.invalidate(fileTreeProvider(projectId)),
                ),
              ),
            AsyncData(value: final entries) when entries.isEmpty =>
              const NeuEmptyState(
                icon: Icons.folder_open_outlined,
                title: 'No files yet',
                message: 'Files the agent creates appear here. Nothing outside '
                    'this VON is reachable from it.',
              ),
            AsyncData(value: final entries) => RefreshIndicator(
                color: VonColors.accent,
                backgroundColor: VonColors.bgSurface,
                onRefresh: () async {
                  ref.invalidate(fileTreeProvider(projectId));
                  ref.invalidate(sandboxUsageProvider(projectId));
                },
                child: ListView.builder(
                  padding: const EdgeInsets.fromLTRB(
                    VonSpace.md,
                    VonSpace.sm,
                    VonSpace.md,
                    VonSpace.xl,
                  ),
                  itemCount: entries.length,
                  itemBuilder: (context, i) => _FileRow(
                    entry: entries[i],
                    onOpen: entries[i].isDir
                        ? null
                        : () => _openFile(context, ref, entries[i]),
                  ),
                ),
              ),
            _ => const SizedBox.shrink(),
          },
        ),
      ],
    );
  }

  void _openFile(BuildContext context, WidgetRef ref, FileEntryDto entry) {
    Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => _FileViewer(projectId: projectId, entry: entry),
      ),
    );
  }
}

class _QuotaBar extends StatelessWidget {
  const _QuotaBar({required this.usage});

  final SandboxUsageDto usage;

  @override
  Widget build(BuildContext context) {
    final used = usage.totalBytes.toInt();
    final max = usage.maxBytes.toInt();
    final fraction = max > 0 ? (used / max).clamp(0.0, 1.0) : 0.0;
    // Only colour the bar once it actually matters.
    final tone = fraction > 0.9
        ? VonColors.danger
        : fraction > 0.7
            ? VonColors.warn
            : VonColors.accent;

    return Padding(
      padding: const EdgeInsets.fromLTRB(VonSpace.md, 0, VonSpace.md, VonSpace.sm),
      child: NeuSurface(
        depth: NeuDepth.concave,
        radius: VonRadius.small,
        padding: const EdgeInsets.all(VonSpace.sm),
        child: Column(
          children: [
            Row(
              children: [
                Text(
                  '${usage.fileCount} file${usage.fileCount == 1 ? '' : 's'}',
                  style: VonText.caption,
                ),
                const Spacer(),
                Text(
                  '${_mb(used)} / ${_mb(max)}',
                  style: VonText.mono.copyWith(fontSize: 10),
                ),
              ],
            ),
            const SizedBox(height: 6),
            ClipRRect(
              borderRadius: BorderRadius.circular(2),
              child: LinearProgressIndicator(
                value: fraction,
                minHeight: 3,
                backgroundColor: VonColors.shadowDark,
                valueColor: AlwaysStoppedAnimation(tone),
              ),
            ),
          ],
        ),
      ),
    );
  }

  static String _mb(int bytes) =>
      '${(bytes / (1024 * 1024)).toStringAsFixed(1)}MB';
}

/// Folders are small extruded containers; files are flat rows (Design.md §6,
/// Screen 4).
class _FileRow extends StatelessWidget {
  const _FileRow({required this.entry, this.onOpen});

  final FileEntryDto entry;
  final VoidCallback? onOpen;

  @override
  Widget build(BuildContext context) {
    final indent = entry.depth * 16.0;

    if (entry.isDir) {
      return Padding(
        padding: EdgeInsets.only(left: indent, bottom: 6),
        child: NeuSurface(
          radius: VonRadius.small,
          intensity: 0.55,
          padding: const EdgeInsets.symmetric(
            horizontal: VonSpace.sm,
            vertical: 7,
          ),
          child: Row(
            children: [
              const Icon(
                Icons.folder_rounded,
                size: 14,
                color: VonColors.textSecondary,
              ),
              const SizedBox(width: VonSpace.sm),
              Text(
                entry.name,
                style: VonText.body.copyWith(
                  fontSize: 13,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ],
          ),
        ),
      );
    }

    return Padding(
      padding: EdgeInsets.only(left: indent, bottom: 2),
      child: InkWell(
        onTap: onOpen,
        borderRadius: BorderRadius.circular(VonRadius.small),
        child: Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: VonSpace.sm,
            vertical: 7,
          ),
          child: Row(
            children: [
              Icon(_iconFor(entry.name), size: 13, color: VonColors.textDisabled),
              const SizedBox(width: VonSpace.sm),
              Expanded(
                child: Text(
                  entry.name,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: VonText.code.copyWith(fontSize: 12.5),
                ),
              ),
              Text(
                _size(entry.sizeBytes.toInt()),
                style: VonText.caption.copyWith(fontSize: 10),
              ),
            ],
          ),
        ),
      ),
    );
  }

  static IconData _iconFor(String name) {
    final extension = name.contains('.') ? name.split('.').last : '';
    return switch (extension) {
      'py' => Icons.code_rounded,
      'rs' || 'c' || 'cpp' || 'go' || 'java' => Icons.code_rounded,
      'html' || 'htm' => Icons.language_rounded,
      'css' => Icons.palette_outlined,
      'js' || 'ts' => Icons.javascript_rounded,
      'json' || 'yaml' || 'yml' || 'toml' => Icons.data_object_rounded,
      'md' || 'txt' => Icons.description_outlined,
      'csv' => Icons.table_chart_outlined,
      'wasm' => Icons.memory_rounded,
      _ => Icons.insert_drive_file_outlined,
    };
  }

  static String _size(int bytes) {
    if (bytes < 1024) return '${bytes}B';
    if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)}K';
    return '${(bytes / (1024 * 1024)).toStringAsFixed(1)}M';
  }
}

/// Read-only viewer with syntax highlighting. Editing goes through the same
/// sandbox validation the agent's writes do.
class _FileViewer extends ConsumerWidget {
  const _FileViewer({required this.projectId, required this.entry});

  final String projectId;
  final FileEntryDto entry;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final content = ref.watch(
      fileContentProvider((projectId: projectId, path: entry.path)),
    );

    return Scaffold(
      appBar: AppBar(
        title: Text(
          entry.name,
          style: VonText.title.copyWith(fontSize: 17),
        ),
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
      body: switch (content) {
        AsyncLoading() => const Center(child: NeuPulsingDots()),
        AsyncError(:final error) => Padding(
            padding: const EdgeInsets.all(VonSpace.md),
            child: NeuErrorBanner(
              message: error.toString().contains('not valid UTF-8')
                  ? 'This is a binary file, so it cannot be shown as text. '
                      'Export the VON to open it elsewhere.'
                  : '$error',
            ),
          ),
        AsyncData(value: final text) => SingleChildScrollView(
            padding: const EdgeInsets.all(VonSpace.md),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(entry.path, style: VonText.mono),
                const SizedBox(height: VonSpace.sm),
                CodeBlock(code: text, language: entry.name, filename: entry.path),
              ],
            ),
          ),
        _ => const SizedBox.shrink(),
      },
    );
  }
}
