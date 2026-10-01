---
id: bug-fix
name: 缺陷修复
description: 先复现、再定位根因、最后用回归测试锁住，把一个可复现的缺陷改对。
when: 有可复现的缺陷、失败测试或明确报错信息，目标是把现有行为改对；不是加新功能，也不是改善结构。
stages:
  - id: reproduce
    objective: 写出能稳定触发缺陷的最小复现，并记录真实的失败输出，确认它确实失败。
    agent: default
  - id: diagnose
    objective: 顺着复现找到根因，用代码或运行输出给出证据，区分已验证的事实与推测。
    agent: default
    depends_on:
      - reproduce
  - id: fix
    objective: 针对根因做最小修复，不动无关代码，也不掩盖症状。
    agent: default
    depends_on:
      - diagnose
    verify:
      - ["cargo", "test"]
  - id: regress
    objective: 加一条会在修复前失败、修复后通过的回归测试，防止同一缺陷再次出现。
    agent: test-writer
    depends_on:
      - fix
    verify:
      - ["cargo", "test"]
---

没有复现就没有修复。`reproduce` 阶段必须先看到失败，再动任何代码；否则你只是在猜。

根因和症状不是一回事。如果修复只是让报错消失，例如吞掉异常、放宽断言或加一个特判，那多半改错了地方，回到 `diagnose` 继续找。

回归测试的价值在于它曾经失败过。写完 `regress` 后，把修复临时回退一次，确认新测试确实变红，再恢复修复。
