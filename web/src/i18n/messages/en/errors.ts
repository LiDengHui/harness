/**
 * The `errors` namespace: every message the transport, the REST client and the
 * stores can put in front of the user.
 *
 * The sentences used to be literals inside the logic. They moved here so a
 * failure keeps its code and its fields, and the wording is built at the moment
 * the user reads it — which is what lets a locale switch re-render what is
 * already on screen.
 */
export default {
  errors: {
    transport: {
      openFailed: 'could not open {url}: {reason}',
      socketError: 'the socket reported an error',
      reconnecting: 'the connection dropped; reconnecting (attempt {attempt})',
      queueOverflow: 'dropped an outbound frame: {limit} were already waiting',
      flushFailed: 'could not flush the outbound queue: {reason}',
      sendFailed: 'send failed, queueing instead: {reason}',
      pongTimeout: 'no pong within {timeout} ms; treating the connection as dead',
      serverSilent: 'the server stopped answering pings; reconnecting',
      heartbeatFailed: 'heartbeat failed: {reason}',
      nonTextFrame: 'received a {kind} frame; this socket speaks text frames only',
      handlerThrew: 'a frame handler threw: {reason}',
      protocol: '{detail}',
    },

    api: {
      unreachable: '{path} is unreachable: {reason}',
      status: '{path} answered {status}',
      statusWithDetail: '{path} answered {status}: {detail}',
      notJson: '{path} did not answer with JSON: {reason}',
      unknown: 'the request failed: {reason}',
      extensionsUnavailable:
        'This server does not serve /api/extensions yet, so there is nothing to list. The panel will fill in as soon as the route exists.',
    },

    transcript: {
      backpressureLabel: 'connection closed',
      backpressure:
        'the connection was closed because this client fell behind — the server dropped it rather than queue without bound. The session is still stored; reconnect to pick it up.',
      approvalLabel: 'needs approval',
      approvalRequest: '{name} ({toolCallId}) needs approval: {reason}',
      queueFullLabel: 'queue full',
      queueFull:
        'this session already has as many messages waiting as it will hold, so this one was not queued. Let one of the waiting messages run, then send it again. {detail}',
      previewMore: '… {count} more characters',
    },

    completion: {
      end_turn: 'finished the turn',
      max_iterations: 'hit the iteration limit',
      aborted: 'aborted',
      budget_exceeded: 'ran out of budget',
      error: 'failed',
    },

    capability: {
      file_read: 'File read',
      file_write: 'File write',
      network_access: 'Network access',
      shell_exec: 'Shell exec',
    },
  },
};
