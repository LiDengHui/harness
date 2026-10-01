---
id: repo-onboard
name: 仓库上手
description: 快速摸清一个陌生仓库的结构、构建方式与关键执行路径，产出可用的上手说明。
when: 用户刚接触一个仓库，要求理解它的结构、怎么构建、主流程在哪；产出上手说明，不是改代码。
stages:
  - id: inventory
    objective: 读构建清单、说明文档与目录结构，确定构建、测试和运行的准确命令。
    agent: default
  - id: trace
    objective: 从入口出发追一条主执行路径，标出关键模块与它们之间的依赖方向。
    agent: default
    depends_on:
      - inventory
    verify:
      - ["cargo", "build"]
  - id: brief
    objective: 写一份上手说明，包含构建与测试命令、目录职责、主流程和最容易踩的坑。
    agent: default
    depends_on:
      - trace
---

上手说明的价值在于命令能跑通。`inventory` 阶段给出的构建与测试命令必须在 `trace` 阶段实际执行过。

先走一条主路径，再看全貌。把每个模块都读一遍既慢又记不住，追一条真实执行路径反而能建立结构感。

写清楚哪里最容易踩坑：隐式的环境依赖、需要先跑的生成步骤、只能按特定顺序执行的命令。
