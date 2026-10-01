/**
 * `common` 命名空间：不属于任何单个页面的通用部分 —— 应用标题、导航、通用按钮，
 * 以及语言切换器自己的标签。
 *
 * 这里放的是每个页面都会用到的键，因此集中在一个文件里，而不是各页面各写一份。
 * `actions` 只保留各页面真正共用的按钮：输入区自己的 `send` 与 `stop` 留在输入区，
 * 它们属于输入区的模式词汇，而不是通用外壳。
 *
 * `rail`、`session`、`time`、`composer`、`queue`、`workflow`、`workflowQueue`、`replay`、`effort` 九组是外壳里的“非内容”词汇：分组标题、
 * 连接自身的状态、相对时间、运行读数描述的是会话列表与运行本身，而不是某一条消息帧；`queue` 说的是
 * 已经受理、还没开始运行的那些消息；`workflow` 说的是给这条任务挑哪个工作流；`workflowQueue` 说的是
 * 已经受理、还没开始运行的那些工作流任务 —— 它和 `queue` 分开，正因为用户必须能分辨“一条消息在等”
 * 和“一个工作流在等”；`replay` 说的是“正在读一条过去会话”这件事本身 —— 正在读、读不到，
 * 都不是会话内容。`effort` 说的是这条消息要模型想多深，是输入区的一个选项，而不是模型说了什么。
 * `session` 的几句话说的都是**这条连接**的会话：左栏存着几十条会话，也不等于这条连接有会话；
 * 最后一句说明重新打开页面却恢复不了原会话时意味着什么。
 */

export default {
  common: {
    appTitle: 'harness',
    nav: {
      chat: '对话',
      agents: '智能体',
      extensions: '扩展',
      ariaLabel: '主导航',
    },
    actions: {
      retry: '重试',
      close: '关闭',
    },
    language: {
      switch: '切换到{language}',
    },
    rail: {
      search: '搜索会话',
      noMatch: '没有匹配的会话。',
      showMore: '展开更多',
      showLess: '收起',
      agent: '智能体 {agent}',
      untitled: '还没有消息的会话',
      forkedFrom: '分叉自 {title}',
      running: '运行中',
      runningTitle: '这条会话有一条正在进行的运行',
      queued: '{count} 条排队',
      queuedTitle: '这条会话有 {count} 条消息在排队等待运行',
      subagentTag: '子智能体',
      subagentCount: '子智能体会话：{count} 条',
      subagentCountTitle:
        '这条会话派给子智能体的工作。每一次子智能体运行都单独存为一条会话；展开可以看到它们，点进去就能读到子智能体实际做了什么。',
      subagentOpenTitle: '展开这条会话下的子智能体会话',
      subagentCloseTitle: '收起这条会话下的子智能体会话',
      statsHint: '会话存储的合计：会话数、节点数、token 估算与二进制块数',
      group: {
        serve: '网页会话',
        run: '命令行',
      },
      groupHint: {
        serve: '按创建方式分组，与项目无关：这些会话来自网页端（harness serve 的浏览器界面）',
        run: '按创建方式分组，与项目无关：这些会话来自命令行（harness run）',
      },
    },
    session: {
      noSessionHere: '这条连接还没有会话 —— 发一条消息就会创建。',
      notAttached: '这条连接还没有会话',
      restoreMissing: '无法恢复本页原先所在的会话：服务端上已经没有它了。发一条消息即可开始新会话。',
    },
    time: {
      justNow: '刚刚',
      minutes: '{count} 分钟',
      hours: '{count} 小时',
      days: '{count} 天',
    },
    composer: {
      turns: '轮次 {count}',
      modelHint: '本会话解析到的模型（来自 session_started 帧）',
    },
    effort: {
      label: '思考等级',
      low: '快速',
      high: '均衡',
      max: '深入',
      hint: {
        low: '回答更快、更省 token：适合简单直接的问题',
        high: '默认强度：正常思考，兼顾速度与深度',
        max: '思考最久、最深入：适合最难的问题，也更慢更费',
      },
    },
    queue: {
      heading: '排队等待（{count}）',
      next: '下一条运行',
      ahead: '前面还有 {count} 条',
    },
    workflow: {
      pickTitle: '为这条任务挑一个工作流；留空则由服务端自动选择',
    },
    workflowQueue: {
      heading: '工作流队列（{count}）',
      tag: '工作流',
      queued: '排队中',
      running: '进行中',
      finished: '已完成',
      failed: '失败',
      skipped: '已跳过',
      openPlan: '查看计划',
      openPlanTitle: '打开这个工作流任务产生的计划',
    },
    replay: {
      badge: '回放',
      loading: '正在读取会话历史…',
      unavailable: '暂时读不到这条会话的历史：服务端还没有提供该接口，或这个会话已经不存在。',
    },
  },
};
