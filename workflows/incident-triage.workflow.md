---
id: incident-triage
name: 事故分诊
description: 运行中的故障先止损，再定位根因，最后写复盘并补上回归修复。
when: 线上或运行中的系统已经出故障，需要先止损再定位；不是提前预防，也不是常规开发。
stages:
  - id: stabilize
    objective: 先止损，用回滚、关闭开关或限流让影响面停止扩大，并记录做了什么。
    agent: default
  - id: diagnose
    objective: 用日志、指标和复现定位根因，给出时间线与证据，区分相关性和因果。
    agent: default
    depends_on:
      - stabilize
  - id: postmortem
    objective: 写出时间线、影响面、根因和跟进项，跟进项要能被单独执行和验证。
    agent: default
    depends_on:
      - diagnose
  - id: followup-fix
    objective: 修复根因并补一条回归测试，确认同一故障不会再以相同方式发生。
    agent: test-writer
    depends_on:
      - postmortem
    verify:
      - ["cargo", "test"]
---

止损优先于根因。系统还在恶化时，先把影响面按住，再开始分析。

定位阶段要区分相关性和因果：某个指标和故障同时出现，不等于它是原因。用时间线和可复现的证据说话。

复盘对事不对人。跟进项要具体到能单独开一个任务执行，否则复盘就只是记录。
