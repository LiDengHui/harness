/**
 * `chat` 命名空间：聊天界面 —— 会话栏、transcript、输入区、诊断条和连接徽标。
 *
 * 线上枚举（`toolStatus.*`、`error.codes.*`、`sessionStatus.*`）直接以枚举值本身
 * 作为键，因此总线送来的值按名取译文，未收录的则原样显示。有几句话是例外，因为它们
 * 并非源自本组件：完成原因走 store 的 `completionReasonText`（`errors.completion.*`），
 * 队列已满的说明走 `errors.transcript.queueFull*` —— 是否受理由 store 判断。
 *
 * `plan.status.*` 说的是计划清单里一步的状态，与 `views.entries.status.*` 措辞相近却是
 * 两处不同的地方：那里是子任务记录卡，这里是计划清单的一行，因此各写各的。两组都由表格
 * 按键取值（`PLAN_STATUS_KEYS`、`STATUS_KEYS`），所以键名以字面量写在表格里。
 */

export default {
  chat: {
    rail: {
      title: '会话',
      open: '会话',
      openTitle: '打开会话列表',
      newSession: '新建',
      newSessionTitle: '开一条新连接，新会话需要它',
      loading: '加载中…',
      empty: '还没有已保存的会话。',
      live: '在线',
      nodes: '{count} 节点',
      tokens: '{count} tok',
      statsSessions: '{sessions} 会话 · {nodes} 节点',
      statsTokens: '{tokens} tokens · {blobs} blobs',
    },

    head: {
      liveSession: '当前会话 {session}',
    },

    sessionStatus: {
      idle: '空闲',
      running: '运行中',
      done: '已完成',
      error: '出错',
    },

    empty: {
      attached: '已接入，但总线不回放历史：这里只显示从现在起到达的消息帧。',
    },

    composer: {
      mode: '发送方式',
      ask: '直接回答',
      askTitle: '把这段话直接交给智能体回答',
      plan: '拆分任务',
      planTitle: '先让智能体把任务拆成若干步骤，再分派给子智能体执行',
      planHint: '智能体会先给出一份计划并分派出去，而不是自己一口气做完',
      permission: {
        label: '权限',
        alwaysAsk: '始终询问',
        askWhenNeeded: '必要时询问',
        fullAuto: '完全自动',
        hint: {
          alwaysAsk: '每次工具调用都先征求同意，未批准不执行',
          askWhenNeeded: '只在调用缺少本次运行还没有的权限时询问',
          fullAuto: '所有工具调用直接执行，不再询问',
        },
      },
      agentDefault: '智能体：默认',
      workflowLabel: '工作流',
      workflowAuto: '工作流：自动',
      workflowNew: '＋ 新建工作流…',
      workflowUnavailable: '服务端还没有提供工作流列表，暂时只能自动选择。',
      workflowWhen: '这个工作流适用于：{when}',
      workflowCreateTitle: '新建工作流',
      workflowCreateHint: '描述它该在什么时候用、做什么，服务端会据此生成并保存它。',
      workflowDescribePlaceholder: '例如：当任务要同时改前端和后端时…',
      workflowCreate: '创建',
      workflowCreating: '创建中…',
      workflowCancel: '取消',
      placeholderAsk: '说点什么。Enter 发送，Shift+Enter 换行。',
      placeholderPlan: '描述要拆分的任务。Enter 发送，Shift+Enter 换行。',
      send: '发送',
      planSend: '开始拆分',
      stop: '中止',
      steeringPlaceholder: '运行途中插话：补充说明会立刻送达',
      steer: '插话',
      steeringIdle: '只有任务执行中才能插话',
      steeringSent: '已送达正在执行的任务：{text}',
    },

    /**
     * 授权面板：某个工具调用被拦下，等待用户决定。它不阻塞页面 —— 会话栏和导航
     * 依然可点 —— 所以措辞是“请求”，而不是“警报”。
     */
    approval: {
      heading: '有工具调用需要你的授权',
      waiting: '共 {count} 项待处理',
      tool: '工具：{name}',
      reason: '被拦下的原因：{reason}',
      approve: '批准',
      deny: '拒绝',
    },

    lane: {
      label: '子智能体',
      labelTitle:
        '子智能体是主智能体派出去干活的分身，它的记录单独折叠在这里，不打断主流程。服务端存下了它自己的会话时，可以在这里打开那个会话。',
      entries: '共 {count} 条记录',
      open: '打开它的会话',
      openTitle: '打开服务端为这个子智能体存下的会话，看它实际做了什么',
      noSession: '暂不可打开',
      noSessionTitle:
        '服务端还没有给出这个子智能体对应的会话，所以暂时打不开它自己的会话。它的记录仍然折叠在下面。',
    },

    /**
     * 追溯：打开子智能体会话时那个面板。它说清楚“现在看的是什么”，
     * 并提供回到这条工作所属主会话的入口。
     */
    trace: {
      badge: '子智能体会话',
      worker: '子智能体 {id}',
      belongs: '属于 {title}',
      objective: '接到的任务：{objective}',
      back: '返回 {title}',
      backTitle: '回到这个子智能体所属的主会话',
      parentGone: '它所属的主会话已不在会话列表中',
    },

    runFinished: '本次运行已结束 · {reason}',

    /**
     * 计划清单：当前这次运行要做的步骤。每一行是一条状态线，不是第二份记录 ——
     * 点开某一行的目的是跳到那个步骤自己的子智能体会话，细节在那里。
     */
    plan: {
      heading: '本次计划',
      progress: '已完成 {finished}/{total}',
      inferred: '服务端还没有下发完整计划，下面按已经开始的步骤列出。',
      fromWorkflow: '来自工作流 {id}',
      fromWorkflowTitle: '这份计划由所选工作流给出，而不是现场拆分出来的',
      expand: '展开',
      collapse: '收起',
      openTitle: '打开这个步骤的子智能体会话，看它实际做了什么',
      agent: '智能体 {agent}',
      status: {
        pending: '待执行',
        running: '进行中',
        succeeded: '已完成',
        failed: '失败',
        skipped: '已跳过',
      },
    },

    /**
     * 时长在两个地方出现 —— 整轮回复和单次工具调用 —— 所以措辞只有这一份。
     * `done` 是结束后的用时，`elapsed` 是还在跑、数字仍在涨的那种。
     */
    duration: {
      done: '用时 {ms} 毫秒',
      elapsed: '已用时 {ms} 毫秒',
    },

    turn: {
      label: '第 {index} 次回复',
      labelTitle:
        '模型的一次完整回复：从开始输出到本轮结束，中间的思考、工具调用和文字都算在内。',
      agent: '智能体 {id}',
      thinking: '模型的思考过程 · {count} 字',
      thinkingTitle: '模型在给出答案前写下的推理过程，只解释它是怎么想的，不参与实际执行。',
    },

    code: {
      copy: '复制代码',
    },

    change: {
      summary: '本次修改了 {count} 个文件',
      deltaTitle:
        '行数取自工具调用本身：write_file 计写入内容的行数，edit_file 计被替换文本的行数。整文件写入替换掉的旧内容不在线路上，因此不计入减少。',
      addedTitle: '新增的行数',
      removedTitle: '删除的行数',
    },

    tool: {
      nameTitle: '工具标识：{name}。排查问题时，用它在日志或源码里检索这次调用。',
      idTitle: '上一行是工具的内部名称，这一行是本次调用的编号，用来和服务端日志对照。',
      arguments: '调用参数',
      progress: '运行过程',
      output: '返回结果',
      showFull: '展开完整结果（共 {count} 字）',
      hideFull: '收起完整结果',
    },

    /**
     * 工具的内部名称读起来像变量名，所以折叠行显示人话；名字本身不丢，它留在
     * 折叠行的提示里和展开后的标识行里，因为检索日志时用的正是那个字符串。
     */
    toolNames: {
      read_file: '读取文件',
      write_file: '写入文件',
      edit_file: '修改文件',
      list_dir: '列出目录',
      grep: '搜索文件内容',
      shell: '执行命令',
      web_fetch: '抓取网页',
      mcp: '外部工具 {tool}（来自 {server}）',
    },

    diagnostics: {
      title: '用量与拦截',
      toggleTitle: '本次运行的 token 用量、剩余额度和安全拦截记录，点开可以看到每一项的含义。',
      brief: '本次已用 {count} tokens',
      usageTitle: '最近一次模型调用的用量：输入是发给模型的文字量，输出是模型生成的文字量。',
      usage: '最近一次：输入 {input} · 输出 {output}',
      totalTitle: '本次会话到目前为止，所有模型调用的用量总和。',
      total: '本次会话累计 {count}',
      budget: '剩余额度 {count}',
      budgetUnbounded: '未设上限',
      noReports: '还没有用量数据',
      blocked: '（已拦截）',
      guardrails: '安全检查 {count} 次',
      guardrailsTitle: '安全检查是服务端对每次工具调用做的规则检查，比如凭据、隐私信息和越权操作。',
      guardrailsBlocked: '已拦截 {count} 项',
      guardrailNames: {
        secret_scanner: '敏感信息扫描',
        tool_policy: '工具权限策略',
        pii_detector: '隐私信息检测',
        llm_judge: '模型审查',
        content_fence: '提示注入防护',
        behavior_monitor: '重复调用检测',
      },
      lanes: '子智能体：{lanes}',
    },

    connection: {
      connected: '已连接，消息会实时送达',
      connecting: '正在连接服务器…',
      reconnecting: '连接已中断，正在重试（第 {attempt} 次）',
      reconnectingPlain: '连接已中断，正在重试…',
      disconnected: '未连接服务器 —— 点“重新连接”恢复',
      queued: '{count} 条消息待发送',
      queuedTitle: '{count} 条消息正在等待连接恢复后发送',
      connect: '重新连接',
    },

    error: {
      codeTitle: '服务端返回的错误代码：{code}，排查问题时可以用它检索日志。',
      codes: {
        busy: '服务器正忙',
        not_running: '当前没有正在运行的任务',
        agent_not_found: '找不到这个智能体',
        unknown_session: '找不到这个会话',
        bad_message: '消息格式错误',
        config: '配置错误',
        session_error: '会话出错',
        guardrail: '被安全规则拦截',
      },
    },

    toolStatus: {
      running: '进行中',
      ok: '已完成',
      error: '失败',
      rejected: '被拒绝',
      timeout: '超时',
    },
  },
};
