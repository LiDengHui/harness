---
name: api-contract
description: Write interface contracts another agent can implement against — exact types, exact errors, exact invariants — without ambiguity a reader has to resolve.
---

A contract is finished when an implementer can satisfy it without deciding
anything you left out. Ambiguity in a contract becomes a disagreement in code.

Be exact:

- Give every parameter and return value a concrete type. `Value`, `impl
  Serialize` and "a map" are not types; if the payload is dynamic, say which
  keys are required and what each one means.
- Name the unit. Seconds or milliseconds, bytes or bytes-per-second, zero-based
  or one-based offsets. Get this wrong once and every call site is wrong.
- Enumerate the errors. Each one gets a trigger condition and what the caller
  should do about it. "Returns an error on failure" says nothing.
- State the invariants the implementation must preserve and the preconditions
  the caller must satisfy. Mark each as checked or unchecked.
- Write down the ordering guarantee explicitly, including "none".

Design for the caller:

- Prefer one obvious way to call it over a flag that changes the behaviour.
- Defaults are part of the contract; if a field is optional, say what happens
  when it is absent.
- Changes that break an existing caller need a migration note in the same
  document: which call sites change and how.
- Keep the contract and the code in the same change. A contract described only
  in prose drifts within a week.

Example, not a description:

```
fn recall(&self, query: RecallQuery) -> Result<Vec<RecallHit>>
// Errors, both `HarnessError::Memory`:
//   - `query.session` does not exist
//   - the stored node body is not valid UTF-8
// Unchecked: `query.limit` above 1000 is truncated, not rejected.
// Ordering: hits are sorted by descending score, ties broken by node id.
```
