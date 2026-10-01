---
id: perf-investigate
name: 性能调查
description: 先建立可重复的基准并测量，再定位热点、优化、复测，拒绝凭感觉优化。
when: 有性能退化或变慢的迹象，要求先测量定位热点再优化；不是功能开发，也不是猜哪里慢就改哪里。
stages:
  - id: measure
    objective: 建立可重复的基准，记录改动前的数字与测量条件，明确测的是哪个指标。
    agent: default
  - id: profile
    objective: 在基准下定位热点，用采样或计时数据给出证据，区分真正的瓶颈与顺带的开销。
    agent: default
    depends_on:
      - measure
  - id: optimize
    objective: 只针对已验证的热点做最小优化，不改外部行为，不为没测过的路径提前优化。
    agent: default
    depends_on:
      - profile
    verify:
      - ["cargo", "test"]
  - id: remeasure
    objective: 用同一个基准、同一组条件复测，给出前后数字对比，并说明噪声范围。
    agent: code-reviewer
    depends_on:
      - optimize
---

先测量，再优化。没有基线数字的优化无法判断是否真的变快了。

基准必须可重复：同样的输入、同样的构建模式、同样的机器状态。跑一次就下结论的测量是噪声。

优化只针对 `profile` 阶段证实的热点。如果复测数字没有明显变化，如实说明，不要把噪声当成收益。
