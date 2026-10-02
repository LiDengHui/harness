/**
 * The `chat` namespace: the chat surface — the session rail, the transcript,
 * the composer, the diagnostics strip and the connection badge.
 *
 * Wire enums (`toolStatus.*`, `error.codes.*`, `sessionStatus.*`) are keyed by
 * the token itself, so a value that arrives off the bus resolves by name and
 * anything unknown falls back to the token as it came. A few sentences are the
 * exception, because they do not originate here: completion reasons read through
 * the store's `completionReasonText` (`errors.completion.*`), and a queue that
 * would not take another message reads through `errors.transcript.queueFull*`,
 * since it is the store that decides what was accepted.
 *
 * `plan.status.*` is how one step of the plan checklist reads. It is close in
 * wording to `views.entries.status.*` but is a different place — that one is a
 * sub-task record card, this one is a checklist row — so the two are worded
 * separately. Both are read through a table (`PLAN_STATUS_KEYS`, `STATUS_KEYS`),
 * which is why their key names are written out as literals there.
 */

export default {
  chat: {
    rail: {
      title: 'Sessions',
      open: 'sessions',
      openTitle: 'Open the session list',
      newSession: 'new',
      newSessionTitle: 'Open a fresh connection, which is what a fresh session needs',
      loading: 'loading…',
      empty: 'No sessions stored yet.',
      live: 'live',
      nodes: '{count} nodes',
      tokens: '{count} tok',
      statsSessions: '{sessions} sessions · {nodes} nodes',
      statsTokens: '{tokens} tokens · {blobs} blobs',
    },

    head: {
      liveSession: 'live session {session}',
    },

    sessionStatus: {
      idle: 'idle',
      running: 'running',
      done: 'done',
      error: 'error',
    },

    empty: {
      attached:
        'Attached, but the bus replays nothing: only frames that arrive from now on appear here.',
    },

    composer: {
      mode: 'send as',
      ask: 'answer directly',
      askTitle: 'Hand this text straight to the agent to answer',
      plan: 'split into steps',
      planTitle:
        'Have the agent break the task into steps and hand them to sub-agents',
      planHint: 'the agent returns a plan and delegates it, instead of doing the work here',
      permission: {
        label: 'permissions',
        alwaysAsk: 'always ask',
        askWhenNeeded: 'ask when needed',
        fullAuto: 'fully automatic',
        hint: {
          alwaysAsk: 'ask before every tool call: nothing runs until you approve it',
          askWhenNeeded: 'ask only when a call needs a permission the run does not have yet',
          fullAuto: 'run every tool call without asking',
        },
      },
      agentDefault: 'agent: default',
      workflowLabel: 'workflow',
      workflowAuto: 'workflow: automatic',
      workflowNew: '＋ new workflow…',
      workflowUnavailable: 'the server does not list workflows yet, so this stays automatic.',
      workflowWhen: 'use this workflow when: {when}',
      workflowCreateTitle: 'New workflow',
      workflowCreateHint: 'Describe when it applies and what it does; the server writes it from that.',
      workflowDescribePlaceholder: 'e.g. when a task changes both the front end and the back end…',
      workflowCreate: 'create',
      workflowCreating: 'creating…',
      workflowCancel: 'cancel',
      placeholderAsk: 'Ask for something. Enter sends, Shift+Enter adds a line.',
      placeholderPlan: 'Describe the task to split. Enter sends, Shift+Enter adds a line.',
      send: 'send',
      planSend: 'split it',
      stop: 'stop',
      steeringPlaceholder: 'Add a note mid-run — it reaches the agent right away',
      steer: 'add a note',
      steeringIdle: 'available only while a task is running',
      steeringSent: 'delivered to the running task: {text}',
    },

    /**
     * The permission panel: a tool call stopped until the user decides. It is
     * non-modal — the rail and the navigation stay usable — so the wording is a
     * request rather than an alarm.
     */
    approval: {
      heading: 'A tool needs your permission',
      waiting: '{count} waiting',
      tool: 'tool: {name}',
      reason: 'stopped because: {reason}',
      approve: 'approve',
      deny: 'deny',
    },

    lane: {
      label: 'sub-agent',
      labelTitle:
        'A sub-agent is a helper the main agent delegated work to. Its records are folded away here so they do not interrupt the main thread; when the server has stored the sub-agent’s own session, its conversation can be opened from here.',
      entries: 'entries: {count}',
      open: 'open its conversation',
      openTitle:
        'Read what this sub-agent did, in the conversation the server stored for it',
      noSession: 'cannot be opened yet',
      noSessionTitle:
        'The server has not reported a session for this sub-agent, so its own conversation cannot be opened. Its records are still folded below.',
    },

    /**
     * The trace: the pane that opens when a sub-agent’s conversation is read.
     * It says what the reader is looking at, and gives them the way back to the
     * conversation the work belonged to.
     */
    trace: {
      badge: 'sub-agent conversation',
      worker: 'sub-agent {id}',
      belongs: 'part of {title}',
      objective: 'asked to: {objective}',
      back: 'back to {title}',
      backTitle: 'Return to the main conversation this sub-agent worked for',
      parentGone: 'its main conversation is no longer in the session list',
    },

    runFinished: 'this run has ended · {reason}',

    /**
     * The plan checklist: the steps the current run is executing. Each row is a
     * status line, not a second transcript — opening one is how the reader gets
     * to that step's own sub-agent conversation, where the detail lives.
     */
    plan: {
      heading: 'this run’s plan',
      progress: '{finished}/{total} done',
      inferred: 'the server has not sent the full plan yet; the steps below are the ones that have started.',
      fromWorkflow: 'from workflow {id}',
      fromWorkflowTitle: 'this plan came from the chosen workflow rather than being made up on the spot',
      expand: 'expand',
      collapse: 'collapse',
      openTitle: 'Open this step’s sub-agent conversation and read what it actually did',
      agent: 'agent {agent}',
      status: {
        pending: 'waiting',
        running: 'running',
        succeeded: 'done',
        failed: 'failed',
        skipped: 'skipped',
      },
    },

    /**
     * A duration shows up in two places — a whole reply and a single tool call —
     * so the wording lives here once. `done` is a finished time, `elapsed` is one
     * that is still counting up.
     */
    duration: {
      done: 'took {ms} ms',
      elapsed: '{ms} ms so far',
    },

    turn: {
      label: 'reply {index}',
      labelTitle:
        'One complete reply from the model: its reasoning, tool calls and text, up to the end of the turn.',
      agent: 'agent {id}',
      thinking: 'the model reasoning · {count} chars',
      thinkingTitle:
        'What the model wrote to itself before answering. It explains the answer; it does not run anything.',
    },

    code: {
      copy: 'copy code',
    },

    change: {
      summary: 'files changed in this reply: {count}',
      deltaTitle:
        'Line counts come from the tool call itself: a write_file counts the lines it wrote, an edit_file counts the text it replaced. A whole-file write cannot see what it displaced, so those removals are not counted.',
      addedTitle: 'lines added',
      removedTitle: 'lines removed',
    },

    tool: {
      nameTitle: 'Tool id: {name}. Use it to search the logs or the source for this call.',
      idTitle:
        'The line above is the internal name of the tool; this line is the id of this particular call, for matching against the server log.',
      arguments: 'arguments',
      progress: 'progress',
      output: 'result',
      showFull: 'show the full result ({count} chars)',
      hideFull: 'hide the full result',
    },

    /**
     * A tool internal name reads like a variable, so the folded line shows plain
     * words instead. The name itself is never lost: it stays in the tooltip on
     * that line and in the id line once the card is open, because it is the
     * string you search the logs for.
     */
    toolNames: {
      read_file: 'read file',
      write_file: 'write file',
      edit_file: 'edit file',
      list_dir: 'list directory',
      grep: 'search file contents',
      shell: 'run a command',
      web_fetch: 'fetch a web page',
      mcp: 'external tool {tool} (from {server})',
    },

    diagnostics: {
      title: 'usage & guardrails',
      toggleTitle:
        'Token usage, remaining budget and safety blocks for this run. Open it to see what each figure means.',
      brief: 'tokens used this session: {count}',
      usageTitle:
        'Usage of the most recent model call: input is what was sent to the model, output is what it wrote back.',
      usage: 'last call: {input} in · {output} out',
      totalTitle: 'Summed over every model call in this session so far.',
      total: '{count} this session',
      budget: '{count} left',
      budgetUnbounded: 'no limit set',
      noReports: 'no usage reported yet',
      blocked: ' (blocked)',
      guardrails: 'safety checks: {count}',
      guardrailsTitle:
        'Safety checks are the rules the server runs against each tool call — credentials, personal data, out-of-policy actions.',
      guardrailsBlocked: '{count} blocked',
      guardrailNames: {
        secret_scanner: 'secret scan',
        tool_policy: 'tool policy',
        pii_detector: 'privacy check',
        llm_judge: 'model review',
        content_fence: 'prompt-injection guard',
        behavior_monitor: 'repeat-call check',
      },
      lanes: 'sub-agents: {lanes}',
    },

    connection: {
      connected: 'connected — updates arrive live',
      connecting: 'connecting to the server…',
      reconnecting: 'connection lost, retrying (attempt {attempt})',
      reconnectingPlain: 'connection lost, retrying…',
      disconnected: 'not connected — press Reconnect to restore it',
      queued: '{count} messages waiting to send',
      queuedTitle: '{count} messages waiting for the connection to come back',
      connect: 'reconnect',
    },

    error: {
      codeTitle:
        'Error code from the server: {code}. Use it to search the logs when something goes wrong.',
      codes: {
        busy: 'server busy',
        not_running: 'nothing is running',
        agent_not_found: 'agent not found',
        unknown_session: 'session not found',
        bad_message: 'malformed message',
        config: 'configuration error',
        session_error: 'session error',
        guardrail: 'blocked by a safety rule',
      },
    },

    toolStatus: {
      running: 'running',
      ok: 'done',
      error: 'failed',
      rejected: 'refused',
      timeout: 'timed out',
    },
  },
};
