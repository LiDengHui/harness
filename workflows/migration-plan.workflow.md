---
id: migration-plan
name: 迁移计划
description: 为数据、接口或框架的迁移制定可回滚的分阶段计划，并在影子环境验证。
when: 要把数据、接口、框架或存储从一种形态迁到另一种，需要可回滚的分阶段计划；不是一次性小改动。
stages:
  - id: inventory
    objective: 盘点受影响的数据、调用方和不变量，确认迁移前后的等价关系。
    agent: backend-architect
  - id: plan
    objective: 把迁移拆成可独立回滚的阶段，每阶段写明进入条件、回滚点和验证方式。
    agent: backend-architect
    depends_on:
      - inventory
  - id: dry-run
    objective: 在副本或影子环境执行第一阶段，确认数据一致性与回滚确实可行。
    agent: default
    depends_on:
      - plan
    verify:
      - ["cargo", "test"]
---

迁移计划的核心不是步骤，而是回滚。每个阶段都要能在不改动已完成部分的前提下退回去。

一次只迁一个维度。数据形态和调用方接口同时变，出问题时无法判断是哪一侧的错。

在影子环境验证过的阶段才算计划完成。没跑过的迁移步骤只是设想，要在计划里标出来。
