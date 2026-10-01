---
id: api-contract-design
name: 接口契约设计
description: 在实现之前把对外接口、协议与数据模型定死，产出可被直接实现的契约。
when: 需要在动手实现之前先确定对外接口、协议或数据模型；产出契约而不是实现代码。
stages:
  - id: requirements
    objective: 收集调用方与使用场景，明确契约要满足的需求和必须兼容的既有调用方。
    agent: backend-architect
  - id: contract
    objective: 写出精确的接口定义，包括类型、单位、错误条件、不变量和顺序保证。
    agent: backend-architect
    depends_on:
      - requirements
  - id: review
    objective: 以实现者视角审查契约，找出仍然需要读者自己决定的歧义与兼容性缺口。
    agent: code-reviewer
    depends_on:
      - contract
---

契约完成的标志是：实现者照着它做，不需要再自行决定任何事。契约里的歧义最终会变成代码里的分歧。

每个参数和返回值都要有具体类型。动态载荷要写清楚哪些键是必需的、每个键是什么意思。单位必须写死，秒还是毫秒、字节还是字节每秒。

错误要逐个列举触发条件与调用方该做什么。会破坏既有调用方的改动，必须在同一份契约里给出迁移说明。
