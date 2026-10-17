#!/usr/bin/env node
// 测试用的 mock MCP 服务器（stdio 上换行分隔的 JSON-RPC 2.0）。
//
// 工具：`echo`（标了只读，原样返回 text）、`boom`（返回 isError）。
// 调 `echo` 且 text 为 "relist" 时，先发 `notifications/tools/list_changed`，
// 之后的 `tools/list` 里多出一个 `extra` 工具。
import readline from 'node:readline';

let extra = false;
const send = (msg) => process.stdout.write(JSON.stringify(msg) + '\n');
const tools = () => [
  {
    name: 'echo',
    description: 'Echo the text back',
    inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
    annotations: { readOnlyHint: true },
  },
  { name: 'boom', description: 'Always fails', inputSchema: { type: 'object', properties: {} } },
  ...(extra ? [{ name: 'extra', description: 'Appears after relist', inputSchema: { type: 'object' } }] : []),
];

console.error('[mock-mcp] started');
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  const { id, method, params } = msg;
  if (id === undefined) return; // 通知
  switch (method) {
    case 'initialize':
      send({
        jsonrpc: '2.0',
        id,
        result: {
          protocolVersion: '2025-06-18',
          capabilities: { tools: { listChanged: true } },
          serverInfo: { name: 'mock', version: '0' },
        },
      });
      break;
    case 'tools/list':
      send({ jsonrpc: '2.0', id, result: { tools: tools() } });
      break;
    case 'tools/call': {
      const { name, arguments: args } = params ?? {};
      if (name === 'echo') {
        if (args?.text === 'relist') {
          extra = true;
          send({ jsonrpc: '2.0', method: 'notifications/tools/list_changed' });
        }
        send({ jsonrpc: '2.0', id, result: { content: [{ type: 'text', text: `echo: ${args?.text}` }] } });
      } else if (name === 'boom') {
        send({ jsonrpc: '2.0', id, result: { content: [{ type: 'text', text: 'boom failed' }], isError: true } });
      } else {
        send({ jsonrpc: '2.0', id, error: { code: -32602, message: `unknown tool ${name}` } });
      }
      break;
    }
    default:
      send({ jsonrpc: '2.0', id, error: { code: -32601, message: 'Method not found' } });
  }
});
rl.on('close', () => process.exit(0));
