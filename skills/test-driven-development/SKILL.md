---
name: test-driven-development
description: Write tests that fail for exactly one reason, run them against the unfixed code, and never widen a test to hide a failure.
---

A test earns its place by failing when the behaviour it names is broken. If you
cannot describe the change that would break it, delete it.

Write in this order:

1. Name the behaviour as a sentence: "a missing directory is not an error".
   That sentence is the test name.
2. Write the assertion against the real output, not a proxy for it. Counting
   calls is not checking behaviour; asserting on a helper's return value is not
   checking the code path the caller takes.
3. Run it and watch it fail, for the reason you claim. A test that passes before
   the fix tests nothing.
4. Then make it pass with the smallest change to the production code.
5. Run the whole suite. A green new test beside a red old one is not progress.

Rules:

- One behaviour per test. If a test has two `assert` phases that fail
  independently, it is two tests.
- No ignored, skipped or `#[should_panic]`-as-a-catch-all tests. Fix or delete.
- Never loosen an assertion to make it pass, and never widen a test's scope to
  absorb a failure someone else introduced.
- Match the structure, naming and helper style of the tests already beside the
  file — a new convention in an established suite costs more than it saves.
- Test edge cases that the implementation had to think about: empty input, the
  boundary value, the second call, the error path.
