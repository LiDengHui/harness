---
id: dependency-upgrade
name: 依赖升级
description: 升级一个具体依赖或工具链版本，先读变更说明，再改清单并修好编译与测试。
when: 用户要求升级某个具名依赖、工具链或基础镜像的版本；不是新增依赖，也不是排查依赖带来的缺陷。
stages:
  - id: survey
    objective: 读目标版本的变更说明与当前用法，列出破坏性变更和受影响的调用点。
    agent: default
  - id: upgrade
    objective: 改依赖清单与锁文件到目标版本，修好由破坏性变更引起的编译错误。
    agent: default
    depends_on:
      - survey
    verify:
      - ["cargo", "check"]
  - id: verify
    objective: 跑全量测试与静态检查，确认升级没有改变行为，并检查锁文件里没有意外的连带升级。
    agent: code-reviewer
    depends_on:
      - upgrade
    verify:
      - ["cargo", "test"]
      - ["cargo", "clippy", "--all-targets", "--", "-D", "warnings"]
---

升级的难点不在改版本号，而在变更说明里那几条破坏性变更。`survey` 阶段要把它们逐条落到本仓库的调用点上，而不是只列出来。

一次只升一个依赖。混着升会让 `verify` 阶段无法把失败归因到某个版本。

锁文件要看 diff。如果出现你没打算升的依赖，说明版本约束被牵动了，要显式说明或加约束把它固定回来。
