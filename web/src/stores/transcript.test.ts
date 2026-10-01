import { describe, expect, it } from 'vitest';

import { i18n, messages } from '../i18n';
import { COMPLETION_REASONS, parseServerFrame, type ServerEnvelope } from '../protocol';
import {
  appendUserEntry,
  completionReasonText,
  createTranscript,
  describeError,
  foldFrame,
  isNoRunError,
  laneTarget,
  latestUsage,
  MAX_WORKFLOW_JOBS,
  planNodeStatus,
  planNodeTarget,
  planProgress,
  previewOf,
  removeUserEntry,
  toolCalls,
  totalTokens,
  turnChanges,
  type Transcript,
  type TurnEntry,
} from './transcript';

/** Parses a frame the way the transport does, so the tests exercise both. */
function frame(fields: Record<string, unknown>): ServerEnvelope {
  const parsed = parseServerFrame(
    JSON.stringify({
      v: 1,
      session_id: 'sess_1',
      agent_id: 'default',
      ...fields,
    }),
  );
  if (!parsed.ok) throw new Error(`test frame is invalid: ${parsed.error}`);
  return parsed.envelope;
}

/** Folds a list of frames, stamping each one a millisecond apart. */
function fold(transcript: Transcript, frames: ServerEnvelope[], from = 1_000): Transcript {
  frames.forEach((envelope, index) => foldFrame(transcript, envelope, from + index));
  return transcript;
}

function session_started() {
  return frame({ type: 'session_started', model: 'mock-1' });
}

function turns(transcript: Transcript) {
  return transcript.entries.filter((entry) => entry.kind === 'turn');
}

describe('foldFrame', () => {
  it('opens a session and accumulates assistant text into one bubble', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'assistant_chunk', text: 'Hello' }),
      frame({ type: 'assistant_chunk', text: ', ' }),
      frame({ type: 'assistant_chunk', text: 'world' }),
    ]);

    expect(transcript.status).toBe('running');
    expect(transcript.model).toBe('mock-1');
    expect(transcript.startedAt).toBe(1_000);

    const [turn] = turns(transcript);
    if (!turn) throw new Error('no turn was opened');
    expect(turn.blocks).toHaveLength(1);
    const block = turn.blocks[0];
    if (!block || block.kind !== 'text') throw new Error('expected a text block');
    expect(block.text).toBe('Hello, world');
  });

  it('keeps thinking in its own block beside the answer', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'thinking_chunk', text: 'weighing' }),
      frame({ type: 'thinking_chunk', text: ' options' }),
      frame({ type: 'assistant_chunk', text: 'Answer' }),
      frame({ type: 'thinking_chunk', text: 'second thought' }),
    ]);

    const [turn] = turns(transcript);
    if (!turn) throw new Error('no turn was opened');
    expect(turn.blocks.map((block) => block.kind)).toEqual(['thinking', 'text', 'thinking']);
    const [first] = turn.blocks;
    if (!first || first.kind !== 'thinking') throw new Error('expected thinking first');
    expect(first.text).toBe('weighing options');
  });

  it('splits the answer around a tool call but keeps it in the same turn', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'assistant_chunk', text: 'Let me look.' }),
      frame({
        type: 'tool_call_start',
        tool_call_id: 'call_1',
        name: 'read_file',
        arguments: { path: 'Cargo.toml' },
      }),
      frame({ type: 'tool_call_progress', tool_call_id: 'call_1', message: 'reading' }),
      frame({
        type: 'tool_call_end',
        tool_call_id: 'call_1',
        name: 'read_file',
        status: 'ok',
        output: '[workspace]',
        duration_ms: 12,
      }),
      frame({ type: 'assistant_chunk', text: 'It is a workspace.' }),
    ]);

    expect(turns(transcript)).toHaveLength(1);
    const [turn] = turns(transcript);
    if (!turn) throw new Error('no turn');
    expect(turn.blocks.map((block) => block.kind)).toEqual(['text', 'tool', 'text']);

    const call = toolCalls(transcript)[0];
    if (!call) throw new Error('no tool call was recorded');
    expect(call.name).toBe('read_file');
    expect(call.status).toBe('ok');
    expect(call.output).toBe('[workspace]');
    expect(call.durationMs).toBe(12);
    expect(call.progress).toEqual(['reading']);
  });

  it('updates a tool card by id even when other output arrived in between', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({
        type: 'tool_call_start',
        tool_call_id: 'call_a',
        name: 'shell',
        arguments: {},
      }),
      frame({ type: 'assistant_chunk', text: 'meanwhile' }),
      frame({
        type: 'tool_call_start',
        tool_call_id: 'call_b',
        name: 'grep',
        arguments: {},
      }),
      frame({
        type: 'tool_call_end',
        tool_call_id: 'call_a',
        name: 'shell',
        status: 'timeout',
        output: 'killed',
        duration_ms: 30_000,
      }),
    ]);

    const calls = toolCalls(transcript);
    expect(calls.map((call) => call.toolCallId)).toEqual(['call_a', 'call_b']);
    expect(calls[0]?.status).toBe('timeout');
    expect(calls[1]?.status).toBe('running');
    expect(calls[1]?.endedAt).toBeNull();
  });

  it('pairs subtask start and end frames', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'subtask_start', subtask_id: 'sub_1', objective: 'review the diff' }),
      frame({
        type: 'subtask_end',
        subtask_id: 'sub_1',
        status: 'succeeded',
        summary: 'two nits',
      }),
    ]);

    const subtask = transcript.entries.find((entry) => entry.kind === 'subtask');
    if (!subtask || subtask.kind !== 'subtask') throw new Error('no subtask entry');
    expect(subtask.objective).toBe('review the diff');
    expect(subtask.status).toBe('succeeded');
    expect(subtask.summary).toBe('two nits');
    expect(subtask.endedAt).toBe(1_002);
  });

  it('announces a handoff with a lane for the agent that takes over', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({
        type: 'agent_handoff',
        from: 'backend-architect',
        to: 'code-reviewer',
        reason: 'the contract is written',
      }),
    ]);

    expect(transcript.lanes['code-reviewer']).toBeDefined();
    expect(transcript.entries.some((entry) => entry.kind === 'handoff')).toBe(true);
  });

  it('routes a sub-agent frame into its own lane and leaves the main transcript alone', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'assistant_chunk', text: 'main ' }),
      frame({ type: 'assistant_chunk', text: 'worker', subagent_id: 'test-writer' }),
      frame({ type: 'assistant_chunk', text: 'lane' }),
    ]);

    const [turn] = turns(transcript);
    if (!turn) throw new Error('no main turn');
    const mainText = turn.blocks.map((block) => (block.kind === 'text' ? block.text : '')).join('');
    expect(mainText).toBe('main lane');

    const lane = transcript.lanes['test-writer'];
    if (!lane) throw new Error('no lane was created');
    expect(lane.entries).toHaveLength(1);
    const laneTurn = lane.entries[0];
    if (!laneTurn || laneTurn.kind !== 'turn') throw new Error('no lane turn');
    expect(laneTurn.blocks).toHaveLength(1);
    const laneBlock = laneTurn.blocks[0];
    if (!laneBlock || laneBlock.kind !== 'text') throw new Error('expected lane text');
    expect(laneBlock.text).toBe('worker');
    // The lane's turn closes with the lane's own done, not the main one.
    expect(lane.openTurnId).toBe(laneTurn.id);
  });

  it('puts a subtask in the lane of the worker that is running it', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({
        type: 'agent_handoff',
        from: 'default',
        to: 'backend-architect',
        reason: 'the plan splits here',
      }),
      frame({
        type: 'subtask_start',
        subtask_id: 'sub_1',
        objective: 'sketch the API',
        subagent_id: 'backend-architect',
      }),
      frame({ type: 'assistant_chunk', text: 'drafted', subagent_id: 'backend-architect' }),
      frame({
        type: 'subtask_end',
        subtask_id: 'sub_1',
        status: 'succeeded',
        summary: 'three routes',
        subagent_id: 'backend-architect',
      }),
    ]);

    const lane = transcript.lanes['backend-architect'];
    if (!lane) throw new Error('the handoff created no lane');
    expect(lane.entries.map((entry) => entry.kind)).toEqual(['subtask', 'turn']);

    const subtask = lane.entries[0];
    if (!subtask || subtask.kind !== 'subtask') throw new Error('no subtask in the lane');
    expect(subtask.objective).toBe('sketch the API');
    expect(subtask.status).toBe('succeeded');
    expect(subtask.summary).toBe('three routes');
    expect(subtask.endedAt).toBe(1_004);

    // The main timeline keeps the delegation, not the worker's own frames.
    expect(transcript.entries.map((entry) => entry.kind)).toEqual(['handoff']);
  });

  it('closes a lane subtask even when its end frame lost the routing', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({
        type: 'subtask_start',
        subtask_id: 'sub_1',
        objective: 'sketch the API',
        subagent_id: 'backend-architect',
      }),
      frame({ type: 'subtask_end', subtask_id: 'sub_1', status: 'failed', summary: 'timed out' }),
    ]);

    const lane = transcript.lanes['backend-architect'];
    if (!lane) throw new Error('no lane was created');
    const subtask = lane.entries[0];
    if (!subtask || subtask.kind !== 'subtask') throw new Error('no subtask in the lane');
    expect(subtask.status).toBe('failed');
    expect(subtask.summary).toBe('timed out');
    expect(subtask.endedAt).toBe(1_002);
  });

  it('collects token usage and guardrails into diagnostics', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({
        type: 'token_usage',
        usage: { input_tokens: 100, output_tokens: 20 },
        budget_remaining: 8_000,
      }),
      frame({ type: 'guardrail', name: 'secrets', blocked: false, detail: 'no keys found' }),
      frame({ type: 'guardrail', name: 'secrets', blocked: true, detail: 'api key in output' }),
      frame({
        type: 'token_usage',
        usage: { input_tokens: 300, output_tokens: 40 },
        budget_remaining: null,
      }),
    ]);

    expect(transcript.diagnostics.tokens).toHaveLength(2);
    expect(transcript.diagnostics.guardrails).toHaveLength(2);
    expect(transcript.diagnostics.guardrails[1]?.blocked).toBe(true);
    expect(latestUsage(transcript)?.usage.input_tokens).toBe(300);
    expect(latestUsage(transcript)?.budgetRemaining).toBeNull();
    expect(totalTokens(transcript)).toEqual({ input_tokens: 400, output_tokens: 60 });
  });

  it('renders an error inline and leaves the run open', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'assistant_chunk', text: 'partial' }),
      frame({ type: 'error', code: 'agent_error', message: 'provider refused' }),
    ]);

    const error = transcript.entries.find((entry) => entry.kind === 'error');
    if (!error || error.kind !== 'error') throw new Error('no error entry');
    expect(error.code).toBe('agent_error');
    expect(error.message).toBe('provider refused');
    expect(turns(transcript)[0]?.endedAt).toBeNull();
    expect(transcript.status).toBe('running');
  });

  it('takes back exactly the optimistic bubble it was given, and only once', () => {
    const transcript = createTranscript('sess_1');
    const first = appendUserEntry(transcript, 'one');
    appendUserEntry(transcript, 'two');

    expect(removeUserEntry(transcript, first.id)).toBe(true);
    expect(transcript.entries.filter((entry) => entry.kind === 'user')).toHaveLength(1);
    expect(transcript.entries[0]).toMatchObject({ kind: 'user', text: 'two' });

    // A second attempt finds nothing: the id names one bubble, not a position.
    expect(removeUserEntry(transcript, first.id)).toBe(false);
    expect(transcript.entries.filter((entry) => entry.kind === 'user')).toHaveLength(1);
  });

  it('knows which error codes mean the run never started', () => {
    expect(isNoRunError('unknown_session')).toBe(true);
    expect(isNoRunError('not_running')).toBe(true);
    // An error that lands while a run is in flight is not a failed start.
    expect(isNoRunError('agent_error')).toBe(false);
    // A refused queue entry leaves the run that was already going alone.
    expect(isNoRunError('queue_full')).toBe(false);
  });

  it('closes the turn on done and opens a fresh one for the next run', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'assistant_chunk', text: 'first answer' }),
      frame({ type: 'done', reason: 'end_turn' }),
      frame({ type: 'assistant_chunk', text: 'second answer' }),
      frame({ type: 'done', reason: 'max_iterations' }),
    ]);

    const all = turns(transcript);
    expect(all).toHaveLength(2);
    expect(all[0]?.done).toBe('end_turn');
    expect(all[0]?.endedAt).toBe(1_002);
    expect(all[1]?.index).toBe(2);
    expect(all[1]?.done).toBe('max_iterations');
    expect(transcript.status).toBe('done');
    expect(transcript.openTurnId).toBeNull();
  });

  it('marks an aborted run as done, not as a failure', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'assistant_chunk', text: 'working' }),
      frame({ type: 'done', reason: 'aborted' }),
    ]);

    expect(transcript.status).toBe('done');
    expect(turns(transcript)[0]?.done).toBe('aborted');
  });

  it('records why a run stopped even when it produced no output at all', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [session_started(), frame({ type: 'done', reason: 'aborted' })]);

    expect(turns(transcript)).toHaveLength(0);
    expect(transcript.status).toBe('done');
    expect(transcript.lastCompletion).toBe('aborted');
  });

  it('carries a user entry into the transcript ahead of the answer', () => {
    const transcript = createTranscript('sess_1');
    appendUserEntry(transcript, 'read the manifest', 900);
    fold(transcript, [session_started(), frame({ type: 'assistant_chunk', text: 'ok' })]);

    expect(transcript.entries.map((entry) => entry.kind)).toEqual(['user', 'turn']);
    const [user] = transcript.entries;
    if (!user || user.kind !== 'user') throw new Error('no user entry');
    expect(user.text).toBe('read the manifest');
  });

  it('ignores pongs', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [frame({ type: 'pong' })]);
    expect(transcript.entries).toEqual([]);
  });
});

describe('a lane’s own session', () => {
  it('records the sub-agent session every frame of the run names', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({
        type: 'assistant_chunk',
        text: 'working',
        subagent_id: 'test-writer',
        subagent_session_id: 'sess_2',
      }),
    ]);

    expect(transcript.lanes['test-writer']?.subagentSessionId).toBe('sess_2');
  });

  it('leaves the lane without one when the server does not name a session', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'assistant_chunk', text: 'working', subagent_id: 'test-writer' }),
    ]);

    expect(transcript.lanes['test-writer']?.subagentSessionId).toBeNull();
  });

  it('is not overwritten by a later frame that carries no hint', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      frame({
        type: 'assistant_chunk',
        text: 'first',
        subagent_id: 'test-writer',
        subagent_session_id: 'sess_2',
      }),
      frame({ type: 'assistant_chunk', text: 'second', subagent_id: 'test-writer' }),
    ]);

    expect(transcript.lanes['test-writer']?.subagentSessionId).toBe('sess_2');
  });
});

describe('laneTarget', () => {
  it('carries the session, the worker and the objective it was given', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      frame({
        type: 'subtask_start',
        subtask_id: 'sub_1',
        objective: 'sketch the API',
        subagent_id: 'backend-architect',
        subagent_session_id: 'sess_2',
      }),
    ]);

    const lane = transcript.lanes['backend-architect'];
    if (!lane) throw new Error('no lane');
    expect(laneTarget(lane)).toEqual({
      sessionId: 'sess_2',
      subagentId: 'backend-architect',
      objective: 'sketch the API',
    });
  });

  it('says nothing about the objective when the lane recorded none', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      frame({
        type: 'agent_handoff',
        from: 'default',
        to: 'code-reviewer',
        reason: 'the plan splits here',
      }),
      frame({
        type: 'assistant_chunk',
        text: 'looking',
        subagent_id: 'code-reviewer',
        subagent_session_id: 'sess_2',
      }),
    ]);

    const lane = transcript.lanes['code-reviewer'];
    if (!lane) throw new Error('no lane');
    expect(laneTarget(lane)?.objective).toBeNull();
  });

  it('is null for a lane the server has not given a session to', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      frame({ type: 'assistant_chunk', text: 'working', subagent_id: 'test-writer' }),
    ]);

    const lane = transcript.lanes['test-writer'];
    if (!lane) throw new Error('no lane');
    expect(laneTarget(lane)).toBeNull();
  });
});

describe('previewOf', () => {
  it('returns short output untouched', () => {
    expect(previewOf('small', 100)).toBe('small');
  });

  it('truncates and says how much is left, in the active locale', () => {
    const preview = previewOf('x'.repeat(120), 100);
    const notice = i18n.global.t('errors.transcript.previewMore', { count: 20 });
    expect(preview).toBe(`${'x'.repeat(100)}\n${notice}`);
  });
});

describe('turnChanges', () => {
  /** The turn a list of frames produced, for the summary to be read off. */
  function turnOf(fields: Record<string, unknown>[]): TurnEntry {
    const transcript = createTranscript('sess_1');
    fold(transcript, [session_started(), ...fields.map((entry) => frame(entry))]);
    const [turn] = turns(transcript);
    if (!turn) throw new Error('no turn was opened');
    return turn;
  }

  /** A file-writing call, started and ended, as the bus sends it. */
  function call(
    id: string,
    name: string,
    args: Record<string, unknown>,
    output: string,
    status = 'ok',
  ): Record<string, unknown>[] {
    return [
      { type: 'tool_call_start', tool_call_id: id, name, arguments: args },
      { type: 'tool_call_end', tool_call_id: id, name, status, output, duration_ms: 1 },
    ];
  }

  it('counts a write by the lines it carried and names the path', () => {
    const turn = turnOf([
      ...call('c1', 'write_file', { path: 'src/a.ts', content: 'one\ntwo\n' }, 'wrote src/a.ts'),
    ]);

    expect(turnChanges(turn)).toEqual({
      files: [{ path: 'src/a.ts', added: 2, removed: 0 }],
      added: 2,
      removed: 0,
    });
  });

  it('counts an edit by the text it replaced, scaled by the replacements', () => {
    const turn = turnOf([
      ...call(
        'c1',
        'edit_file',
        { path: 'src/b.ts', old_string: 'x\ny', new_string: 'x' },
        'src/b.ts: 3 replacement(s)',
      ),
    ]);

    // Two lines removed and one added, three times over.
    expect(turnChanges(turn)).toEqual({
      files: [{ path: 'src/b.ts', added: 3, removed: 6 }],
      added: 3,
      removed: 6,
    });
  });

  it('leaves a failed or rejected call out, because it changed nothing', () => {
    const turn = turnOf([
      ...call('c1', 'write_file', { path: 'a.ts', content: 'x\n' }, 'nope', 'error'),
      ...call('c2', 'edit_file', { path: 'b.ts', old_string: 'x', new_string: 'y' }, 'no', 'rejected'),
    ]);

    expect(turnChanges(turn)).toBeNull();
  });

  it('ignores calls that do not write files', () => {
    const turn = turnOf([
      ...call('c1', 'read_file', { path: 'a.ts' }, 'contents'),
      ...call('c2', 'shell', { command: 'ls' }, 'a.ts'),
    ]);

    expect(turnChanges(turn)).toBeNull();
  });

  it('merges repeated edits to one path and sums the whole turn', () => {
    const turn = turnOf([
      ...call('c1', 'write_file', { path: 'a.ts', content: 'one\n' }, 'wrote a.ts'),
      ...call(
        'c2',
        'edit_file',
        { path: 'a.ts', old_string: 'one', new_string: 'one\ntwo' },
        'a.ts: 1 replacement(s)',
      ),
    ]);

    expect(turnChanges(turn)).toEqual({
      files: [{ path: 'a.ts', added: 3, removed: 1 }],
      added: 3,
      removed: 1,
    });
  });
});

describe('describeError', () => {
  /** The error entry a folded frame produced. */
  function errorEntry(fields: Record<string, unknown>) {
    const transcript = createTranscript('sess_1');
    fold(transcript, [session_started(), frame(fields)]);
    const entry = transcript.entries.find((candidate) => candidate.kind === 'error');
    if (!entry || entry.kind !== 'error') throw new Error('no error entry was folded');
    return entry;
  }

  it('explains a backpressure disconnect as a notice rather than a failure', () => {
    const entry = errorEntry({
      type: 'error',
      code: 'backpressure',
      message: 'this subscriber stopped reading',
    });

    // The frame is kept verbatim; only the presentation changes.
    expect(entry.code).toBe('backpressure');
    expect(entry.message).toBe('this subscriber stopped reading');

    const shown = describeError(entry);
    expect(shown.tone).toBe('notice');
    // Asserted against the active locale's message, never against its prose.
    expect(shown.label).toBe(i18n.global.t('errors.transcript.backpressureLabel'));
    expect(shown.message).toBe(i18n.global.t('errors.transcript.backpressure'));
    // The server's own wording is about the socket, not about the run.
    expect(shown.message).not.toContain('this subscriber stopped reading');
  });

  it('leaves a failure of the run looking like one', () => {
    const shown = describeError(
      errorEntry({ type: 'error', code: 'agent_error', message: 'provider refused' }),
    );

    // An unknown code is the server's own message: the label is the code and the
    // wording passes through untouched.
    expect(shown).toEqual({ label: 'agent_error', message: 'provider refused', tone: 'error' });
  });

  it('composes an approval request from the fields the frame carried', () => {
    const entry = errorEntry({
      type: 'tool_approval_request',
      tool_call_id: 'call_7',
      name: 'shell',
      arguments: {},
      reason: 'the command writes outside the workspace',
    });

    // The entry keeps the fields; the sentence is built when it is read.
    expect(entry.code).toBe('tool_approval_request');
    expect(entry.approval).toEqual({
      name: 'shell',
      toolCallId: 'call_7',
      reason: 'the command writes outside the workspace',
    });

    const shown = describeError(entry);
    expect(shown.tone).toBe('error');
    expect(shown.label).toBe(i18n.global.t('errors.transcript.approvalLabel'));
    expect(shown.message).toBe(
      i18n.global.t('errors.transcript.approvalRequest', {
        name: 'shell',
        toolCallId: 'call_7',
        reason: 'the command writes outside the workspace',
      }),
    );
  });
});

describe('completionReasonText', () => {
  it('words every reason the server can send, from the locale files', () => {
    for (const reason of COMPLETION_REASONS) {
      const text = completionReasonText(reason);
      expect(text).toBe(i18n.global.t(`errors.completion.${reason}`));
      // A missing key resolves to the key itself, so this catches a gap.
      expect(text).not.toContain('errors.completion');
    }
  });
});

describe('error wording', () => {
  /** The raw message for a dotted key, straight from the locale tree. */
  function rawMessage(locale: string, path: string): string {
    const found = path
      .split('.')
      .reduce<unknown>(
        (node, part) => (node as Record<string, unknown> | undefined)?.[part],
        messages[locale],
      );
    if (typeof found !== 'string') throw new Error(`${path} is missing from ${locale}`);
    return found;
  }

  it('renders one failure differently per locale and follows a switch', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'error', code: 'backpressure', message: 'this subscriber stopped reading' }),
    ]);
    const entry = transcript.entries.find((candidate) => candidate.kind === 'error');
    if (!entry || entry.kind !== 'error') throw new Error('no error entry was folded');

    i18n.global.locale.value = 'en';
    const english = describeError(entry).message;
    i18n.global.locale.value = 'zh-CN';
    const chinese = describeError(entry).message;

    expect(english).toBe(rawMessage('en', 'errors.transcript.backpressure'));
    expect(chinese).toBe(rawMessage('zh-CN', 'errors.transcript.backpressure'));
    expect(english).not.toBe(chinese);
  });
});

describe('the plan of a run', () => {
  /** A plan frame with the given nodes, as the server would send it. */
  function planFrame(nodes: Record<string, unknown>[], workflowId?: string) {
    return frame({
      type: 'plan_created',
      nodes,
      ...(workflowId === undefined ? {} : { workflow_id: workflowId }),
    });
  }

  /** A node's start frame, keyed to the plan node it runs. */
  function nodeStart(nodeId: string, objective: string, laneId = `lane_${nodeId}`) {
    return frame({ type: 'subtask_start', subtask_id: laneId, node_id: nodeId, objective });
  }

  /** A node's terminal frame. */
  function nodeEnd(
    nodeId: string,
    status: string,
    summary = 'done',
    laneId = `lane_${nodeId}`,
  ) {
    return frame({ type: 'subtask_end', subtask_id: laneId, node_id: nodeId, status, summary });
  }

  it('takes the whole graph from the plan frame, not only the first step', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame(
        [
          { id: 'a', objective: 'sketch the API', agent: 'backend-architect', depends_on: [] },
          { id: 'b', objective: 'review it', depends_on: ['a'] },
        ],
        'ship-it',
      ),
    ]);

    expect(transcript.plan).toEqual({
      nodes: [
        { id: 'a', objective: 'sketch the API', agent: 'backend-architect', dependsOn: [] },
        { id: 'b', objective: 'review it', agent: null, dependsOn: ['a'] },
      ],
      workflowId: 'ship-it',
      source: 'frame',
    });
  });

  it('keys a row on the node id, which is not the worker’s lane id', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([{ id: 'sketch', objective: 'sketch the API' }]),
      nodeStart('sketch', 'sketch the API', 'lane_9'),
    ]);

    const node = transcript.plan?.nodes[0];
    if (!node) throw new Error('no plan node');
    expect(transcript.lanes['default']).toBeUndefined();
    // The record is found by node id even though its lane id differs.
    expect(planNodeStatus(transcript, node)).toBe('running');
  });

  it('reads a step’s status from its own subtask frames', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([{ id: 'sketch', objective: 'sketch the API' }]),
    ]);
    const node = transcript.plan?.nodes[0];
    if (!node) throw new Error('no plan node');

    // Before it starts, while the run is in flight: waiting.
    expect(planNodeStatus(transcript, node)).toBe('pending');

    fold(transcript, [nodeStart('sketch', 'sketch the API')]);
    expect(planNodeStatus(transcript, node)).toBe('running');

    fold(transcript, [nodeEnd('sketch', 'succeeded')]);
    expect(planNodeStatus(transcript, node)).toBe('succeeded');
  });

  it('ends a step the run never reached as skipped, never blank', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([
        { id: 'a', objective: 'sketch the API' },
        { id: 'b', objective: 'never reached' },
      ]),
      nodeStart('a', 'sketch the API'),
      nodeEnd('a', 'succeeded'),
      frame({ type: 'done', reason: 'end_turn' }),
    ]);

    const [, unreached] = transcript.plan?.nodes ?? [];
    if (!unreached) throw new Error('no second node');
    expect(transcript.status).toBe('done');
    expect(planNodeStatus(transcript, unreached)).toBe('skipped');
    expect(planProgress(transcript)).toEqual({ total: 2, finished: 2 });
  });

  it('ends a step whose worker never closed as skipped once the run is over', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([{ id: 'a', objective: 'sketch the API' }]),
      nodeStart('a', 'sketch the API'),
      frame({ type: 'done', reason: 'aborted' }),
    ]);

    const node = transcript.plan?.nodes[0];
    if (!node) throw new Error('no plan node');
    // Nothing is still running once the run has ended.
    expect(planNodeStatus(transcript, node)).toBe('skipped');
  });

  it('keeps a step’s failure as failed rather than as skipped', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([{ id: 'a', objective: 'sketch the API' }]),
      nodeStart('a', 'sketch the API'),
      nodeEnd('a', 'failed', 'timed out'),
      frame({ type: 'done', reason: 'error' }),
    ]);

    const node = transcript.plan?.nodes[0];
    if (!node) throw new Error('no plan node');
    expect(planNodeStatus(transcript, node)).toBe('failed');
  });

  it('assembles a plan from the steps that start when no plan frame arrives', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({
        type: 'subtask_start',
        subtask_id: 'lane_1',
        node_id: 'sketch',
        objective: 'sketch the API',
        subagent_id: 'backend-architect',
      }),
      frame({ type: 'subtask_start', subtask_id: 'lane_2', node_id: 'review', objective: 'review it' }),
    ]);

    expect(transcript.plan?.source).toBe('inferred');
    expect(transcript.plan?.workflowId).toBeNull();
    expect(transcript.plan?.nodes.map((node) => node.id)).toEqual(['sketch', 'review']);
    expect(transcript.plan?.nodes[0]?.agent).toBe('backend-architect');
  });

  it('falls back to the lane id for a server that sends no node id', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'subtask_start', subtask_id: 'lane_1', objective: 'sketch the API' }),
    ]);

    expect(transcript.plan?.nodes.map((node) => node.id)).toEqual(['lane_1']);
  });

  it('adds a running step the plan frame did not list instead of dropping it', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([{ id: 'a', objective: 'sketch the API' }]),
      nodeStart('surprise', 'an extra step'),
    ]);

    expect(transcript.plan?.nodes.map((node) => node.id)).toEqual(['a', 'surprise']);
    // A step that joined an announced plan does not make the whole plan inferred.
    expect(transcript.plan?.source).toBe('frame');
  });

  it('retires the previous run’s plan when the next run starts', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [session_started(), planFrame([{ id: 'a', objective: 'do a' }])]);
    expect(transcript.plan).not.toBeNull();

    // A second run on the same session: no session_started, only the new output.
    fold(transcript, [frame({ type: 'done', reason: 'end_turn' })]);
    fold(transcript, [frame({ type: 'assistant_chunk', text: 'a fresh run' })]);

    expect(transcript.plan).toBeNull();
  });

  it('ignores an empty plan rather than blanking the checklist', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([{ id: 'a', objective: 'do a' }]),
      frame({ type: 'plan_created', nodes: [] }),
    ]);

    expect(transcript.plan?.nodes.map((node) => node.id)).toEqual(['a']);
  });

  it('opens a step through the lane that recorded it', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([{ id: 'sketch', objective: 'sketch the API', agent: 'backend-architect' }]),
      frame({
        type: 'subtask_start',
        subtask_id: 'lane_1',
        node_id: 'sketch',
        objective: 'sketch the API',
        subagent_id: 'backend-architect',
        subagent_session_id: 'sess_2',
      }),
    ]);

    expect(planNodeTarget(transcript, 'sketch')).toEqual({
      sessionId: 'sess_2',
      subagentId: 'backend-architect',
      objective: 'sketch the API',
    });
  });

  it('opens each step’s own conversation when several ran under one agent', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([
        { id: 'a', objective: 'sketch the API', agent: 'default' },
        { id: 'b', objective: 'review it', agent: 'default' },
      ]),
      frame({
        type: 'subtask_start',
        subtask_id: 'lane_1',
        node_id: 'a',
        objective: 'sketch the API',
        subagent_id: 'default',
        subagent_session_id: 'sess_a',
      }),
      frame({
        type: 'subtask_start',
        subtask_id: 'lane_2',
        node_id: 'b',
        objective: 'review it',
        subagent_id: 'default',
        subagent_session_id: 'sess_b',
      }),
    ]);

    // Both workers share the `default` lane, so a row that read the lane would
    // open its sibling's conversation — or the first one it ever saw.
    expect(planNodeTarget(transcript, 'a')?.sessionId).toBe('sess_a');
    expect(planNodeTarget(transcript, 'b')?.sessionId).toBe('sess_b');
    expect(planNodeTarget(transcript, 'a')?.objective).toBe('sketch the API');
    expect(planNodeTarget(transcript, 'b')?.objective).toBe('review it');
  });

  it('reads a repeated node id from the run on screen, not from the one before it', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([{ id: 'review', objective: 'first run’s review' }]),
      nodeStart('review', 'first run’s review', 'lane_1'),
      nodeEnd('review', 'succeeded', 'done', 'lane_1'),
      frame({ type: 'done', reason: 'end_turn' }),
    ]);

    // A second run reuses the planner's slug: the row must read this run.
    fold(
      transcript,
      [
        frame({ type: 'assistant_chunk', text: 'a second run' }),
        planFrame([{ id: 'review', objective: 'second run’s review' }]),
        frame({
          type: 'subtask_start',
          subtask_id: 'lane_2',
          node_id: 'review',
          objective: 'second run’s review',
          subagent_id: 'default',
          subagent_session_id: 'sess_second',
        }),
      ],
      2_000,
    );

    const node = transcript.plan?.nodes[0];
    if (!node) throw new Error('no plan node');
    expect(planNodeStatus(transcript, node)).toBe('running');
    expect(planNodeTarget(transcript, 'review')).toEqual({
      sessionId: 'sess_second',
      subagentId: 'default',
      objective: 'second run’s review',
    });
  });

  it('offers no conversation for a step that never ran', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      planFrame([{ id: 'never', objective: 'never reached' }]),
      frame({ type: 'done', reason: 'aborted' }),
    ]);

    expect(planNodeTarget(transcript, 'never')).toBeNull();
  });

  it('reports no progress for a transcript with no plan', () => {
    expect(planProgress(createTranscript('sess_1'))).toEqual({ total: 0, finished: 0 });
  });
});

describe('the workflow queue', () => {
  /** The queued frame a job starts with. */
  function queuedFrame(overrides: Record<string, unknown> = {}) {
    return frame({
      type: 'workflow_queued',
      job_id: 7,
      task: 'ship the endpoint',
      workflow_id: 'ship-it',
      position: 1,
      ...overrides,
    });
  }

  it('keeps one row across every state change', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [session_started(), queuedFrame()]);

    expect(transcript.workflowQueue).toHaveLength(1);
    expect(transcript.workflowQueue[0]?.state).toBe('queued');
    expect(transcript.workflowQueue[0]?.position).toBe(1);
    expect(transcript.workflowQueue[0]?.sessionId).toBe('sess_1');
    // The task is not the user's message until the job actually starts.
    expect(transcript.entries.filter((entry) => entry.kind === 'user')).toHaveLength(0);

    fold(transcript, [
      frame({ type: 'workflow_started', job_id: 7, task: 'ship the endpoint', workflow_id: 'ship-it' }),
    ]);
    expect(transcript.workflowQueue).toHaveLength(1);
    expect(transcript.workflowQueue[0]?.state).toBe('running');

    fold(transcript, [
      frame({
        type: 'workflow_finished',
        job_id: 7,
        status: 'succeeded',
        summary: '3 of 3 node(s) succeeded',
      }),
    ]);
    expect(transcript.workflowQueue).toHaveLength(1);
    expect(transcript.workflowQueue[0]?.state).toBe('finished');
    expect(transcript.workflowQueue[0]?.status).toBe('succeeded');
    expect(transcript.workflowQueue[0]?.summary).toBe('3 of 3 node(s) succeeded');
    // The terminal frame names only the job, so the task it was queued with stays.
    expect(transcript.workflowQueue[0]?.task).toBe('ship the endpoint');
  });

  it('places the user’s own words when the job stops waiting, and only once', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      queuedFrame(),
      frame({ type: 'workflow_started', job_id: 7, task: 'ship the endpoint' }),
    ]);

    expect(transcript.entries.map((entry) => entry.kind)).toEqual(['user']);
    expect(transcript.entries[0]).toMatchObject({ kind: 'user', text: 'ship the endpoint' });

    // The later frames update the row; they do not repeat the bubble.
    fold(transcript, [
      frame({ type: 'workflow_finished', job_id: 7, status: 'succeeded', summary: 'done' }),
    ]);
    expect(transcript.entries.filter((entry) => entry.kind === 'user')).toHaveLength(1);
  });

  it('draws the bubble for a job first seen already running', () => {
    const transcript = createTranscript('sess_1');
    fold(transcript, [
      session_started(),
      frame({ type: 'workflow_started', job_id: 7, task: 'ship it' }),
    ]);

    expect(transcript.entries.filter((entry) => entry.kind === 'user')).toHaveLength(1);
    expect(transcript.workflowQueue[0]?.userEntryId).not.toBeNull();
  });

  it('is bounded, dropping the oldest jobs', () => {
    const transcript = createTranscript('sess_1');
    const overflow = MAX_WORKFLOW_JOBS + 3;
    for (let index = 0; index < overflow; index += 1) {
      fold(transcript, [queuedFrame({ job_id: index, task: `t${index}` })]);
    }

    expect(transcript.workflowQueue).toHaveLength(MAX_WORKFLOW_JOBS);
    expect(transcript.workflowQueue[0]?.id).toBe('3');
    expect(transcript.workflowQueue[transcript.workflowQueue.length - 1]?.id).toBe(
      String(overflow - 1),
    );
  });
});
