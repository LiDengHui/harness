---
id: docs-write
name: 编写文档
description: 补 README、模块文档或使用说明，并逐条验证文档里的示例确实能跑。
when: 用户要求补写或整理文档、说明与示例；不涉及代码行为变更。
stages:
  - id: inventory
    objective: 读代码与现有文档，列出读者真正需要知道的内容，并标出哪些说法会随代码变化。
    agent: default
  - id: draft
    objective: 按读者顺序写文档，每条命令和示例都可执行、可复现。
    agent: default
    depends_on:
      - inventory
  - id: verify-examples
    objective: 逐条执行文档中的命令与示例，跑不通的要么改文档，要么标注前置条件。
    agent: default
    depends_on:
      - draft
    verify:
      - ["cargo", "doc", "--no-deps"]
---

文档的目标是让读者不必读代码就能用对。所以先想清楚读者是谁、他要完成什么，再决定写什么。

示例必须被执行过。一条跑不通的命令比没有文档更糟，因为它会让读者怀疑自己。

不要复述代码已经说清楚的事。文档写的是为什么这样用、什么时候不该用、以及出错了怎么办。
