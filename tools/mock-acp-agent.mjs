#!/usr/bin/env node
// claudesktop M1 的 mock ACP v1 agent。
//
// 用途：在不接真实 LLM 的前提下，端到端验证 claudesktop-core 的
// AcpKernel 协议实现（initialize / session/new / session/prompt /
// session/update 全变体 / session/request_permission / fs/read_text_file / fs/write_text_file）。
//
// 提示词以 `write:<路径>:<内容>` 开头时只经客户端写一个文件就结束这一轮（检查点测试用）。
// 提示词以 `llm:` 开头时向 `$ANTHROPIC_BASE_URL/v1/messages` 发一个流式请求（像 Claude Code 那样在
// metadata.user_id 里带会话 ID），把收到的字原样回给客户端（用量网关接外部 agent 的测试用）。
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

// 一条连接上可有多个会话：每个会话各自一个 sessionId，事件按它路由。
let sessionCount = 0;
// 会话 → 开会话时客户端交过来的 MCP 服务名
const mcpServers = new Map();
let nextId = 0;
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
      metadata: { user_id: `user_mock_account__session_${sid}` },
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
          ...(RESUME_MODE === 'load' ? { loadSession: true } : {}),
          ...(RESUME_MODE === 'resume' ? { sessionCapabilities: { resume: {} } } : {}),
        },
        authMethods: [],
        agentInfo: { name: 'mock-agent', title: 'Mock Agent', version: '0.1.0' },
      });
      break;
    case 'authenticate':
      reply(id, {});
      break;
    case 'session/new': {
      const sid = `mock-session-${Date.now()}-${++sessionCount}`;
      // 记下客户端交过来的 MCP 服务（插件带的），「mcp?」时回给客户端看
      mcpServers.set(sid, (params?.mcpServers ?? []).map((s) => s.name));
      reply(id, { sessionId: sid });
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
    case 'session/prompt': {
      const text = (params?.prompt ?? [])
        .filter((b) => b?.type === 'text')
        .map((b) => b.text)
        .join('');
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
          .then(() => reply(id, { stopReason: 'end_turn' }));
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
