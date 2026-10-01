---
id: feature-implement
name: 功能实现
description: 把一个需求已经明确的新功能，从接口契约一路做到实现、测试与评审。
when: 用户要求新增当前仓库里还不存在的功能、命令、接口或模块，且需求已经明确；不是修缺陷，不是重构，也不是先探索代码。
stages:
  - id: design
    objective: 读现有代码与调用方，确定新功能的接口边界、数据流和错误路径，写出实现者不必再做决定就能照做的契约。
    agent: backend-architect
  - id: implement
    objective: 严格按契约做最小实现，不改动无关代码，不顺手重构，不做契约之外的扩展。
    agent: default
    depends_on:
      - design
    verify:
      - ["cargo", "check"]
  - id: test
    objective: 为新行为补测试，覆盖正常路径、边界和错误路径，每条测试只为一种失败原因而失败。
    agent: test-writer
    depends_on:
      - implement
    verify:
      - ["cargo", "test"]
  - id: review
    objective: 对照契约与 diff 逐条检查正确性、回归风险和缺失的测试，给出带文件与行的发现。
    agent: code-reviewer
    depends_on:
      - test
    verify:
      - ["cargo", "clippy", "--all-targets", "--", "-D", "warnings"]
---

新功能最大的浪费不是写错，而是写了一个没人需要的接口。所以先定契约，再写代码。

契约在 `design` 阶段就要固定下来：每个参数和返回值的具体类型、每个错误在什么条件下触发、不变量由谁保证、顺序是否有序。实现阶段不允许再改契约；如果实现中发现契约不可行，退回 `design` 改契约，而不是在实现里偷偷绕开。

实现只做契约要求的事。看到相邻代码有问题可以记下来，但不要在本工作流里顺手修，那会让这次改动的评审面失控。

测试必须能因为实现被改坏而失败。只断言真实输出，不要断言一个代理指标。
