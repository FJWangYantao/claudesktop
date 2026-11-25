#!/usr/bin/env node
// claudesktop M1 的 mock ACP v1 agent。
//
// 用途：在不接真实 LLM 的前提下，端到端验证 claudesktop-core 的
// AcpKernel 协议实现（initialize / session/new / session/prompt /
// session/update 全变体 / session/request_permission / fs/read_text_file / fs/write_text_file）。
//
// 提示词以 `write:<路径>:<内容>` 开头时只经客户端写一个文件就结束这一轮（检查点测试用）。
// 提示词以 `llm:` 开头时向 `$ANTHROPIC_BASE_URL/v1/messages` 发一个流式请求（像 Claude Code 那样在
// metadata.user_id 的 JSON 里带会话 ID），把收到的字原样回给客户端（用量网关接外部 agent 的测试用）。
//
// 传输：stdio 上的换行分隔 JSON-RPC 2.0。
import readline from 'node:readline';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const SELF_PATH = fileURLToPath(import.meta.url);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// 续接能力：`--resume=resume|load` 参数优先（测试并行时不共享环境变量），其次 MOCK_RESUME
const RESUME_MODE =
  process.argv.find((a) => a.startsWith('--resume='))?.slice('--resume='.length) ??
  process.env.MOCK_RESUME ??
  '';
const PROMPT_IMAGE = process.argv.includes('--prompt-image') || process.env.MOCK_PROMPT_IMAGE === '1';
// session/new 之后、首轮 prompt 之前主动发一段正文（模拟 pi-acp 的开机清单）
const STARTUP_CHUNK = process.argv.includes('--startup-chunk') || process.env.MOCK_STARTUP_CHUNK === '1';
// 应答之后再下一 tick 才发（模拟 pi-acp 的 setTimeout 0，可能晚于客户端首轮 prompt）
const STARTUP_CHUNK_LATE =
  process.argv.includes('--startup-chunk-late') || process.env.MOCK_STARTUP_CHUNK_LATE === '1';
const AGENT_NAME =
  process.argv.find((a) => a.startsWith('--agent-name='))?.slice('--agent-name='.length) ??
  process.env.MOCK_AGENT_NAME ??
  'mock-agent';
const AUTH_MODE = process.argv.find((a) => a.startsWith('--auth='))?.slice('--auth='.length) ?? process.env.MOCK_AUTH ?? '';
const REQUIRE_AUTH = process.argv.includes('--require-auth') || process.env.MOCK_REQUIRE_AUTH === '1';
const AUTH_DELAY = Number(process.argv.find((a) => a.startsWith('--auth-delay='))?.slice('--auth-delay='.length) ?? process.env.MOCK_AUTH_DELAY_MS ?? 0);
let authenticated = !REQUIRE_AUTH;
const PIXEL_PNG ='iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAWElEQVR4nGO8Y+P2n2EAAdNAWj7qgNEQGA0BEGAhlE2c5KZRlM32PcoazYZ4ARPDAAOmUQcwjEbBAAOmUQcwjEbBAAOmEe8AxtF+AcNoIhxgwDTqAIYBBgAjBwf7jQE1sAAAAABJRU5ErkJggg==';

// 一条连接上可有多个会话：每个会话各自一个 sessionId，事件按它路由。
let sessionCount = 0;
// 会话 → 开会话时客户端交过来的 MCP 服务名
const mcpServers = new Map();
const sessionConfigs = new Map();
let nextId = 0;

function makeConfig(currentModel) {
  return [
    {
      id: 'model',
      name: '模型',
      category: 'model',
      type: 'select',
      currentValue: currentModel,
      options: [
        {
          group: 'fast',
          name: '快速',
          options: [
            { value: 'model-a', name: '模型 A', description: '默认模型' },
            { value: 'model-b', name: '模型 B', description: '切换后的模型' },
          ],
        },
        {
          group: 'careful',
          name: '深思',
          options: [{ value: 'model-c', name: '模型 C', description: '外部更新后的模型' }],
        },
      ],
    },
    {
      id: 'thinking',
      name: '思考强度',
      category: 'thought_level',
      type: 'select',
      currentValue: 'high',
      options: [
        { value: 'low', name: '低' },
        { value: 'high', name: '高' },
      ],
    },
  ];
}

function sessionModes(currentModeId = 'default') {
  return {
    currentModeId,
    availableModes: [
      { id: 'default', name: '逐项确认', description: '每次工具调用都确认' },
      { id: 'plan', name: '计划', description: '先规划再执行' },
    ],
  };
}

function advertisedAuthMethods() {
  const agent = { type: 'agent', id: 'login', name: 'Mock 登录', description: '完成一次模拟登录' };
  const terminal = { type: 'terminal', id: 'terminal-login', name: '终端登录', args: ['--terminal-login'] };
  if (AUTH_MODE === 'both') return [agent, terminal];
  if (AUTH_MODE === 'terminal') return [terminal];
  return [agent];
}

function emitStartupUpdates(sid) {
  update(sid, {
    sessionUpdate: 'available_commands_update',
    availableCommands: [
      { name: 'skill:review', description: '审查代码', input: { hint: '文件或目录' } },
      { name: 'status', description: '查看会话状态' },
    ],
  });
  update(sid, { sessionUpdate: 'usage_update', used: 1200, size: 10000, cost: { amount: 0.125, currency: 'USD' } });
  update(sid, { sessionUpdate: 'session_info_update', title: 'Mock 配置会话' });
  update(sid, { sessionUpdate: 'notice', severity: 'warning', title: 'mock 通知', description: '不会进日志' });
  update(sid, { sessionUpdate: 'compaction_update', compactionId: 'compact-1', status: 'in_progress' });
  update(sid, {
    sessionUpdate: 'compaction_summary_chunk',
    compactionId: 'compact-1',
    content: { type: 'text', text: '第一段摘要' },
  });
  update(sid, { sessionUpdate: 'compaction_update', compactionId: 'compact-1', status: 'completed' });
  if (STARTUP_CHUNK) {
    update(sid, {
      sessionUpdate: 'agent_message_chunk',
      content: { type: 'text', text: '## Context\nmock 开机清单：不是回复' },
      messageId: 'startup',
    });
  }
}
/** @type {Map<number, (result: any) => void>} */
const pending = new Map();

const send = (msg) => process.stdout.write(JSON.stringify(msg) + '\n');
const reply = (id, result) => send({ jsonrpc: '2.0', id, result });
const update = (sid, u) => send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: sid, update: u } });

/** 向客户端发起一个请求并等待响应。 */
function request(method, params) {
  const id = ++nextId;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    send({ jsonrpc: '2.0', id, method, params });
  });
}

/** 发一个 Anthropic 流式请求，拼出回答的文字。 */
async function askLlm(sid, text) {
  const base = process.env.ANTHROPIC_BASE_URL ?? 'https://api.anthropic.com';
  const res = await fetch(`${base}/v1/messages`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-api-key': process.env.ANTHROPIC_API_KEY ?? 'mock' },
    body: JSON.stringify({
      model: 'claude-sonnet-4-5',
      max_tokens: 100,
      stream: true,
      metadata: { user_id: JSON.stringify({ device_id: 'mock', account_uuid: '', session_id: sid }) },
      messages: [{ role: 'user', content: text }],
    }),
  });
  let answer = '';
  for (const line of (await res.text()).split('\n')) {
    if (!line.startsWith('data:')) continue;
    try {
      const event = JSON.parse(line.slice(5));
      if (event.delta?.type === 'text_delta') answer += event.delta.text;
    } catch {}
  }
  return answer || `HTTP ${res.status}`;
}

async function runElicitTurn(promptId, sid) {
  const answer = await request('elicitation/create', {
    sessionId: sid,
    mode: 'form',
    message: '选择执行参数',
    requestedSchema: {
      type: 'object',
      required: ['choice', 'multi', 'name', 'count', 'flag'],
      properties: {
        choice: {
          type: 'string',
          title: '执行方式',
          oneOf: [
            { const: 'safe', title: '稳妥' },
            { const: 'fast', title: '快速' },
          ],
        },
        multi: {
          type: 'array',
          title: '组件',
          items: { type: 'string', enum: ['core', 'ui', 'docs'] },
        },
        name: { type: 'string', title: '名称', minLength: 1 },
        count: { type: 'integer', title: '次数', minimum: 1, maximum: 10 },
        flag: { type: 'boolean', title: '启用' },
      },
    },
  });
  update(sid, {
    sessionUpdate: 'agent_message_chunk',
    content: { type: 'text', text: JSON.stringify(answer) },
  });
  reply(promptId, { stopReason: 'end_turn' });
}

async function runContentTurn(promptId, sid) {
  const toolCallId = `content-${sid}`;
  update(sid, {
    sessionUpdate: 'tool_call',
    toolCallId,
    title: '读取多模态结果',
    kind: 'other',
    status: 'in_progress',
    content: [
      { type: 'content', content: { type: 'text', text: '工具正文' } },
      { type: 'content', content: { type: 'image', data: PIXEL_PNG, mimeType: 'image/png', uri: 'mock://pixel' } },
      {
        type: 'content',
        content: {
          type: 'resource_link',
          name: '协议文档',
          title: 'ACP 文档',
          uri: 'https://example.com/acp',
          description: '仅显示，不自动请求',
        },
      },
      {
        type: 'content',
        content: {
          type: 'resource',
          resource: { uri: 'mock://embedded', text: '嵌入正文', mimeType: 'text/plain' },
        },
      },
      { type: 'diff', path: SELF_PATH, oldText: '旧文本', newText: '新文本' },
      { type: 'terminal', terminalId: 'terminal-mock' },
    ],
  });
  await sleep(20);
  update(sid, {
    sessionUpdate: 'tool_call_update',
    toolCallId,
    status: 'completed',
    rawOutput: { note: '原始输出不应覆盖结构化内容' },
  });
  reply(promptId, { stopReason: 'end_turn' });
}

async function runReadRange(promptId, sid, path, line, limit) {
  const result = await request('fs/read_text_file', { sessionId: sid, path, line, limit });
  const toolCallId = `range-${sid}`;
  update(sid, {
    sessionUpdate: 'tool_call',
    toolCallId,
    title: '分页读取',
    kind: 'read',
    status: result.code ? 'failed' : 'completed',
  });
  update(sid, {
    sessionUpdate: 'tool_call_update',
    toolCallId,
    status: result.code ? 'failed' : 'completed',
    rawOutput: result,
  });
  reply(promptId, { stopReason: 'end_turn' });
}

async function runTurn(promptId, sid, text) {
  await sleep(40);
  update(sid, { sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: `思考：用户说了「${text}」，我演示一轮完整的协议交互。` } });
  await sleep(40);
  for (const part of ['你好，', '我是 mock 内核，', '正在演示流式回复。']) {
    update(sid, { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: part }, messageId: 'm1' });
    await sleep(40);
  }
  update(sid, {
    sessionUpdate: 'plan',
    entries: [
      { content: '分析请求', priority: 'high', status: 'completed' },
      { content: '执行演示工具', priority: 'medium', status: 'in_progress' },
      { content: '汇总回复', priority: 'low', status: 'pending' },
    ],
  });

  const toolCallId = `tool-${sid}-${Date.now()}`;
  update(sid, {
    sessionUpdate: 'tool_call',
    toolCallId,
    title: '读取 mock-acp-agent.mjs',
    kind: 'read',
    status: 'in_progress',
    rawInput: { path: SELF_PATH },
  });

  // 权限请求 → 客户端自动应答
  // 响应结构（ACP 官方 schema）：{ outcome: { outcome: 'selected', optionId } | { outcome: 'cancelled' } }
  const permission = await request('session/request_permission', {
    sessionId: sid,
    toolCall: { toolCallId, title: '读取 mock-acp-agent.mjs', status: 'pending' },
    options: [
      { optionId: 'allow_once', name: '允许一次', kind: 'allow_once' },
      { optionId: 'reject_once', name: '拒绝一次', kind: 'reject_once' },
    ],
  });
  const outcome = permission?.outcome ?? {};
  const granted = outcome.outcome === 'selected' && outcome.optionId === 'allow_once';

  // fs 读请求（客户端已声明 fs.readTextFile 能力）
  let lines = -1;
  const fsResult = await request('fs/read_text_file', { sessionId: sid, path: SELF_PATH });
  if (typeof fsResult?.content === 'string') {
    lines = fsResult.content.split('\n').length;
  }

  update(sid, {
    sessionUpdate: 'tool_call_update',
    toolCallId,
    status: granted && lines > 0 ? 'completed' : 'failed',
    rawOutput: { granted, lines },
  });
  update(sid, { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: `（工具完成：授权=${granted}，读到 ${lines} 行）` }, messageId: 'm1' });
  reply(promptId, { stopReason: 'end_turn' });
}

const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  line = line.trim();
  if (!line) return;
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    console.error('[mock] bad json:', line.slice(0, 200));
    return;
  }
  // 客户端对我们请求的响应
  if (msg.id !== undefined && (msg.result !== undefined || msg.error !== undefined)) {
    const resolve = pending.get(msg.id);
    if (resolve) {
      pending.delete(msg.id);
      resolve(msg.error ?? msg.result ?? {});
    }
    return;
  }
  const { id, method, params } = msg;
  switch (method) {
    case 'initialize':
      reply(id, {
        protocolVersion: params?.protocolVersion ?? 1,
        // MOCK_RESUME=resume → 声明 sessionCapabilities.resume；=load → 声明 loadSession
        agentCapabilities: {
          promptCapabilities: { image: PROMPT_IMAGE, audio: false, embeddedContext: true },
          ...(RESUME_MODE === 'load' ? { loadSession: true } : {}),
          ...(RESUME_MODE === 'resume' ? { sessionCapabilities: { resume: {} } } : {}),
        },
        authMethods: AUTH_MODE ? advertisedAuthMethods() : [],
        agentInfo: { name: AGENT_NAME, title: 'Mock Agent', version: '0.1.0' },
      });
      break;
    case 'authenticate':
      setTimeout(() => {
        authenticated = true;
        reply(id, {});
      }, AUTH_DELAY);
      break;
    case 'session/new': {
      if (REQUIRE_AUTH && !authenticated) {
        send({ jsonrpc: '2.0', id, error: { code: -32000, message: 'Authentication required' } });
        break;
      }
      const sid = `mock-session-${Date.now()}-${++sessionCount}`;
      // 记下客户端交过来的 MCP 服务（插件带的），「mcp?」时回给客户端看
      mcpServers.set(sid, (params?.mcpServers ?? []).map((s) => s.name));
      sessionConfigs.set(sid, makeConfig('model-a'));
      reply(id, { sessionId: sid, configOptions: sessionConfigs.get(sid), modes: sessionModes() });
      emitStartupUpdates(sid);
      if (STARTUP_CHUNK_LATE) {
        setTimeout(
          () =>
            update(sid, {
              sessionUpdate: 'agent_message_chunk',
              content: { type: 'text', text: '## Context\n晚到的开机清单' },
              messageId: 'startup',
            }),
          0,
        );
      }
      break;
    }
    case 'session/resume':
      // 不回放历史，直接续用原 sessionId
      reply(id, {});
      break;
    case 'session/load': {
      // 先按协议回放历史（客户端应丢弃），再应答
      const sid = params?.sessionId;
      update(sid, { sessionUpdate: 'user_message_chunk', content: { type: 'text', text: '（历史回放）旧提问' } });
      update(sid, { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: '（历史回放）旧回答' } });
      reply(id, {});
      break;
    }
    case 'session/set_config_option': {
      const sid = params?.sessionId;
      const config = sessionConfigs.get(sid) ?? makeConfig('model-a');
      const value = params?.value?.value ?? params?.value;
      if (params?.configId === 'model') {
        const model = config.find((option) => option.id === 'model');
        model.currentValue = value;
      } else if (params?.configId === 'thinking') {
        const thinking = config.find((option) => option.id === 'thinking');
        thinking.currentValue = value;
      }
      sessionConfigs.set(sid, config);
      reply(id, { configOptions: config });
      break;
    }
    case 'session/set_mode': {
      reply(id, {});
      update(params?.sessionId, { sessionUpdate: 'current_mode_update', currentModeId: params?.modeId });
      break;
    }
    case 'session/prompt': {
      const text = (params?.prompt ?? [])
        .filter((b) => b?.type === 'text')
        .map((b) => b.text)
        .join('');
      if (text === 'blocks?') {
        const types = (params?.prompt ?? []).map((b) => b?.type ?? 'unknown').join(',');
        update(params?.sessionId, {
          sessionUpdate: 'agent_message_chunk',
          content: { type: 'text', text: `prompt blocks: ${types}` },
        });
        reply(id, { stopReason: 'end_turn' });
        break;
      }
      if (text === 'elicit') {
        runElicitTurn(id, params?.sessionId);
        break;
      }
      if (text === 'external') {
        const sid = params?.sessionId;
        const config = sessionConfigs.get(sid) ?? makeConfig('model-a');
        config.find((option) => option.id === 'model').currentValue = 'model-c';
        sessionConfigs.set(sid, config);
        update(sid, { sessionUpdate: 'config_option_update', configOptions: config });
        reply(id, { stopReason: 'end_turn' });
        break;
      }
      if (text === 'content') {
        runContentTurn(id, params?.sessionId);
        break;
      }
      if (text.startsWith('range:')) {
        const parts = text.slice('range:'.length).split(':');
        const limit = Number(parts.pop());
        const line = Number(parts.pop());
        const path = parts.join(':');
        runReadRange(id, params?.sessionId, path, line, limit);
        break;
      }
      if (text === 'mcp?') {
        const names = mcpServers.get(params?.sessionId) ?? [];
        update(params?.sessionId, {
          sessionUpdate: 'agent_message_chunk',
          content: { type: 'text', text: `mcp servers: ${names.join(',')}` },
        });
        reply(id, { stopReason: 'end_turn' });
        break;
      }
      if (text.startsWith('llm:')) {
        askLlm(params?.sessionId, text.slice('llm:'.length))
          .then((answer) => {
            update(params?.sessionId, { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: answer } });
            reply(id, { stopReason: 'end_turn' });
          })
          .catch((err) => send({ jsonrpc: '2.0', id, error: { code: -32603, message: String(err) } }));
        break;
      }
      if (text.startsWith('write:')) {
        // 路径里可能有盘符的冒号：按最后一个冒号切
        const rest = text.slice('write:'.length);
        const cut = rest.lastIndexOf(':');
        const path = rest.slice(0, cut);
        const content = rest.slice(cut + 1);
        request('fs/write_text_file', { sessionId: params?.sessionId, path, content })
          .then((result) => {
            update(params?.sessionId, {
              sessionUpdate: 'tool_call_update',
              toolCallId: 'client-write',
              status: result.code ? 'failed' : 'completed',
              rawOutput: result,
            });
            reply(id, { stopReason: 'end_turn' });
          });
        break;
      }
      runTurn(id, params?.sessionId, text).catch((err) => {
        console.error('[mock] turn failed:', err);
        send({ jsonrpc: '2.0', id, error: { code: -32603, message: String(err) } });
      });
      break;
    }
    case 'session/cancel':
      // mock 轮次极短，取消无需特殊处理
      break;
    default:
      if (id !== undefined) reply(id, {});
  }
});
rl.on('close', () => process.exit(0));
