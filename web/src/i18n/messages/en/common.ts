/**
 * The `common` namespace: chrome that belongs to no single screen — the app
 * title, the navigation, the generic buttons, and the switcher's own label.
 *
 * Keys here are the ones every view needs, so they live in one file rather
 * than being re-declared per screen. `actions` holds only the buttons the
 * screens actually share: the composer's own `send` and `stop` stay with the
 * composer, because they are part of its mode vocabulary rather than chrome.
 *
 * `rail`, `session`, `time`, `composer`, `queue`, `workflow`, `workflowQueue`,
 * `replay` and `effort` are the shell's non-content vocabulary: a group heading,
 * the state of this connection, a relative age and a run readout describe the
 * session list and the run itself rather than any one frame; `queue` names the
 * messages already accepted but not started yet, `workflow` names the choice of
 * recipe for a task, and `workflowQueue` names the workflow jobs accepted but
 * not started yet — kept apart from `queue` because a reader has to be able to
 * tell "a message is waiting" from "a workflow is waiting"; `replay` describes
 * the act of reading a past session — loading, unreadable — which is not
 * conversation content either. `effort` names how hard the next message should
 * be thought about: a composer setting, not something the model said. The
 * `session` sentences are about *this connection*: a rail holding dozens of
 * stored sessions is not the same thing as this connection having one, and the
 * last says what it means when a reload cannot reopen the session it remembered.
 */

export default {
  common: {
    appTitle: 'harness',
    nav: {
      chat: 'Chat',
      agents: 'Agents',
      extensions: 'Extensions',
      ariaLabel: 'Primary navigation',
    },
    actions: {
      retry: 'Retry',
      close: 'Close',
    },
    language: {
      switch: 'Switch to {language}',
    },
    rail: {
      search: 'Search sessions',
      noMatch: 'No session matches that.',
      showMore: 'show more',
      showLess: 'show less',
      agent: 'agent {agent}',
      untitled: 'no message yet',
      forkedFrom: 'forked from {title}',
      running: 'running',
      runningTitle: 'this session has a run in flight',
      queued: '{count} waiting',
      queuedTitle: '{count} messages waiting to run on this session',
      subagentTag: 'sub-agent',
      subagentCount: 'sub-agent conversations: {count}',
      subagentCountTitle:
        'Work this conversation handed to sub-agents. Each one is stored as a session of its own; open this to list them, then pick one to read what it actually did.',
      subagentOpenTitle: 'Show the sub-agent conversations of this session',
      subagentCloseTitle: 'Hide the sub-agent conversations of this session',
      statsHint: 'Totals across the session store: sessions, nodes, estimated tokens and blobs',
      group: {
        serve: 'Web sessions',
        run: 'Command line',
      },
      groupHint: {
        serve: 'Grouped by how the session was started, not by project: these came from the web UI (the browser front end of harness serve)',
        run: 'Grouped by how the session was started, not by project: these came from the command line (harness run)',
      },
    },
    session: {
      noSessionHere: 'This connection has no session yet — send a message to start one.',
      notAttached: 'no session on this connection',
      restoreMissing:
        'The session this page was in could not be reopened: this server no longer has it. Send a message to start a new one.',
    },
    time: {
      justNow: 'just now',
      minutes: '{count}m',
      hours: '{count}h',
      days: '{count}d',
    },
    composer: {
      turns: 'turn {count}',
      modelHint: 'the model this session resolved to (from the session_started frame)',
    },
    effort: {
      label: 'thinking level',
      low: 'Fast',
      high: 'Balanced',
      max: 'Thorough',
      hint: {
        low: 'answers faster and uses fewer tokens: for simple, direct questions',
        high: 'the default: a normal amount of thinking, balancing speed and depth',
        max: 'thinks longest and deepest: for the hardest questions, slower and costlier',
      },
    },
    queue: {
      heading: 'Waiting to run ({count})',
      next: 'runs next',
      ahead: '{count} ahead',
    },
    workflow: {
      pickTitle: 'Pick a workflow for this task; leave it empty to let the server choose',
    },
    workflowQueue: {
      heading: 'Workflow queue ({count})',
      tag: 'workflow',
      queued: 'queued',
      running: 'running',
      finished: 'finished',
      failed: 'failed',
      skipped: 'skipped',
      openPlan: 'view plan',
      openPlanTitle: 'Open the plan this workflow job produced',
    },
    replay: {
      badge: 'Replay',
      loading: 'Loading this session’s history…',
      unavailable: 'This session’s history cannot be read right now: the server does not serve it yet, or the session no longer exists.',
    },
  },
};
