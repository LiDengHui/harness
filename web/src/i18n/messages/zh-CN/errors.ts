/**
 * `errors` 命名空间：传输层、REST 客户端与各 store 可能呈现给用户的全部文案。
 *
 * 这些句子原本是写在逻辑里的字面量。现在失败只保留自己的 code 与字段，
 * 措辞在用户读到它的那一刻才拼出来 —— 这样切换语言才能重新渲染屏幕上已有的内容。
 */
export default {
  errors: {
    transport: {
      openFailed: '无法打开 {url}：{reason}',
      socketError: '套接字报错',
      reconnecting: '连接已断开，正在重连（第 {attempt} 次）',
      queueOverflow: '丢弃了一条待发送消息：队列中已有 {limit} 条',
      flushFailed: '发送队列清空失败：{reason}',
      sendFailed: '发送失败，改为排队：{reason}',
      pongTimeout: '{timeout} 毫秒内未收到 pong，判定连接已断开',
      serverSilent: '服务端不再响应 ping，正在重连',
      heartbeatFailed: '心跳失败：{reason}',
      nonTextFrame: '收到 {kind} 帧；该连接只接受文本帧',
      handlerThrew: '帧处理函数抛出异常：{reason}',
      protocol: '{detail}',
    },

    api: {
      unreachable: '{path} 无法访问：{reason}',
      status: '{path} 返回 {status}',
      statusWithDetail: '{path} 返回 {status}：{detail}',
      notJson: '{path} 的响应不是 JSON：{reason}',
      unknown: '请求失败：{reason}',
      extensionsUnavailable: '此服务端尚未提供 /api/extensions，暂无可列出的内容；路由上线后此处会自动填充。',
    },

    transcript: {
      backpressureLabel: '连接已关闭',
      backpressure:
        '由于本客户端消费过慢，服务端关闭了连接，而不是无限制地堆积消息。会话仍然保留，重连后即可继续。',
      approvalLabel: '需要批准',
      approvalRequest: '{name}（{toolCallId}）需要批准：{reason}',
      queueFullLabel: '队列已满',
      queueFull:
        '这个会话等待中的消息已经到达上限，这条因此没有被排队。等其中一条开始运行后再发一次。{detail}',
      previewMore: '… 还有 {count} 个字符',
    },

    completion: {
      end_turn: '正常结束',
      max_iterations: '达到迭代上限',
      aborted: '已中止',
      budget_exceeded: '超出预算',
      error: '出错',
    },

    capability: {
      file_read: '读取文件',
      file_write: '写入文件',
      network_access: '网络访问',
      shell_exec: '执行命令',
    },
  },
};
