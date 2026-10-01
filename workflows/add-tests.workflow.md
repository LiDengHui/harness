---
id: add-tests
name: 补充测试
description: 为已有代码补上缺失的测试覆盖，不改动生产代码的行为。
when: 用户要求为已有代码补充测试覆盖或提高可信度；不改变生产代码行为，也不修缺陷。
stages:
  - id: survey
    objective: 读被测代码与旁边的测试，列出没有被覆盖的分支、边界和错误路径。
    agent: test-writer
  - id: write
    objective: 按仓库已有的测试结构与命名写测试，一条测试只验证一种行为。
    agent: test-writer
    depends_on:
      - survey
  - id: prove
    objective: 确认每条新测试都会因为唯一的失败原因变红，而不是恒真或重复已有覆盖。
    agent: test-writer
    depends_on:
      - write
    verify:
      - ["cargo", "test"]
---

补测试的前提是知道缺什么。`survey` 阶段要给出清单：哪些分支没有测试、哪些边界没人试过、哪些错误路径只被读过没被跑过。

一条测试只为一个原因失败。如果一个测试需要改三处代码才会变红，它就不能告诉你是哪里坏了。

`prove` 阶段逐条确认测试会失败：临时把被测行为改坏一次，确认对应的测试变红，再恢复。恒真的测试比没有测试更危险。
