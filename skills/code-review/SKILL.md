---
name: code-review
description: Review a change against its fixed point — correctness, regressions and missing tests — and report only what the evidence supports.
---

Review the change, not the repository. Establish the fixed point first (the base
commit, or the diff you were handed), then read what the change touches plus the
callers that depend on it. Nothing else.

For every finding, give:

- the file and line,
- what breaks, in one sentence a reader can act on,
- the smallest fix,
- and whether you verified it by reading or running, or merely suspect it.

A test you did not run is a suspicion. Say so instead of presenting it as a
finding.

Check in this order, because regressions hide in the later ones:

1. Does the code do what the change claims? Compare against the commit message
   or the issue, not against what you would have written.
2. Are the error paths right? A new failure that is silently swallowed is worse
   than the one it replaced.
3. Did anything else depend on the old behaviour? Search for the callers.
4. Is the new behaviour tested by a test that would fail if the change were
   reverted?
5. Are paths, units, ordering and lifetimes consistent with what the rest of the
   codebase assumes?

Out of scope: formatting, naming preferences, and anything the project's own
tooling enforces. If the change is sound, say so in one line and stop. Never
pad a review, and never restate the diff back to its author.
