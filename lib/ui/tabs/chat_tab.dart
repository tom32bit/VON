import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/providers.dart';
import '../../core/theme.dart';
import '../../models/chat.dart';
import '../widgets/code_block.dart';
import '../widgets/feedback.dart';
import '../widgets/neumorphic.dart';

/// PRD Page 5, Tab 1: the Chat / Command Center.
class ChatTab extends ConsumerStatefulWidget {
  const ChatTab({super.key, required this.projectId});

  final String projectId;

  @override
  ConsumerState<ChatTab> createState() => _ChatTabState();
}

class _ChatTabState extends ConsumerState<ChatTab> {
  final _input = TextEditingController();
  final _scroll = ScrollController();

  @override
  void dispose() {
    _input.dispose();
    _scroll.dispose();
    super.dispose();
  }

  /// Follows the stream only while the user is already near the bottom —
  /// yanking the viewport away from someone reading back through the log is
  /// the classic streaming-chat annoyance.
  void _followTail() {
    if (!_scroll.hasClients) return;
    final distanceFromBottom =
        _scroll.position.maxScrollExtent - _scroll.position.pixels;
    if (distanceFromBottom > 220) return;

    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!_scroll.hasClients) return;
      _scroll.animateTo(
        _scroll.position.maxScrollExtent,
        duration: const Duration(milliseconds: 180),
        curve: Curves.easeOut,
      );
    });
  }

  void _send() {
    final text = _input.text;
    if (text.trim().isEmpty) return;
    _input.clear();
    ref.read(chatProvider(widget.projectId).notifier).send(text);
    _followTail();
  }

  @override
  Widget build(BuildContext context) {
    final chat = ref.watch(chatProvider(widget.projectId));

    ref.listen(chatProvider(widget.projectId), (previous, next) {
      // Follow on a new turn, and on growth of the streaming turn's text.
      // `lastOrNull` lives in package:collection, which this app doesn't
      // depend on, so index explicitly.
      int tailLength(ChatState? state) {
        final turns = state?.turns ?? const <ChatTurn>[];
        return turns.isEmpty ? -1 : turns.last.text.length;
      }

      if (previous?.turns.length != next.turns.length ||
          tailLength(previous) != tailLength(next)) {
        _followTail();
      }
    });

    return Column(
      children: [
        Expanded(
          child: switch (chat) {
            ChatState(loading: true) => const Center(child: NeuPulsingDots()),
            ChatState(turns: final turns) when turns.isEmpty =>
              const _ChatEmptyState(),
            _ => ListView.builder(
                controller: _scroll,
                padding: const EdgeInsets.fromLTRB(
                  VonSpace.md,
                  VonSpace.sm,
                  VonSpace.md,
                  VonSpace.md,
                ),
                // Rebuilding only visible rows keeps scrolling smooth when the
                // agent has generated thousands of lines (SWE §5.2).
                itemCount: chat.turns.length,
                itemBuilder: (context, i) => _TurnBubble(turn: chat.turns[i]),
              ),
          },
        ),
        if (chat.throttleMessage != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(
              VonSpace.md,
              0,
              VonSpace.md,
              VonSpace.sm,
            ),
            child: _ThrottleNotice(message: chat.throttleMessage!),
          ),
        if (chat.error != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(
              VonSpace.md,
              0,
              VonSpace.md,
              VonSpace.sm,
            ),
            child: NeuErrorBanner(
              message: chat.error!,
              onRetry: chat.errorTransient ? _send : null,
              onDismiss: () =>
                  ref.read(chatProvider(widget.projectId).notifier).dismissError(),
            ),
          ),
        _Composer(
          controller: _input,
          running: chat.running,
          phase: chat.phase,
          phaseDetail: chat.phaseDetail,
          onSend: _send,
          onStop: () => ref.read(chatProvider(widget.projectId).notifier).cancel(),
        ),
      ],
    );
  }
}

class _ChatEmptyState extends StatelessWidget {
  const _ChatEmptyState();

  @override
  Widget build(BuildContext context) {
    return const NeuEmptyState(
      icon: Icons.bolt_rounded,
      title: 'Tell the agent what to build',
      message: 'It can write files, run them, read the output, and fix what '
          'breaks — all inside this VON, and nowhere else.',
    );
  }
}

/// User prompts are extruded and right-aligned; agent replies are intruded and
/// left-aligned, as if spoken from inside the device (Design.md §5.5).
class _TurnBubble extends StatelessWidget {
  const _TurnBubble({required this.turn});

  final ChatTurn turn;

  @override
  Widget build(BuildContext context) {
    final isUser = turn.role == ChatRole.user;
    final maxWidth = MediaQuery.sizeOf(context).width * 0.88;

    return Align(
      alignment: isUser ? Alignment.centerRight : Alignment.centerLeft,
      child: Container(
        constraints: BoxConstraints(maxWidth: maxWidth),
        margin: const EdgeInsets.only(bottom: VonSpace.md),
        child: NeuPulse(
          active: turn.streaming && turn.text.isEmpty && turn.steps.isEmpty,
          child: NeuSurface(
            depth: isUser ? NeuDepth.extruded : NeuDepth.concave,
            radius: VonRadius.medium,
            padding: const EdgeInsets.all(VonSpace.md),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (turn.thought.isNotEmpty) _ThoughtPanel(text: turn.thought),
                if (turn.steps.isNotEmpty) ...[
                  for (final step in turn.steps) _ToolRow(step: step),
                  if (turn.text.isNotEmpty) const SizedBox(height: VonSpace.sm),
                ],
                if (turn.text.isNotEmpty)
                  MarkdownishText(
                    text: turn.text,
                    style: isUser ? VonText.body : VonText.body,
                  ),
                if (turn.streaming && turn.text.isNotEmpty)
                  const Align(
                    alignment: Alignment.centerLeft,
                    child: NeuTypingCursor(),
                  ),
                if (turn.streaming && turn.text.isEmpty && turn.steps.isEmpty)
                  const NeuPulsingDots(),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// The model's reasoning summary, when the provider streams one. Collapsed by
/// default — it is context, not the answer.
class _ThoughtPanel extends StatefulWidget {
  const _ThoughtPanel({required this.text});

  final String text;

  @override
  State<_ThoughtPanel> createState() => _ThoughtPanelState();
}

class _ThoughtPanelState extends State<_ThoughtPanel> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(bottom: VonSpace.sm),
      child: GestureDetector(
        onTap: () => setState(() => _open = !_open),
        behavior: HitTestBehavior.opaque,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  _open ? Icons.expand_less_rounded : Icons.psychology_outlined,
                  size: 13,
                  color: VonColors.textDisabled,
                ),
                const SizedBox(width: 5),
                Text(
                  _open ? 'Hide reasoning' : 'Reasoning',
                  style: VonText.caption,
                ),
              ],
            ),
            AnimatedCrossFade(
              duration: VonMotion.press,
              crossFadeState: _open
                  ? CrossFadeState.showSecond
                  : CrossFadeState.showFirst,
              firstChild: const SizedBox(width: double.infinity),
              secondChild: Padding(
                padding: const EdgeInsets.only(top: VonSpace.sm),
                child: Text(widget.text, style: VonText.thought),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// One tool call: Planning → Calling Tool → Executing → Observing, made visible.
class _ToolRow extends StatefulWidget {
  const _ToolRow({required this.step});

  final ToolStep step;

  @override
  State<_ToolRow> createState() => _ToolRowState();
}

class _ToolRowState extends State<_ToolRow> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final step = widget.step;
    final (icon, tone) = switch (step) {
      ToolStep(running: true) => (Icons.more_horiz_rounded, VonColors.accent),
      ToolStep(ok: false) => (Icons.error_outline_rounded, VonColors.danger),
      _ => (Icons.check_rounded, VonColors.success),
    };

    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: GestureDetector(
        onTap: step.output.isEmpty ? null : () => setState(() => _open = !_open),
        behavior: HitTestBehavior.opaque,
        child: NeuSurface(
          depth: NeuDepth.flat,
          radius: VonRadius.small,
          color: VonColors.bgSurface.withOpacity(0.55),
          padding: const EdgeInsets.symmetric(
            horizontal: VonSpace.sm,
            vertical: 7,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  if (step.running)
                    const NeuPulsingDots(size: 4)
                  else
                    Icon(icon, size: 13, color: tone),
                  const SizedBox(width: VonSpace.sm),
                  Expanded(
                    child: Text(
                      step.summary.isEmpty ? step.name : step.summary,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: VonText.mono.copyWith(
                        color: step.ok == false
                            ? VonColors.danger
                            : VonColors.textSecondary,
                      ),
                    ),
                  ),
                  if (step.output.isNotEmpty)
                    Icon(
                      _open ? Icons.expand_less_rounded : Icons.expand_more_rounded,
                      size: 14,
                      color: VonColors.textDisabled,
                    ),
                ],
              ),
              if (_open && step.output.isNotEmpty) ...[
                const SizedBox(height: VonSpace.sm),
                TerminalBlock(text: step.output),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

class _ThrottleNotice extends StatelessWidget {
  const _ThrottleNotice({required this.message});

  final String message;

  @override
  Widget build(BuildContext context) {
    return NeuSurface(
      depth: NeuDepth.concave,
      radius: VonRadius.small,
      padding: const EdgeInsets.all(VonSpace.sm),
      child: Row(
        children: [
          const Icon(
            Icons.thermostat_rounded,
            size: 14,
            color: VonColors.warn,
          ),
          const SizedBox(width: VonSpace.sm),
          Expanded(
            child: Text(
              message,
              style: VonText.caption.copyWith(color: VonColors.warn),
            ),
          ),
        ],
      ),
    );
  }
}

class _Composer extends StatelessWidget {
  const _Composer({
    required this.controller,
    required this.running,
    required this.phase,
    required this.phaseDetail,
    required this.onSend,
    required this.onStop,
  });

  final TextEditingController controller;
  final bool running;
  final String phase;
  final String phaseDetail;
  final VoidCallback onSend;
  final VoidCallback onStop;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: EdgeInsets.fromLTRB(
        VonSpace.md,
        0,
        VonSpace.md,
        VonSpace.md + MediaQuery.viewPaddingOf(context).bottom * 0.2,
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (running) ...[
            Padding(
              padding: const EdgeInsets.only(bottom: VonSpace.sm, left: 4),
              child: Row(
                children: [
                  Text(
                    _phaseLabel(phase),
                    style: VonText.caption.copyWith(color: VonColors.accent),
                  ),
                  if (phaseDetail.isNotEmpty) ...[
                    Text(' · ', style: VonText.caption),
                    Expanded(
                      child: Text(
                        phaseDetail,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: VonText.caption,
                      ),
                    ),
                  ],
                ],
              ),
            ),
          ],
          Row(
            crossAxisAlignment: CrossAxisAlignment.end,
            children: [
              Expanded(
                child: NeuField(
                  controller: controller,
                  hint: running
                      ? 'The agent is working…'
                      : 'What should the agent build?',
                  enabled: !running,
                  maxLines: 5,
                  minLines: 1,
                  onSubmitted: (_) => onSend(),
                  keyboardType: TextInputType.multiline,
                ),
              ),
              const SizedBox(width: VonSpace.sm),
              running
                  ? NeuIconButton(
                      icon: Icons.stop_rounded,
                      size: 48,
                      iconSize: 22,
                      tone: VonColors.danger,
                      tooltip: 'Stop the agent',
                      onPressed: onStop,
                    )
                  : NeuIconButton(
                      icon: Icons.arrow_upward_rounded,
                      size: 48,
                      iconSize: 22,
                      glow: true,
                      tooltip: 'Send',
                      onPressed: onSend,
                    ),
            ],
          ),
        ],
      ),
    );
  }

  static String _phaseLabel(String phase) => switch (phase) {
        'planning' => 'PLANNING',
        'calling_tool' => 'CALLING TOOL',
        'executing' => 'EXECUTING',
        'observing' => 'OBSERVING',
        'retrying' => 'RETRYING',
        _ => 'WORKING',
      };
}
