/**
 * The `history` namespace: replaying a stored conversation.
 *
 * A stored record is not a live transcript, and the wording here says so: the
 * messages carry no timing, no in-flight state and no token usage, so a replay
 * omits those fields rather than showing an empty one. Keys the replay shares
 * with the live transcript — the tool name labels, the `reply {index}` heading,
 * the arguments/result labels, the copy button — are reused from the `chat`
 * namespace rather than translated a second time here.
 */

export default {
  history: {
    head: {
      stored: 'stored conversation',
      turns: '{count} replies',
      note: 'A replay of a saved record. It carries no live timing, no in-flight state and no token counts — none of that was stored.',
    },

    empty: {
      none: 'No messages are stored for this conversation.',
    },

    preamble: {
      label: 'before the first reply',
    },

    system: {
      label: 'system prompt',
    },

    role: {
      user: 'you',
      unknown: 'unrecognised role “{role}”',
    },

    message: {
      empty: 'this record holds no text and no tool calls',
    },

    tool: {
      orphan: 'result with no matching call',
      noResult: 'no result was recorded for this call',
    },
  },
};
