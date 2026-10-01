export default {
  views: {
    agents: {
      title: '智能体',
      intro:
        '服务端每发现一个 {file} 就对应一个智能体。{tools} 是它可调用的工具白名单 —— 留空表示所有已注册工具。',
      loading: '加载中…',
      empty: '当前工作区没有找到任何智能体。',
      columns: {
        id: 'ID',
        name: '名称',
        model: '模型',
        tools: '工具',
        skills: '技能',
        maxTokens: '最大 token',
        source: '来源',
      },
      inherit: '继承默认',
      allTools: '全部',
      detail: {
        modelInherited: '继承路由默认模型',
        unbounded: '不限制',
        everyTool: '所有已注册工具',
        none: '无',
      },
    },

    extensions: {
      title: '扩展',
      intro: 'harness 能调用的东西：按需加载的技能、通过 MCP 调用的服务，以及在沙箱里运行的 WASM 插件。',
      loading: '加载中…',
      unavailable: {
        title: '暂无内容',
        explanation: '扩展接口没有响应，暂时没有可列出的内容。服务端提供该接口后，本页会自动填充。',
      },
      empty: '接口返回成功，但没有任何已注册的技能、MCP 服务或插件。',

      skills: {
        heading: '技能',
        cost: '元数据 {metadata} tok · 正文 {bodies} tok',
        disclosure: '（知道它们存在只需付全价的 {percent}%）',
        empty: '没有注册技能。',
        inherit: '继承默认',
        columns: {
          name: '名称',
          description: '说明',
          model: '模型',
          allowedTools: '允许的工具',
          metadataTokens: '元数据 tok',
          bodyTokens: '正文 tok',
          source: '来源',
        },
      },

      mcp: {
        heading: 'MCP 服务',
        configured: '已配置 {count} 个',
        empty: '没有配置 MCP 服务。',
        enabled: '已启用',
        disabled: '已禁用',
        lazy: '按需连接',
        noTools: '未声明任何工具',
        kind: {
          stdio: '服务端启动的本地进程，通过标准输入输出通信',
          http: '服务端调用的 HTTP 地址',
        },
      },

      plugins: {
        heading: 'WASM 插件',
        loaded: '已加载 {count} 个',
        empty: '没有加载任何插件。',
        entry: '入口 {entry}',
        capabilities: '能力',
        noCapabilities: '无 —— 模块未导入任何能力',
      },
    },

    entries: {
      handoff: '交接',
      subtask: '子任务 {status}',
      status: {
        pending: '等待中',
        running: '运行中',
        succeeded: '已完成',
        failed: '失败',
        skipped: '已跳过',
      },
    },
  },
};
