---
id: refactor
name: 重构
description: 在外部行为不变的前提下改善结构，用测试作为行为基线。
when: 用户要求在不改变对外行为的前提下改善结构、命名或消除重复，且已有测试可作为安全网。
stages:
  - id: baseline
    objective: 记录当前测试与关键输出作为行为基线，确认改动前是绿的。
    agent: default
    verify:
      - ["cargo", "test"]
  - id: refactor
    objective: 小步重构，每一步之后都能编译并保持行为不变，不顺手改需求或修缺陷。
    agent: default
    depends_on:
      - baseline
    verify:
      - ["cargo", "check"]
  - id: confirm
    objective: 用同一组测试与同一批关键输入复测，证明外部行为与基线一致。
    agent: code-reviewer
    depends_on:
      - refactor
    verify:
      - ["cargo", "test"]
---

重构的验收标准只有一个：对外行为没变。所以先有基线，才有重构。

一次只做一件事。改名、提取函数、移动模块分开放；混在一起会让 `confirm` 阶段无法判断行为变化来自哪一步。

如果重构过程中发现缺少测试覆盖，不要就地补一大片测试，那会把这次改动变成两件事。记下来，另开 add-tests。
