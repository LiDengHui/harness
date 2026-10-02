import { describe, expect, it } from 'vitest';

import { parseServerFrame } from './parse';
import { PROTOCOL_VERSION, clientEnvelope, serializeClientEnvelope } from './types';

/** A frame in the shape `ServerEnvelope` produces on the wire. */
function frame(fields: Record<string, unknown>): string {
  return JSON.stringify({
    v: PROTOCOL_VERSION,
    session_id: '01J8Z000000000000000000000',
    agent_id: 'default',
    ...fields,
  });
}

describe('parseServerFrame', () => {
  it('reads routing context and an assistant chunk', () => {
    const result = parseServerFrame(frame({ type: 'assistant_chunk', text: 'hello' }));

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.envelope.v).toBe(1);
    expect(result.envelope.session_id).toBe('01J8Z000000000000000000000');
    expect(result.envelope.agent_id).toBe('default');
    expect(result.envelope.type).toBe('assistant_chunk');
    if (result.envelope.type !== 'assistant_chunk') return;
    expect(result.envelope.text).toBe('hello');
  });

  it('keeps the sub-agent lane a frame arrived on', () => {
    const result = parseServerFrame(
      frame({ type: 'assistant_chunk', text: 'from the worker', subagent_id: 'code-reviewer' }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.envelope.subagent_id).toBe('code-reviewer');
  });

  it('keeps the sub-agent’s own session, which is what a lane is opened from', () => {
    const result = parseServerFrame(
      frame({
        type: 'assistant_chunk',
        text: 'from the worker',
        subagent_id: 'code-reviewer',
        subagent_session_id: 'sess_2',
      }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.envelope.subagent_session_id).toBe('sess_2');
  });

  it('leaves the sub-agent session off a frame that does not carry one', () => {
    const result = parseServerFrame(frame({ type: 'assistant_chunk', text: 'hi' }));

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.envelope.subagent_session_id).toBeUndefined();
  });

  it('keeps the frame when the sub-agent session is unreadable, since it is a hint', () => {
    const result = parseServerFrame(
      frame({ type: 'assistant_chunk', text: 'still here', subagent_session_id: 7 }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok || result.envelope.type !== 'assistant_chunk') return;
    expect(result.envelope.text).toBe('still here');
    expect(result.envelope.subagent_session_id).toBeUndefined();
  });

  it('reads a session_started frame whose routing keys were collapsed onto one value', () => {
    const result = parseServerFrame(
      frame({ type: 'session_started', model: 'mock-1' }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok || result.envelope.type !== 'session_started') throw new Error('wrong variant');
    expect(result.envelope.session_id).toBe('01J8Z000000000000000000000');
    expect(result.envelope.agent_id).toBe('default');
    expect(result.envelope.model).toBe('mock-1');
  });

  it('reads a complete tool card', () => {
    const result = parseServerFrame(
      frame({
        type: 'tool_call_end',
        tool_call_id: 'call_1',
        name: 'read_file',
        status: 'ok',
        output: 'contents',
        duration_ms: 12,
      }),
    );

    if (!result.ok || result.envelope.type !== 'tool_call_end') {
      throw new Error(`expected a tool_call_end frame, got ${JSON.stringify(result)}`);
    }
    expect(result.envelope.status).toBe('ok');
    expect(result.envelope.duration_ms).toBe(12);
  });

  it('reads token usage, including an exhausted budget', () => {
    const result = parseServerFrame(
      frame({
        type: 'token_usage',
        usage: { input_tokens: 120, output_tokens: 30 },
        budget_remaining: null,
      }),
    );

    if (!result.ok || result.envelope.type !== 'token_usage') throw new Error('wrong variant');
    expect(result.envelope.usage.input_tokens).toBe(120);
    expect(result.envelope.usage.output_tokens).toBe(30);
    expect(result.envelope.budget_remaining).toBeNull();
  });

  it('reads a queued message with the position the server gave it', () => {
    const result = parseServerFrame(
      frame({ type: 'message_queued', text: 'and another thing', position: 2 }),
    );

    if (!result.ok || result.envelope.type !== 'message_queued') throw new Error('wrong variant');
    expect(result.envelope.session_id).toBe('01J8Z000000000000000000000');
    expect(result.envelope.text).toBe('and another thing');
    expect(result.envelope.position).toBe(2);
  });

  it('reads a pong', () => {
    const result = parseServerFrame(frame({ type: 'pong' }));
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.envelope.type).toBe('pong');
  });

  it('reads free-form tool arguments as JSON', () => {
    const result = parseServerFrame(
      frame({
        type: 'tool_call_start',
        tool_call_id: 'call_1',
        name: 'shell',
        arguments: { command: 'ls', flags: ['-l'], depth: 2, dry: false },
      }),
    );

    if (!result.ok || result.envelope.type !== 'tool_call_start') throw new Error('wrong variant');
    expect(result.envelope.arguments).toEqual({ command: 'ls', flags: ['-l'], depth: 2, dry: false });
  });

  describe('rejections', () => {
    it('refuses a frame that is not JSON', () => {
      const result = parseServerFrame('{not json');
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toContain('not valid JSON');
    });

    it('refuses a JSON value that is not an object', () => {
      const result = parseServerFrame('[1,2,3]');
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toContain('must be a JSON object');
    });

    it('refuses an unsupported protocol version', () => {
      const result = parseServerFrame(
        JSON.stringify({
          v: 2,
          session_id: 's',
          agent_id: 'a',
          type: 'assistant_chunk',
          text: 'x',
        }),
      );

      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toContain('unsupported protocol version 2');
      expect(result.error).toContain('speaks 1');
    });

    it('refuses a frame with no version at all', () => {
      const result = parseServerFrame(
        JSON.stringify({ session_id: 's', agent_id: 'a', type: 'pong' }),
      );
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toContain('does not declare a protocol version');
    });

    it('refuses an unknown message type and lists the known ones', () => {
      const result = parseServerFrame(frame({ type: 'turn_started' }));
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toContain('unknown message type "turn_started"');
      expect(result.error).toContain('assistant_chunk');
    });

    it('refuses a frame with no routing session', () => {
      const result = parseServerFrame(
        JSON.stringify({ v: 1, agent_id: 'a', type: 'assistant_chunk', text: 'x' }),
      );
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toBe('assistant_chunk: `session_id` must be a string, got missing');
    });

    it('refuses a tool status outside the enum', () => {
      const result = parseServerFrame(
        frame({
          type: 'tool_call_end',
          tool_call_id: 'call_1',
          name: 'shell',
          status: 'finished',
          output: '',
          duration_ms: 1,
        }),
      );
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toBe(
        'tool_call_end: `status` must be one of ok, error, rejected, timeout; got the string "finished"',
      );
    });

    it('refuses a done frame with an unknown completion reason', () => {
      const result = parseServerFrame(frame({ type: 'done', reason: 'gave_up' }));
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toContain('`reason` must be one of end_turn');
    });

    it('refuses a queued message with no position', () => {
      const result = parseServerFrame(frame({ type: 'message_queued', text: 'x' }));
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toBe('message_queued: `position` must be a finite number, got missing');
    });

    it('refuses a chunk with a non-string body', () => {
      const result = parseServerFrame(frame({ type: 'assistant_chunk', text: 42 }));
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error).toBe('assistant_chunk: `text` must be a string, got number 42');
    });
  });
});

describe('client frames', () => {
  it('serializes a ping with only the version', () => {
    expect(serializeClientEnvelope(clientEnvelope({ type: 'ping' }))).toBe('{"v":1,"type":"ping"}');
  });

  it('flattens the message and writes priority on every steering frame', () => {
    const raw = serializeClientEnvelope(
      clientEnvelope(
        { type: 'steering_message', text: 'prefer the short path', priority: 'high' },
        { sessionId: 'sess_1' },
      ),
    );

    expect(JSON.parse(raw)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'steering_message',
      text: 'prefer the short path',
      priority: 'high',
    });
    expect(JSON.parse(raw)).not.toHaveProperty('message');
  });

  it('keeps the session id at the envelope level for a subscribe frame', () => {
    const raw = serializeClientEnvelope(
      clientEnvelope({ type: 'subscribe', session_id: 'sess_2' }),
    );

    expect(JSON.parse(raw)).toEqual({ v: 1, type: 'subscribe', session_id: 'sess_2' });
  });

  it('omits the agent override when none was chosen', () => {
    const raw = serializeClientEnvelope(clientEnvelope({ type: 'user_message', text: 'hi' }));

    expect(JSON.parse(raw)).toEqual({ v: 1, type: 'user_message', text: 'hi' });
    expect(raw).not.toContain('agent_id');
  });

  it('carries the chosen thinking level on the message it applies to', () => {
    const raw = serializeClientEnvelope(
      clientEnvelope(
        { type: 'user_message', text: 'prove the theorem', effort: 'max' },
        { sessionId: 'sess_1' },
      ),
    );

    expect(JSON.parse(raw)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'user_message',
      text: 'prove the theorem',
      effort: 'max',
    });
  });

  it('omits the thinking level when the caller names none, so the server default stands', () => {
    const raw = serializeClientEnvelope(clientEnvelope({ type: 'user_message', text: 'hi' }));

    expect(JSON.parse(raw)).toEqual({ v: 1, type: 'user_message', text: 'hi' });
    expect(raw).not.toContain('effort');
  });

  it('round-trips the chosen permission tier through the wire and back through JSON', () => {
    const raw = serializeClientEnvelope(
      clientEnvelope(
        { type: 'user_message', text: 'keep it safe', permission_mode: 'ask_when_needed' },
        { sessionId: 'sess_1' },
      ),
    );

    expect(JSON.parse(raw)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'user_message',
      text: 'keep it safe',
      permission_mode: 'ask_when_needed',
    });
  });

  it('omits the permission tier when the caller names none', () => {
    const raw = serializeClientEnvelope(clientEnvelope({ type: 'user_message', text: 'hi' }));

    expect(raw).not.toContain('permission_mode');
  });

  it('leaves the thinking level off a plan request, which carries no such field', () => {
    const raw = serializeClientEnvelope(
      clientEnvelope({ type: 'plan_task', task: 'split the refactor' }),
    );

    expect(raw).not.toContain('effort');
  });

  it('round-trips an abort through the wire and back through JSON', () => {
    const raw = serializeClientEnvelope(clientEnvelope({ type: 'abort', reason: 'user' }));

    expect(JSON.parse(raw)).toEqual({ v: 1, type: 'abort', reason: 'user' });
  });

  it('writes a plan request under the field name the server reads', () => {
    const raw = serializeClientEnvelope(
      clientEnvelope({ type: 'plan_task', task: 'split the refactor' }),
    );

    expect(JSON.parse(raw)).toEqual({ v: 1, type: 'plan_task', task: 'split the refactor' });
    expect(raw).not.toContain('"text"');
  });

  it('carries the session and the chosen agent on a plan request', () => {
    const raw = serializeClientEnvelope(
      clientEnvelope(
        { type: 'plan_task', task: 'split the refactor', agent_id: 'backend-architect' },
        { sessionId: 'sess_1' },
      ),
    );

    expect(JSON.parse(raw)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'plan_task',
      task: 'split the refactor',
      agent_id: 'backend-architect',
    });
  });

  it('omits the agent override when no agent was chosen for a plan request', () => {
    const raw = serializeClientEnvelope(clientEnvelope({ type: 'plan_task', task: 'split it' }));

    expect(raw).not.toContain('agent_id');
  });

  it('writes a workflow job under the fields the server reads', () => {
    const raw = serializeClientEnvelope(
      clientEnvelope({ type: 'queue_workflow', task: 'ship the endpoint', workflow: 'ship-it' }),
    );

    expect(JSON.parse(raw)).toEqual({
      v: 1,
      type: 'queue_workflow',
      task: 'ship the endpoint',
      workflow: 'ship-it',
    });
    // The task is the workflow's body field, not `text`, exactly as `plan_task`.
    expect(raw).not.toContain('"text"');
  });

  it('leaves the workflow off a job the server is asked to choose one for', () => {
    const raw = serializeClientEnvelope(clientEnvelope({ type: 'queue_workflow', task: 'ship it' }));

    const parsed = JSON.parse(raw);
    expect(parsed).toEqual({ v: 1, type: 'queue_workflow', task: 'ship it' });
    expect(parsed).not.toHaveProperty('workflow');
  });

  it('carries the session and the agent on a workflow job', () => {
    const raw = serializeClientEnvelope(
      clientEnvelope(
        {
          type: 'queue_workflow',
          task: 'ship it',
          workflow: 'ship-it',
          agent_id: 'backend-architect',
        },
        { sessionId: 'sess_1' },
      ),
    );

    expect(JSON.parse(raw)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'queue_workflow',
      task: 'ship it',
      workflow: 'ship-it',
      agent_id: 'backend-architect',
    });
  });
});

describe('the plan_created frame', () => {
  it('reads every node, with the fields a node may leave off', () => {
    const result = parseServerFrame(
      frame({
        type: 'plan_created',
        nodes: [
          { id: 'a', objective: 'sketch the API', agent: 'backend-architect', depends_on: [] },
          { id: 'b', objective: 'review it', depends_on: ['a'] },
        ],
        workflow_id: 'ship-it',
      }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok || result.envelope.type !== 'plan_created') return;
    expect(result.envelope.nodes).toEqual([
      { id: 'a', objective: 'sketch the API', agent: 'backend-architect', depends_on: [] },
      // `agent` and `depends_on` are `#[serde(default)]` on the Rust side, so an
      // omitted one is a node with none rather than a malformed frame.
      { id: 'b', objective: 'review it', agent: null, depends_on: ['a'] },
    ]);
    expect(result.envelope.workflow_id).toBe('ship-it');
  });

  it('leaves the workflow off a free-form plan', () => {
    const result = parseServerFrame(
      frame({ type: 'plan_created', nodes: [{ id: 'a', objective: 'do a' }] }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok || result.envelope.type !== 'plan_created') return;
    expect(result.envelope.workflow_id).toBeNull();
  });

  it('refuses a plan whose node has no objective', () => {
    const result = parseServerFrame(frame({ type: 'plan_created', nodes: [{ id: 'a' }] }));

    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error).toContain('`objective` must be a string');
  });

  it('refuses a plan whose nodes are not a list', () => {
    const result = parseServerFrame(frame({ type: 'plan_created', nodes: 'a' }));

    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error).toContain('`nodes` must be an array');
  });

  it('keeps the node id a subtask carries, which is what keys a plan row', () => {
    const result = parseServerFrame(
      frame({
        type: 'subtask_start',
        subtask_id: 'lane_1',
        node_id: 'review-diff',
        objective: 'review the diff',
      }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok || result.envelope.type !== 'subtask_start') return;
    // The lane id and the node id are different keys, and both are kept.
    expect(result.envelope.subtask_id).toBe('lane_1');
    expect(result.envelope.node_id).toBe('review-diff');
  });

  it('reads a subtask from a server that does not send the node id yet', () => {
    const result = parseServerFrame(
      frame({ type: 'subtask_start', subtask_id: 'lane_1', objective: 'review the diff' }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok || result.envelope.type !== 'subtask_start') return;
    expect(result.envelope.node_id).toBe('');
  });
});

describe('the workflow queue frames', () => {
  it('reads a queued job with its place in the workflow queue', () => {
    const result = parseServerFrame(
      frame({
        type: 'workflow_queued',
        job_id: 7,
        task: 'ship the endpoint',
        workflow_id: 'ship-it',
        position: 1,
      }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok || result.envelope.type !== 'workflow_queued') return;
    expect(result.envelope.job_id).toBe(7);
    expect(result.envelope.task).toBe('ship the endpoint');
    expect(result.envelope.workflow_id).toBe('ship-it');
    expect(result.envelope.position).toBe(1);
  });

  it('reads a started job, whose workflow may be the server’s own choice', () => {
    const result = parseServerFrame(
      frame({ type: 'workflow_started', job_id: 7, task: 'ship it' }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok || result.envelope.type !== 'workflow_started') return;
    expect(result.envelope.workflow_id).toBeNull();
  });

  it('reads a finished job, which names only the job and its outcome', () => {
    const result = parseServerFrame(
      frame({
        type: 'workflow_finished',
        job_id: 7,
        status: 'succeeded',
        summary: '3 of 3 node(s) succeeded',
      }),
    );

    expect(result.ok).toBe(true);
    if (!result.ok || result.envelope.type !== 'workflow_finished') return;
    expect(result.envelope.job_id).toBe(7);
    expect(result.envelope.status).toBe('succeeded');
    expect(result.envelope.summary).toBe('3 of 3 node(s) succeeded');
  });

  it('refuses a job state outside the enum', () => {
    const result = parseServerFrame(
      frame({ type: 'workflow_finished', job_id: 7, status: 'stalled', summary: '' }),
    );

    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error).toContain('`status` must be one of pending, running, succeeded, failed, skipped');
  });
});
