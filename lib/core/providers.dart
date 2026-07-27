import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../models/chat.dart';
import '../services/core_api.dart';

/// Riverpod graph (SWE §4.1).
///
/// Rust owns the truth; these providers are a cache with invalidation. Anything
/// that mutates state on the Rust side invalidates the provider that reads it
/// back, rather than trying to keep a parallel Dart copy in sync.

final coreApiProvider = Provider<CoreApi>((ref) => CoreApi.instance);

/// Gates the whole app on the native library loading.
final coreInitProvider = FutureProvider<void>((ref) async {
  await CoreApi.ensureInitialised();
});

// ---- projects -------------------------------------------------------------

final projectsProvider =
    AsyncNotifierProvider<ProjectsNotifier, List<ProjectDto>>(
  ProjectsNotifier.new,
);

class ProjectsNotifier extends AsyncNotifier<List<ProjectDto>> {
  CoreApi get _api => ref.read(coreApiProvider);

  @override
  Future<List<ProjectDto>> build() async {
    await ref.watch(coreInitProvider.future);
    return _api.listProjects();
  }

  Future<ProjectDto> create({
    required String name,
    required String description,
    required String kind,
    required String provider,
    required String modelId,
  }) async {
    final project = await _api.createProject(
      name: name,
      description: description,
      kind: kind,
      provider: provider,
      modelId: modelId,
    );
    ref.invalidateSelf();
    return project;
  }

  Future<void> rename(String id, String name) async {
    await _api.renameProject(id, name);
    ref.invalidateSelf();
  }

  Future<void> setModel(String id, String provider, String modelId) async {
    await _api.setProjectModel(id, provider, modelId);
    ref.invalidateSelf();
    ref.invalidate(projectProvider(id));
  }

  Future<void> delete(String id) async {
    await _api.deleteProject(id);
    ref.invalidateSelf();
  }
}

final projectProvider =
    FutureProvider.family<ProjectDto, String>((ref, id) async {
  await ref.watch(coreInitProvider.future);
  return ref.read(coreApiProvider).getProject(id);
});

// ---- model hub ------------------------------------------------------------

final offlineModelsProvider =
    FutureProvider<List<OfflineModelDto>>((ref) async {
  await ref.watch(coreInitProvider.future);
  return ref.read(coreApiProvider).listOfflineModels();
});

final cloudModelsProvider = FutureProvider<List<CloudModelDto>>((ref) async {
  await ref.watch(coreInitProvider.future);
  return ref.read(coreApiProvider).listCloudModels();
});

// ---- github ---------------------------------------------------------------

/// Whether a GitHub token is stored. Offline, so it settles immediately and the
/// connect card never flashes through a wrong state.
final githubConnectedProvider = FutureProvider<bool>((ref) async {
  await ref.watch(coreInitProvider.future);
  return ref.read(coreApiProvider).githubConnected();
});

/// The account behind the stored token.
///
/// Separate from [githubConnectedProvider] because this one makes a request:
/// an error here means the token was revoked or expired, which the card should
/// say plainly rather than silently showing "not connected".
final githubAccountProvider = FutureProvider<GitHubAccountDto>((ref) async {
  await ref.watch(coreInitProvider.future);
  return ref.read(coreApiProvider).githubAccount();
});

/// Every model one provider offers, fetched live from its list endpoint.
///
/// A family rather than one big list: fetching all five providers to show one
/// picker would be four wasted requests, and a provider whose key is wrong
/// should fail on its own rather than emptying everyone else's list.
final providerModelsProvider =
    FutureProvider.family<List<CloudModelDto>, String>((ref, provider) async {
  await ref.watch(coreInitProvider.future);
  return ref.read(coreApiProvider).listProviderModels(provider);
});

/// Masked key hint per provider, for the Model Hub's key fields.
final apiKeyHintProvider =
    FutureProvider.family<String?, String>((ref, provider) async {
  await ref.watch(coreInitProvider.future);
  return ref.read(coreApiProvider).apiKeyHint(provider);
});

/// Model ids the user has actually asked to download.
///
/// [downloadProvider] must never be watched for a model that isn't in here:
/// watching a `StreamProvider` starts it, so a card that watched
/// unconditionally would begin downloading multi-gigabyte weights the moment
/// the Model Hub rendered.
final requestedDownloadsProvider =
    NotifierProvider<RequestedDownloads, Set<String>>(RequestedDownloads.new);

class RequestedDownloads extends Notifier<Set<String>> {
  @override
  Set<String> build() => const {};

  void request(String modelId) => state = {...state, modelId};

  void forget(String modelId) =>
      state = {...state}..removeWhere((id) => id == modelId);
}

/// Live download progress, keyed by model id. Kept alive while a download runs
/// so leaving and returning to the Model Hub doesn't cancel it.
final downloadProvider = StreamProvider.family
    .autoDispose<DownloadProgressDto, String>((ref, modelId) {
  final link = ref.keepAlive();
  final controller = StreamController<DownloadProgressDto>();

  final subscription =
      ref.read(coreApiProvider).downloadModel(modelId).listen(
    controller.add,
    onError: controller.addError,
    onDone: () {
      controller.close();
      // The model list now shows it as downloaded.
      ref.invalidate(offlineModelsProvider);
      link.close();
    },
  );

  ref.onDispose(() {
    subscription.cancel();
    controller.close();
  });
  return controller.stream;
});

// ---- device ---------------------------------------------------------------

final deviceStatsProvider = StreamProvider<DeviceStatsDto>((ref) async* {
  await ref.watch(coreInitProvider.future);
  yield ref.read(coreApiProvider).deviceStats;
  yield* ref.read(coreApiProvider).watchDeviceStats();
});

final deviceProfileProvider =
    AsyncNotifierProvider<DeviceProfileNotifier, DeviceProfileDto?>(
  DeviceProfileNotifier.new,
);

class DeviceProfileNotifier extends AsyncNotifier<DeviceProfileDto?> {
  @override
  Future<DeviceProfileDto?> build() async {
    await ref.watch(coreInitProvider.future);
    return ref.read(coreApiProvider).cachedDeviceProfile;
  }

  /// Runs the 5-second benchmark. Blocks the Rust worker for its duration by
  /// design — a benchmark competing with other work measures nothing useful.
  Future<DeviceProfileDto> measure({int durationMs = 5000}) async {
    state = const AsyncLoading();
    try {
      final profile =
          await ref.read(coreApiProvider).runBenchmark(durationMs: durationMs);
      state = AsyncData(profile);
      // Compatibility badges depend on the profile.
      ref.invalidate(offlineModelsProvider);
      return profile;
    } catch (e, st) {
      state = AsyncError(e, st);
      rethrow;
    }
  }
}

// ---- workspace ------------------------------------------------------------

final fileTreeProvider =
    FutureProvider.family<List<FileEntryDto>, String>((ref, projectId) async {
  await ref.watch(coreInitProvider.future);
  return ref.read(coreApiProvider).fileTree(projectId);
});

final sandboxUsageProvider =
    FutureProvider.family<SandboxUsageDto, String>((ref, projectId) async {
  await ref.watch(coreInitProvider.future);
  return ref.read(coreApiProvider).sandboxUsage(projectId);
});

final fileContentProvider = FutureProvider.family
    .autoDispose<String, ({String projectId, String path})>((ref, args) async {
  return ref.read(coreApiProvider).readFile(args.projectId, args.path);
});

/// The preview server's URL. Null until the agent (or the user) starts it.
final previewUrlProvider =
    StateNotifierProvider.family<PreviewNotifier, AsyncValue<String?>, String>(
  (ref, projectId) => PreviewNotifier(ref, projectId),
);

class PreviewNotifier extends StateNotifier<AsyncValue<String?>> {
  PreviewNotifier(this._ref, this._projectId)
      : super(AsyncData(_ref.read(coreApiProvider).previewUrl(_projectId)));

  final Ref _ref;
  final String _projectId;

  Future<void> start() async {
    state = const AsyncLoading();
    try {
      state = AsyncData(await _ref.read(coreApiProvider).startPreview(_projectId));
    } catch (e, st) {
      state = AsyncError(e, st);
    }
  }

  /// Called when the agent reports it started the server itself.
  void adopt(String url) => state = AsyncData(url);

  Future<void> stop() async {
    await _ref.read(coreApiProvider).stopPreview(_projectId);
    state = const AsyncData(null);
  }
}

// ---- chat -----------------------------------------------------------------

final chatProvider =
    StateNotifierProvider.family<ChatNotifier, ChatState, String>(
  (ref, projectId) => ChatNotifier(ref, projectId)..load(),
);
