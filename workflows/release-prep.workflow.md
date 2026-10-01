---
id: release-prep
name: 发布准备
description: 冻结状态、汇总变更、更新版本号与变更日志，跑通发布门禁。
when: 要把当前代码发布一个版本，需要整理变更日志、版本号和发布前检查；不是开发新功能。
stages:
  - id: freeze
    objective: 确认工作树干净、处在正确的分支与提交上，并记下本次发布的固定点。
    agent: default
  - id: changelog
    objective: 汇总自上一个发布以来的所有变更，按用户可感知的类型分类，剔除纯内部改动。
    agent: default
    depends_on:
      - freeze
  - id: bump
    objective: 按变更性质确定版本号并更新所有声明版本的位置，提交变更日志。
    agent: default
    depends_on:
      - changelog
  - id: gate
    objective: 跑发布门禁，确认测试、静态检查与发布构建在固定点上全部通过。
    agent: code-reviewer
    depends_on:
      - bump
    verify:
      - ["cargo", "test"]
      - ["cargo", "clippy", "--all-targets", "--", "-D", "warnings"]
      - ["cargo", "build", "--release"]
---

发布是从一个确定的固定点长出来的。`freeze` 阶段没确认干净之前，后面的变更日志和版本号都没有意义。

变更日志写给使用者，不是写给提交记录。按用户能感知的行为分类，纯内部重构不必逐条列出。

版本号跟着变更性质走：破坏性变更升主版本，新增功能升次版本，只有修复升补丁版本。不确定时按更高一级处理。
