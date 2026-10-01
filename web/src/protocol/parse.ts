/**
 * Turns a raw socket frame into a typed envelope, or into a sentence explaining
 * why it could not be read.
 *
 * The parser is deliberately strict. A front end that silently drops a frame it
 * did not understand shows a transcript with a hole in it and no way to tell;
 * one that reports the frame keeps the failure visible. Version mismatches get
 * the same treatment: a server that speaks a newer protocol is a thing the user
 * has to know about, not something to paper over.
 *
 * Nothing here throws: every failure is a `ParseResult` error string.
 *
 * One field is the exception to the strictness, and it is called out where it is
 * read: `subagent_session_id` is a routing hint rather than message content, so a
 * value this build cannot read is ignored instead of failing the frame.
 */

import {
  COMPLETION_REASONS,
  PROTOCOL_VERSION,
  SERVER_MESSAGE_TYPES,
  SUBTASK_STATUSES,
  TOOL_CALL_STATUSES,
  type JsonValue,
  type PlanNode,
  type ServerEnvelope,
  type ServerEnvelopeRouting,
  type ServerMessageType,
} from './types';

export type ParseResult =
  | { ok: true; envelope: ServerEnvelope }
  | { ok: false; error: string };

/** Raised by the field readers below and caught by `parseServerFrame`. */
class FrameError extends Error {}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isServerMessageType(value: string): value is ServerMessageType {
  return (SERVER_MESSAGE_TYPES as readonly string[]).includes(value);
}

/** A short, safe rendering of a bad value for an error message. */
function describe(value: unknown): string {
  if (value === undefined) return 'missing';
  if (value === null) return 'null';
  if (Array.isArray(value)) return 'an array';
  switch (typeof value) {
    case 'string': {
      const preview = value.length > 40 ? `${value.slice(0, 40)}…` : value;
      return `the string ${JSON.stringify(preview)}`;
    }
    case 'number':
    case 'boolean':
    case 'bigint':
      return `${typeof value} ${String(value)}`;
    case 'object':
      return 'an object';
    default:
      return typeof value;
  }
}

function requireString(record: Record<string, unknown>, key: string): string {
  const value = record[key];
  if (typeof value !== 'string') {
    throw new FrameError(`\`${key}\` must be a string, got ${describe(value)}`);
  }
  return value;
}

function optionalString(record: Record<string, unknown>, key: string): string | null {
  const value = record[key];
  if (value === undefined || value === null) return null;
  if (typeof value !== 'string') {
    throw new FrameError(`\`${key}\` must be a string when present, got ${describe(value)}`);
  }
  return value;
}

/**
 * An optional routing hint, read leniently.
 *
 * Every other field on a frame is read strictly, because a frame this build
 * cannot fully understand is a frame it must not pretend to have understood.
 * A routing hint is the exception: `subagent_session_id` says where a frame came
 * from, not what it carries, so a value this build cannot read costs the
 * transcript nothing — while refusing the frame would drop a real chunk of the
 * run's output over a field that only decides whether one lane is clickable.
 * The hint is therefore ignored rather than raised on; the message's own fields
 * are still read strictly.
 */
function routingHint(record: Record<string, unknown>, key: string): string | null {
  const value = record[key];
  return typeof value === 'string' && value !== '' ? value : null;
}

function requireNumber(record: Record<string, unknown>, key: string): number {
  const value = record[key];
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new FrameError(`\`${key}\` must be a finite number, got ${describe(value)}`);
  }
  return value;
}

function optionalNumber(record: Record<string, unknown>, key: string): number | null {
  const value = record[key];
  if (value === undefined || value === null) return null;
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new FrameError(`\`${key}\` must be a finite number when present, got ${describe(value)}`);
  }
  return value;
}

function requireBoolean(record: Record<string, unknown>, key: string): boolean {
  const value = record[key];
  if (typeof value !== 'boolean') {
    throw new FrameError(`\`${key}\` must be a boolean, got ${describe(value)}`);
  }
  return value;
}

function requireObject(record: Record<string, unknown>, key: string): Record<string, unknown> {
  const value = record[key];
  if (!isRecord(value)) {
    throw new FrameError(`\`${key}\` must be an object, got ${describe(value)}`);
  }
  return value;
}

function requireOneOf<T extends string>(
  record: Record<string, unknown>,
  key: string,
  allowed: readonly T[],
): T {
  const value = record[key];
  if (typeof value !== 'string' || !(allowed as readonly string[]).includes(value)) {
    throw new FrameError(
      `\`${key}\` must be one of ${allowed.join(', ')}; got ${describe(value)}`,
    );
  }
  return value as T;
}

function isJsonValue(value: unknown): value is JsonValue {
  if (value === null) return true;
  switch (typeof value) {
    case 'string':
    case 'number':
    case 'boolean':
      return true;
    case 'object':
      if (Array.isArray(value)) return value.every(isJsonValue);
      return Object.values(value as Record<string, unknown>).every(isJsonValue);
    default:
      return false;
  }
}

function requireJsonValue(record: Record<string, unknown>, key: string): JsonValue {
  const value = record[key];
  if (!isJsonValue(value)) {
    throw new FrameError(`\`${key}\` must be a JSON value, got ${describe(value)}`);
  }
  return value;
}

/**
 * An optional array of strings, defaulting to empty when it is absent.
 *
 * `depends_on` is `#[serde(default)]` on `TaskNode`, so an omitted field is a
 * node with no dependencies rather than a malformed one; a present value of the
 * wrong shape is still refused, because that is a frame this build cannot read.
 */
function optionalStringArray(record: Record<string, unknown>, key: string): string[] {
  const value = record[key];
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value) || !value.every((item) => typeof item === 'string')) {
    throw new FrameError(`\`${key}\` must be an array of strings when present, got ${describe(value)}`);
  }
  return value;
}

/** The plan's nodes, each read field-for-field from the frame. */
function requirePlanNodes(record: Record<string, unknown>, key: string): PlanNode[] {
  const value = record[key];
  if (!Array.isArray(value)) {
    throw new FrameError(`\`${key}\` must be an array, got ${describe(value)}`);
  }
  return value.map((element, index) => {
    if (!isRecord(element)) {
      throw new FrameError(`\`${key}[${index}]\` must be an object, got ${describe(element)}`);
    }
    return {
      id: requireString(element, 'id'),
      objective: requireString(element, 'objective'),
      agent: optionalString(element, 'agent'),
      depends_on: optionalStringArray(element, 'depends_on'),
    };
  });
}

/**
 * Reads one message off a frame whose routing fields are already validated.
 *
 * Each branch returns the envelope rather than the message alone, because the
 * flattened wire shape is exactly `routing & message` — building it here keeps
 * the intersection honest instead of asserting it back together afterwards.
 */
function readMessage(
  routing: ServerEnvelopeRouting,
  type: ServerMessageType,
  record: Record<string, unknown>,
): ServerEnvelope {
  switch (type) {
    case 'session_started':
      // `session_id` and `agent_id` live on the envelope too; serde's encoder
      // collapses the duplicate keys onto one value, so the routing fields are
      // the authoritative copy.
      return { ...routing, type, model: requireString(record, 'model') };

    case 'assistant_chunk':
      return { ...routing, type, text: requireString(record, 'text') };

    case 'thinking_chunk':
      return { ...routing, type, text: requireString(record, 'text') };

    case 'tool_call_start':
      return {
        ...routing,
        type,
        tool_call_id: requireString(record, 'tool_call_id'),
        name: requireString(record, 'name'),
        arguments: requireJsonValue(record, 'arguments'),
      };

    case 'tool_call_progress':
      return {
        ...routing,
        type,
        tool_call_id: requireString(record, 'tool_call_id'),
        message: requireString(record, 'message'),
      };

    case 'tool_call_end':
      return {
        ...routing,
        type,
        tool_call_id: requireString(record, 'tool_call_id'),
        name: requireString(record, 'name'),
        status: requireOneOf(record, 'status', TOOL_CALL_STATUSES),
        output: requireString(record, 'output'),
        duration_ms: requireNumber(record, 'duration_ms'),
      };

    case 'subtask_start':
      return {
        ...routing,
        type,
        subtask_id: requireString(record, 'subtask_id'),
        // `#[serde(default)]` on the Rust side: an older server that does not
        // send it yet leaves the node unnamed rather than failing the frame.
        node_id: optionalString(record, 'node_id') ?? '',
        objective: requireString(record, 'objective'),
      };

    case 'subtask_end':
      return {
        ...routing,
        type,
        subtask_id: requireString(record, 'subtask_id'),
        node_id: optionalString(record, 'node_id') ?? '',
        status: requireOneOf(record, 'status', SUBTASK_STATUSES),
        summary: requireString(record, 'summary'),
      };

    case 'plan_created':
      return {
        ...routing,
        type,
        nodes: requirePlanNodes(record, 'nodes'),
        workflow_id: optionalString(record, 'workflow_id'),
      };

    case 'workflow_queued':
      return {
        ...routing,
        type,
        job_id: requireNumber(record, 'job_id'),
        task: requireString(record, 'task'),
        workflow_id: optionalString(record, 'workflow_id'),
        position: requireNumber(record, 'position'),
      };

    case 'workflow_started':
      return {
        ...routing,
        type,
        job_id: requireNumber(record, 'job_id'),
        task: requireString(record, 'task'),
        workflow_id: optionalString(record, 'workflow_id'),
      };

    case 'workflow_finished':
      return {
        ...routing,
        type,
        job_id: requireNumber(record, 'job_id'),
        status: requireOneOf(record, 'status', SUBTASK_STATUSES),
        summary: requireString(record, 'summary'),
      };

    case 'agent_handoff':
      return {
        ...routing,
        type,
        from: requireString(record, 'from'),
        to: requireString(record, 'to'),
        reason: requireString(record, 'reason'),
      };

    case 'token_usage': {
      const usage = requireObject(record, 'usage');
      return {
        ...routing,
        type,
        usage: {
          input_tokens: requireNumber(usage, 'input_tokens'),
          output_tokens: requireNumber(usage, 'output_tokens'),
        },
        budget_remaining: optionalNumber(record, 'budget_remaining'),
      };
    }

    case 'guardrail':
      return {
        ...routing,
        type,
        name: requireString(record, 'name'),
        blocked: requireBoolean(record, 'blocked'),
        detail: requireString(record, 'detail'),
      };

    case 'tool_approval_request':
      return {
        ...routing,
        type,
        tool_call_id: requireString(record, 'tool_call_id'),
        name: requireString(record, 'name'),
        arguments: requireJsonValue(record, 'arguments'),
        reason: requireString(record, 'reason'),
      };

    case 'message_queued':
      return {
        ...routing,
        type,
        text: requireString(record, 'text'),
        position: requireNumber(record, 'position'),
      };

    case 'error':
      return {
        ...routing,
        type,
        code: requireString(record, 'code'),
        message: requireString(record, 'message'),
      };

    case 'done':
      return { ...routing, type, reason: requireOneOf(record, 'reason', COMPLETION_REASONS) };

    case 'pong':
      return { ...routing, type };
  }
}

/**
 * Parses one text frame from the bus.
 *
 * `raw` is the frame body as a string; the caller is responsible for refusing
 * binary frames (the protocol is text-only).
 */
export function parseServerFrame(raw: string): ParseResult {
  let decoded: unknown;
  try {
    decoded = JSON.parse(raw);
  } catch (err) {
    return { ok: false, error: `frame is not valid JSON: ${errorMessage(err)}` };
  }

  if (!isRecord(decoded)) {
    return { ok: false, error: `frame must be a JSON object, got ${describe(decoded)}` };
  }

  const version = decoded['v'];
  if (typeof version !== 'number') {
    return {
      ok: false,
      error: `frame does not declare a protocol version (\`v\` is ${describe(version)}; this client speaks ${PROTOCOL_VERSION})`,
    };
  }
  if (version !== PROTOCOL_VERSION) {
    return {
      ok: false,
      error: `unsupported protocol version ${version}; this client speaks ${PROTOCOL_VERSION}`,
    };
  }

  const type = decoded['type'];
  if (typeof type !== 'string') {
    return { ok: false, error: `frame does not declare a message type (\`type\` is ${describe(type)})` };
  }
  if (!isServerMessageType(type)) {
    return {
      ok: false,
      error: `unknown message type ${JSON.stringify(type)}; known types are ${SERVER_MESSAGE_TYPES.join(', ')}`,
    };
  }

  try {
    const session_id = requireString(decoded, 'session_id');
    const agent_id = requireString(decoded, 'agent_id');
    const subagent_id = optionalString(decoded, 'subagent_id');
    const routing: ServerEnvelopeRouting = { v: version, session_id, agent_id };
    if (subagent_id !== null) {
      routing.subagent_id = subagent_id;
    }
    // A hint, not content: an unreadable one leaves the frame intact.
    const subagent_session_id = routingHint(decoded, 'subagent_session_id');
    if (subagent_session_id !== null) {
      routing.subagent_session_id = subagent_session_id;
    }
    return { ok: true, envelope: readMessage(routing, type, decoded) };
  } catch (err) {
    if (err instanceof FrameError) {
      return { ok: false, error: `${type}: ${err.message}` };
    }
    throw err;
  }
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
