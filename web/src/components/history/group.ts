/**
 * The message → turn grouping a replay is rendered from.
 *
 * A live transcript is *folded* from a stream of frames
 * (`stores/transcript.ts`), which has no `turn_started` frame and no user echo,
 * so it infers a turn from the assistant's output. A stored conversation arrives
 * instead as a flat list of messages in the harness's own shape
 * (`{ role, content?, tool_calls?, tool_call_id?, name? }`), and that list comes
 * back from a database over REST. Nothing about it is trusted here: every field
 * is re-read and narrowed before it is used, and a record this module cannot
 * make sense of is kept as a note rather than dropped.
 *
 * Two rules do the work:
 *
 *   * a `user` message opens a turn, and every message after it belongs to that
 *     turn until the next `user` message. The stored record has a user echo,
 *     which is the honest boundary; the live transcript cannot use one because
 *     the bus never sends it;
 *   * an assistant message's `tool_calls` are paired with the `tool` messages
 *     that answer them by `tool_call_id`, so a result is read inside the card
 *     for the call it belongs to instead of as a message of its own. A result
 *     that matches no visible call is kept as a standalone entry.
 *
 * The module is deliberately free of Vue and of i18n so it can be tested in the
 * node environment the rest of this suite uses.
 */

/** One tool call an assistant message asked for. */
export interface StoredToolCall {
  kind: 'call';
  /** The wire id; `''` when the record carried none, which also means unpaired. */
  id: string;
  name: string;
  /** Kept as it arrived — an object, a string, or absent — and rendered by shape. */
  arguments: unknown;
  /** The answering tool message's text; `null` when no answer was recorded. */
  result: string | null;
  /** The `name` field on the answering tool message, if it carried one. */
  resultName: string | null;
}

/** A `tool` message whose `tool_call_id` matches no visible call. */
export interface StoredToolResult {
  kind: 'result';
  toolCallId: string | null;
  name: string | null;
  content: string;
}

export type StoredTool = StoredToolCall | StoredToolResult;

/** A record that is neither readable text nor a tool exchange. */
export interface HistoryNote {
  /** The wire role, verbatim — `system`, or whatever an unknown record said. */
  role: string;
  /** The text, or `null` for a record with neither text nor tool calls. */
  text: string | null;
}

export type HistoryBlock =
  | { kind: 'text'; text: string }
  | { kind: 'tool'; tool: StoredTool }
  | { kind: 'note'; note: HistoryNote };

export interface HistoryTurn {
  /** 1-based, so it reads like the live transcript's `reply {index}` heading. */
  index: number;
  /** The user message's text, or `null` when it carried none. */
  prompt: string | null;
  blocks: HistoryBlock[];
}

export interface GroupedHistory {
  /** Blocks before the first user message — usually the system prompt. */
  preamble: HistoryBlock[];
  turns: HistoryTurn[];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** A non-empty string, or `null` — the one place an empty `content` becomes "no text". */
function textOf(value: unknown): string | null {
  return typeof value === 'string' && value !== '' ? value : null;
}

/**
 * One entry of `tool_calls`, or `null` when it cannot be labelled.
 *
 * A call without a name has nothing to show and cannot be matched to a result,
 * so it is skipped; a call without an id is kept but never registered for
 * pairing, because an empty id would collide with every other one.
 */
function callOf(raw: unknown): StoredToolCall | null {
  if (!isRecord(raw)) return null;
  const name = typeof raw.name === 'string' ? raw.name : null;
  if (name === null) return null;
  return {
    kind: 'call',
    id: typeof raw.id === 'string' ? raw.id : '',
    name,
    arguments: raw.arguments,
    result: null,
    resultName: null,
  };
}

/**
 * Groups a stored message list into a preamble and its turns.
 *
 * Accepts `unknown` on purpose: the array is a replayed database record, and a
 * caller that hands over something else (a missing field, a string, `null`)
 * gets an empty result rather than an exception.
 */
export function groupMessages(input: unknown): GroupedHistory {
  const messages = Array.isArray(input) ? input : [];
  const preamble: HistoryBlock[] = [];
  const turns: HistoryTurn[] = [];
  /** Every call seen so far, so a later `tool` message can find what it answers. */
  const calls = new Map<string, StoredToolCall>();
  let open: HistoryTurn | null = null;

  /** Pushes a message's tool calls onto `target`, registering each for pairing. */
  const pushCalls = (target: HistoryBlock[], raw: unknown): number => {
    if (!Array.isArray(raw)) return 0;
    let pushed = 0;
    for (const entry of raw) {
      const call = callOf(entry);
      if (call === null) continue;
      if (call.id !== '') calls.set(call.id, call);
      target.push({ kind: 'tool', tool: call });
      pushed += 1;
    }
    return pushed;
  };

  for (const raw of messages) {
    if (!isRecord(raw)) continue;
    const role = typeof raw.role === 'string' ? raw.role : '';
    const text = textOf(raw.content);
    const callsRaw = raw.tool_calls;

    if (role === 'user') {
      const turn: HistoryTurn = { index: turns.length + 1, prompt: text, blocks: [] };
      turns.push(turn);
      open = turn;
      const pushed = pushCalls(turn.blocks, callsRaw);
      // A user message with neither text nor calls still opened a turn; say so
      // rather than rendering an empty bubble.
      if (text === null && pushed === 0) {
        turn.blocks.push({ kind: 'note', note: { role, text: null } });
      }
      continue;
    }

    const target = open === null ? preamble : open.blocks;

    if (role === 'tool') {
      const toolCallId = typeof raw.tool_call_id === 'string' ? raw.tool_call_id : null;
      const name = typeof raw.name === 'string' ? raw.name : null;
      const paired = toolCallId === null ? undefined : calls.get(toolCallId);
      if (paired !== undefined) {
        paired.result = text ?? '';
        paired.resultName = name;
      } else {
        target.push({
          kind: 'tool',
          tool: { kind: 'result', toolCallId, name, content: text ?? '' },
        });
      }
      // A `tool` message carrying tool calls is malformed; keep them anyway.
      pushCalls(target, callsRaw);
      continue;
    }

    if (role === 'assistant') {
      if (text !== null) target.push({ kind: 'text', text });
      const pushed = pushCalls(target, callsRaw);
      if (text === null && pushed === 0) {
        target.push({ kind: 'note', note: { role, text: null } });
      }
      continue;
    }

    // `system`, and any role this build does not know: a note, folded away.
    if (text !== null) target.push({ kind: 'note', note: { role, text } });
    const pushed = pushCalls(target, callsRaw);
    if (text === null && pushed === 0) {
      target.push({ kind: 'note', note: { role, text: null } });
    }
  }

  return { preamble, turns };
}
