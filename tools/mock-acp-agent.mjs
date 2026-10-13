#!/usr/bin/env node
// claudesktop M1 的 mock ACP v1 agent。
//
// 用途：在不接真实 LLM 的前提下，端到端验证 claudesktop-core 的
// AcpKernel 协议实现（initialize / session/new / session/prompt /
// session/update 全变体 / session/request_permission / fs/read_text_file）。
//
// 传输：stdio 上的换行分隔 JSON-RPC 2.0。
import readline from 'node:readline';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const SELF_PATH = fileURLToPath(import.meta.url);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** @type {string|null} */
let sessionId = null;
let nextId = 0;
/** @type {Map<number, (result: any) => void>} */
const pending = new Map();

const send = (msg) => process.stdout.write(JSON.stringify(msg) + '\n');
const reply = (id, result) => send({ jsonrpc: '2.0', id, result });
const update = (u) => send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId, update: u } });

/** 向客户端发起一个请求并等待响应。 */
function request(method, params) {
  const id = ++nextId;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    send({ jsonrpc: '2.0', id, method, params });
  });
}

async function runTurn(promptId, text) {
  await sleep(40);
  update({ sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: `思考：用户说了「${text}」，我演示一轮完整的协议交互。` } });
  await sleep(40);
  for (const part of ['你好，', '我是 mock 内核，', '正在演示流式回复。']) {
    update({ sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: part }, messageId: 'm1' });
    await sleep(40);
  }
  update({
    sessionUpdate: 'plan',
    entries: [
      { content: '分析请求', priority: 'high', status: 'completed' },
      { content: '执行演示工具', priority: 'medium', status: 'in_progress' },
      { content: '汇总回复', priority: 'low', status: 'pending' },
    ],
  });

  const toolCallId = `tool-${Date.now()}`;
  update({
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
    sessionId,
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
  const fsResult = await request('fs/read_text_file', { sessionId, path: SELF_PATH });
  if (typeof fsResult?.content === 'string') {
    lines = fsResult.content.split('\n').length;
  }

  update({
    sessionUpdate: 'tool_call_update',
    toolCallId,
    status: granted && lines > 0 ? 'completed' : 'failed',
    rawOutput: { granted, lines },
  });
  update({ sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: `（工具完成：授权=${granted}，读到 ${lines} 行）` }, messageId: 'm1' });
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
        agentCapabilities: {},
        authMethods: [],
        agentInfo: { name: 'mock-agent', title: 'Mock Agent', version: '0.1.0' },
      });
      break;
    case 'authenticate':
      reply(id, {});
      break;
    case 'session/new':
      sessionId = `mock-session-${Date.now()}`;
      reply(id, { sessionId });
      break;
    case 'session/prompt': {
      const text = (params?.prompt ?? [])
        .filter((b) => b?.type === 'text')
        .map((b) => b.text)
        .join('');
      runTurn(id, text).catch((err) => {
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
