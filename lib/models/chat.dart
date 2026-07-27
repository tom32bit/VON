import 'dart:async';
import 'dart:convert';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../core/errors.dart';
import '../core/providers.dart';
import '../services/agent_service_channel.dart';
import '../services/core_api.dart';

enum ChatRole { user, assistant, system }

/// One tool invocation, shown as a row in the agent's thought process.
/// `ok == null` means it is still running.
class ToolStep {
  const ToolStep({
    required this.callId,
    required this.name,
    required this.summary,
    this.ok,
    this.output = '',
  });

  final String callId;
  final String name;
  final String summary;
  final bool? ok;
  final String output;

  bool get running => ok == null;

  ToolStep copyWith({bool? ok, String? output, String? summary}) => ToolStep(
        callId: callId,
        name: name,
        summary: summary ?? this.summary,
        ok: ok ?? this.ok,
        output: output ?? this.output,
      );
}

class ChatTurn {
  const ChatTurn({
    required this.id,
    required this.role,
    this.text = '',
    this.steps = const [],
    this.thought = '',
    this.streaming = false,
  });

  final String id;
  final ChatRole role;
  final String text;
  final List<ToolStep> steps;

  /// The model's reasoning summary, when the provider streams one.
  final String thought;
  final bool streaming;

  bool get isEmpty => text.trim().isEmpty && steps.isEmpty && thought.isEmpty;

  ChatTurn copyWith({
    String? text,
    List<ToolStep>? steps,
    String? thought,
    bool? streaming,
  }) =>
      ChatTurn(
        id: id,
        role: role,
        text: text ?? this.text,
        steps: steps ?? this.steps,
        thought: thought ?? this.thought,
        streaming: streaming ?? this.streaming,
      );
}

class ChatState {
  const ChatState({
    this.turns = const [],
    this.loading = true,
    this.running = false,
    this.phase = '',
    this.phaseDetail = '',
    this.error,
    this.errorTransient = false,
    this.throttleMessage,
    this.inputTokens = 0,
    this.outputTokens = 0,
    this.hasDeliverables = false,
  });

  final List<ChatTurn> turns;
  final bool loading;

  /// A run is in flight — drives the shimmer bar and the Stop button.
  final bool running;

  /// `planning` | `calling_tool` | `executing` | `observing` | `retrying`
  final String phase;
  final String phaseDetail;

  final String? error;

  /// Whether the failure is worth retrying — controls whether the error banner
  /// offers a Retry action at all.
  final bool errorTransient;

  /// Set while the governor is throttling, so the UI can explain the slowdown
  /// instead of just feeling broken.
  final String? throttleMessage;

  final int inputTokens;
  final int outputTokens;

  /// The agent has produced files at least once — arms "Download Project"
  /// (Design.md §6, Screen 5).
  final bool hasDeliverables;

  ChatState copyWith({
    List<ChatTurn>? turns,
    bool? loading,
    bool? running,
    String? phase,
    String? phaseDetail,
    Object? error = _sentinel,
    bool? errorTransient,
    Object? throttleMessage = _sentinel,
    int? inputTokens,
    int? outputTokens,
    bool? hasDeliverables,
  }) =>
      ChatState(
        turns: turns ?? this.turns,
        loading: loading ?? this.loading,
        running: running ?? this.running,
        phase: phase ?? this.phase,
        phaseDetail: phaseDetail ?? this.phaseDetail,
        error: error == _sentinel ? this.error : error as String?,
        errorTransient: errorTransient ?? this.errorTransient,
        throttleMessage: throttleMessage == _sentinel
            ? this.throttleMessage
            : throttleMessage as String?,
        inputTokens: inputTokens ?? this.inputTokens,
        outputTokens: outputTokens ?? this.outputTokens,
        hasDeliverables: hasDeliverables ?? this.hasDeliverables,
      );

  static const _sentinel = Object();
}

/// Consumes the Rust agent event stream and shapes it into chat UI state
/// (SWE §4.1, `chatProvider`).
class ChatNotifier extends StateNotifier<ChatState> {
  ChatNotifier(this._ref, this.projectId) : super(const ChatState());

  final Ref _ref;
  final String projectId;

  StreamSubscription<AgentEventDto>? _subscription;

  /// Assistant prose the core has actually written to SQLite for the run in
  /// flight, joined across steps. Held here rather than derived from the turn
  /// because the turn's text also holds unreconciled streamed deltas.
  String _persisted = '';

  /// Whether this notifier is holding the foreground service up.
  ///
  /// Tracked per-notifier rather than trusting call order: the channel counts
  /// holders so two projects can run at once, and a stray extra release would
  /// drop the service out from under the other one.
  bool _holdsService = false;

  CoreApi get _api => _ref.read(coreApiProvider);

  /// Restores the conversation from SQLite (Process C: Resumption).
  Future<void> load() async {
    try {
      final rows = await _api.listMessages(projectId);
      state = state.copyWith(
        turns: _rebuild(rows),
        loading: false,
        hasDeliverables: rows.any((r) => r.role == 'tool'),
      );
    } catch (e) {
      state = state.copyWith(loading: false, error: describeError(e));
    }
  }

  /// Rebuilds turns from persisted rows.
  ///
  /// Tool results are stored as their own row (they are a distinct turn to the
  /// model) but belong to the preceding assistant turn visually, so they get
  /// merged back onto it here.
  List<ChatTurn> _rebuild(List<MessageDto> rows) {
    final turns = <ChatTurn>[];

    for (final row in rows) {
      switch (row.role) {
        case 'user':
          turns.add(ChatTurn(
            id: 'm${row.seq}',
            role: ChatRole.user,
            text: row.content,
          ));
        case 'assistant':
          turns.add(ChatTurn(
            id: 'm${row.seq}',
            role: ChatRole.assistant,
            text: row.content,
            steps: _decodeCalls(row.stepsJson),
          ));
        case 'tool':
          final results = _decodeResults(row.stepsJson);
          if (turns.isEmpty || results.isEmpty) continue;
          final target = turns.last;
          if (target.role != ChatRole.assistant) continue;
          turns[turns.length - 1] = target.copyWith(
            steps: [
              for (final step in target.steps)
                results.containsKey(step.callId)
                    ? step.copyWith(
                        ok: !(results[step.callId]!.isError),
                        output: results[step.callId]!.content,
                      )
                    : step,
            ],
          );
        default:
          break;
      }
    }

    return turns.where((t) => !t.isEmpty).toList();
  }

  List<ToolStep> _decodeCalls(String json) {
    if (json.isEmpty || json == '[]') return const [];
    try {
      final list = jsonDecode(json) as List<dynamic>;
      return [
        for (final item in list.cast<Map<String, dynamic>>())
          ToolStep(
            callId: item['id'] as String? ?? '',
            name: item['name'] as String? ?? '',
            summary: _summarise(
              item['name'] as String? ?? '',
              item['arguments'],
            ),
            // Restored from history: whatever happened has already happened.
            ok: true,
          ),
      ];
    } catch (_) {
      // A transcript that can't be parsed shouldn't take the screen down with
      // it — show the turn without its steps.
      return const [];
    }
  }

  Map<String, ({String content, bool isError})> _decodeResults(String json) {
    if (json.isEmpty || json == '[]') return const {};
    try {
      final list = jsonDecode(json) as List<dynamic>;
      return {
        for (final item in list.cast<Map<String, dynamic>>())
          (item['call_id'] as String? ?? ''): (
            content: item['content'] as String? ?? '',
            isError: item['is_error'] as bool? ?? false,
          ),
      };
    } catch (_) {
      return const {};
    }
  }

  static String _summarise(String tool, Object? args) {
    final map = args is Map<String, dynamic> ? args : null;
    final path = map?['path'] as String?;
    return switch (tool) {
      'write_file' => 'Wrote ${path ?? 'a file'}',
      'read_file' => 'Read ${path ?? 'a file'}',
      'delete_file' => 'Deleted ${path ?? 'a file'}',
      'list_files' => 'Listed the files in this VON',
      'run_python' => path == null ? 'Ran Python' : 'Ran $path',
      'run_wasm' => 'Ran ${path ?? 'a module'}',
      'start_preview' => 'Started live preview',
      'git_push' => 'Synced with ${map?['repo'] as String? ?? 'GitHub'}',
      'git_pull' => 'Pulled ${map?['repo'] as String? ?? 'from GitHub'}',
      'task_complete' => 'Finished',
      _ => tool,
    };
  }

  /// Sends a prompt and streams the run.
  Future<void> send(String message) async {
    if (state.running) return;
    final text = message.trim();
    if (text.isEmpty) return;

    final now = DateTime.now().microsecondsSinceEpoch;
    state = state.copyWith(
      turns: [
        ...state.turns,
        ChatTurn(id: 'u$now', role: ChatRole.user, text: text),
        ChatTurn(id: 'a$now', role: ChatRole.assistant, streaming: true),
      ],
      running: true,
      error: null,
      throttleMessage: null,
      phase: 'planning',
      phaseDetail: 'Reading the request',
    );

    _persisted = '';
    _acquireService();
    _subscription = _api
        .runAgent(projectId: projectId, message: text)
        .listen(_onEvent, onError: _onStreamError, onDone: _finish);
  }

  void _onEvent(AgentEventDto event) {
    switch (event.kind) {
      case 'phase':
        state = state.copyWith(phase: event.text, phaseDetail: event.detail);

      case 'token':
        _mutateActive((turn) => turn.copyWith(text: turn.text + event.text));

      case 'thought':
        _mutateActive((turn) => turn.copyWith(thought: turn.thought + event.text));

      case 'tool_started':
        _mutateActive((turn) {
          final steps = [...turn.steps];
          final at = steps.indexWhere((s) => s.callId == event.callId);
          final step = ToolStep(
            callId: event.callId,
            name: event.toolName,
            summary: event.detail,
          );
          // The engine announces a call when it starts streaming arguments and
          // again when it actually runs; the second announcement carries the
          // real summary, so update in place rather than duplicating the row.
          if (at >= 0) {
            steps[at] = steps[at].copyWith(summary: event.detail);
          } else {
            steps.add(step);
          }
          return turn.copyWith(steps: steps);
        });
        state = state.copyWith(phase: 'calling_tool', phaseDetail: event.detail);

      case 'tool_finished':
        _mutateActive((turn) => turn.copyWith(
              steps: [
                for (final step in turn.steps)
                  step.callId == event.callId
                      ? step.copyWith(ok: event.ok, output: event.text)
                      : step,
              ],
            ));

      case 'files_changed':
        _ref.invalidate(fileTreeProvider(projectId));
        _ref.invalidate(sandboxUsageProvider(projectId));
        state = state.copyWith(hasDeliverables: true);

      case 'preview_ready':
        _ref.read(previewUrlProvider(projectId).notifier).adopt(event.text);

      case 'throttled':
        state = state.copyWith(throttleMessage: event.detail);

      case 'message_saved':
        // Rust owns sequence numbers, and this is the signal that the turn is
        // durable. It is also the authoritative text: the streamed deltas are
        // raw model output, which for a local model carries the tool-call block
        // and any leaked turn markers that Rust strips before persisting.
        //
        // One agent turn produces one of these per step, each holding only that
        // step's prose, so they accumulate — replacing outright would drop
        // everything the agent said before it reached for a tool. Rewriting
        // even when the step saved nothing is the point: that is exactly the
        // tool-only step whose raw JSON is sitting in the bubble.
        if (event.role == 'assistant') {
          if (event.text.trim().isNotEmpty) {
            _persisted = _persisted.isEmpty
                ? event.text
                : '$_persisted\n\n${event.text}';
          }
          _mutateActive((turn) => turn.copyWith(text: _persisted));
        }
        break;

      case 'done':
        state = state.copyWith(
          inputTokens: state.inputTokens + event.inputTokens.toInt(),
          outputTokens: state.outputTokens + event.outputTokens.toInt(),
          phase: '',
          phaseDetail: '',
          throttleMessage: null,
        );
        if (event.text == 'step_limit' || event.text == 'max_tokens') {
          _mutateActive((turn) => turn.copyWith(
                text: turn.text.isEmpty
                    ? event.detail
                    : '${turn.text}\n\n${event.detail}',
              ));
        }

      case 'failed':
        state = state.copyWith(
          error: event.text,
          errorTransient: event.errorTransient,
        );

      default:
        break;
    }
  }

  void _mutateActive(ChatTurn Function(ChatTurn) update) {
    final turns = [...state.turns];
    final at = turns.lastIndexWhere((t) => t.streaming);
    if (at < 0) return;
    turns[at] = update(turns[at]);
    state = state.copyWith(turns: turns);
  }

  void _onStreamError(Object error) {
    state = state.copyWith(error: describeError(error), errorTransient: true);
  }

  void _finish() {
    _releaseService();
    final turns = [...state.turns];
    final at = turns.lastIndexWhere((t) => t.streaming);
    if (at >= 0) {
      turns[at] = turns[at].copyWith(streaming: false);
      // A turn that produced nothing visible would render as an empty bubble.
      if (turns[at].isEmpty) turns.removeAt(at);
    }
    state = state.copyWith(
      turns: turns,
      running: false,
      phase: '',
      phaseDetail: '',
    );
    _subscription = null;
    _ref.invalidate(projectProvider(projectId));
  }

  /// Stops the run. The Rust loop finishes the current tool call first, so the
  /// transcript stays consistent.
  Future<void> cancel() async {
    if (!state.running) return;
    await _api.cancelAgent(projectId);
  }

  Future<void> clear() async {
    await _api.clearConversation(projectId);
    state = const ChatState(loading: false);
  }

  void dismissError() => state = state.copyWith(error: null);

  @override
  void dispose() {
    // Leaving the workspace mid-run does not stop the run, but this notifier
    // will never see its `done`, so the hold has to be given up here or the
    // service outlives the work.
    _releaseService();
    _subscription?.cancel();
    super.dispose();
  }

  /// Raises the process priority for as long as the run lasts, so switching
  /// apps does not hand VON to the low-memory killer mid-build.
  void _acquireService() {
    if (_holdsService) return;
    _holdsService = true;
    AgentServiceChannel.instance
        .acquire(projectLabel: 'Building your project. Tap to come back.')
        .ignore();
  }

  void _releaseService() {
    if (!_holdsService) return;
    _holdsService = false;
    AgentServiceChannel.instance.release().ignore();
  }
}
