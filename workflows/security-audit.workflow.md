---
id: security-audit
name: 安全审计
description: 划定信任边界，审计代码与依赖中的安全问题，按严重度给出发现而非直接修复。
when: 用户要求审计代码或依赖中的安全问题，例如注入、越权、密钥泄露、不安全的反序列化；产出发现与建议，不是直接改代码。
stages:
  - id: scope
    objective: 划定系统的信任边界与攻击面，标出外部输入进入系统的位置。
    agent: backend-architect
  - id: scan
    objective: 逐个攻击面检查输入校验、权限判断、密钥与依赖漏洞，记录可复现的证据。
    agent: code-reviewer
    depends_on:
      - scope
    verify:
      - ["cargo", "audit"]
  - id: report
    objective: 按严重度整理发现，每条给出触发条件、影响、证据和最小修复方向。
    agent: code-reviewer
    depends_on:
      - scan
---

审计的产出是发现，不是补丁。在本工作流里不要顺手改代码，那会让发现和修复混在一起，也会让审计者失去独立性。

先划边界，再找问题。没有攻击面的清单，检查就会变成漫无目的地读代码。

每条发现都要能说清楚：谁能触发、需要什么前提、最坏后果是什么、证据在哪个文件哪一行。无法复现的怀疑要标成怀疑。
