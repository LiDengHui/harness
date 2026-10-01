/**
 * `history` 命名空间：复盘一段已保存的对话。
 *
 * 已保存的记录不是实时 transcript，这里的措辞也如实说明：消息里没有耗时、没有运行
 * 状态、也没有 token 用量，因此复盘时直接省略这些字段，而不是留一个空位。与实时
 * transcript 共用的键 —— 工具名称、`第 {index} 次回复` 标题、参数/结果标签、复制
 * 按钮 —— 一律复用 `chat` 命名空间，不在这里重复翻译。
 */

export default {
  history: {
    head: {
      stored: '已保存的对话',
      turns: '共 {count} 次回复',
      note: '这是对已保存记录的复盘：没有实时耗时、没有运行状态，也没有 token 用量 —— 这些当时就没有存下来。',
    },

    empty: {
      none: '这个会话没有保存任何消息。',
    },

    preamble: {
      label: '第一条消息之前',
    },

    system: {
      label: '系统提示',
    },

    role: {
      user: '你',
      unknown: '未知角色“{role}”',
    },

    message: {
      empty: '这条记录既没有文字，也没有工具调用',
    },

    tool: {
      orphan: '没有对应调用的结果',
      noResult: '这次调用没有记录结果',
    },
  },
};
