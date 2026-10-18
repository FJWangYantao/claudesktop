#!/usr/bin/env node
// 测试用的 mock MCP 服务器。
//
//   node mock-mcp-server.mjs                 stdio 上换行分隔的 JSON-RPC 2.0
//   node mock-mcp-server.mjs --http [token]  Streamable HTTP（POST /mcp），stdout 第一行打印 `PORT <端口>`
//   node mock-mcp-server.mjs --sse [token]   旧版 HTTP + SSE（GET /sse + POST /messages），同上
//
// 给了 token 时远程模式要求 `Authorization: Bearer <token>`，否则回 401。
//
// 工具：`echo`（标了只读，原样返回 text）、`boom`（返回 isError）、`crash`（模拟断线：
// stdio 退出进程；HTTP 丢掉所有会话，之后带旧会话的请求回 404；SSE 关掉事件流）。
// 调 `echo` 且 text 为 "relist" 时，先发 `notifications/tools/list_changed`，
// 之后的 `tools/list` 里多出一个 `extra` 工具。HTTP 模式下 `tools/call` 用 SSE 流回（通知在响应前面），
// 其余请求回 JSON。
import http from 'node:http';
import readline from 'node:readline';

let extra = false;
const tools = () => [
  {
    name: 'echo',
    description: 'Echo the text back',
    inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
    annotations: { readOnlyHint: true },
  },
  { name: 'boom', description: 'Always fails', inputSchema: { type: 'object', properties: {} } },
  { name: 'crash', description: 'Drops the connection', inputSchema: { type: 'object', properties: {} } },
  ...(extra ? [{ name: 'extra', description: 'Appears after relist', inputSchema: { type: 'object' } }] : []),
];

// 一条请求 → 要发回的消息（通知在前、响应在后）；`crash` 时第二项为 true
function handle(msg) {
  const { id, method, params } = msg;
  if (id === undefined) return [[], false]; // 通知
  const reply = (result) => ({ jsonrpc: '2.0', id, result });
  switch (method) {
    case 'initialize':
      return [[reply({
        protocolVersion: '2025-06-18',
        capabilities: { tools: { listChanged: true } },
        serverInfo: { name: 'mock', version: '0' },
      })], false];
    case 'tools/list':
      return [[reply({ tools: tools() })], false];
    case 'tools/call': {
      const { name, arguments: args } = params ?? {};
      if (name === 'echo') {
        const out = [];
        if (args?.text === 'relist') {
          extra = true;
          out.push({ jsonrpc: '2.0', method: 'notifications/tools/list_changed' });
        }
        out.push(reply({ content: [{ type: 'text', text: `echo: ${args?.text}` }] }));
        return [out, false];
      }
      if (name === 'boom') {
        return [[reply({ content: [{ type: 'text', text: 'boom failed' }], isError: true })], false];
      }
      if (name === 'crash') return [[reply({ content: [{ type: 'text', text: 'bye' }] })], true];
      return [[{ jsonrpc: '2.0', id, error: { code: -32602, message: `unknown tool ${name}` } }], false];
    }
    default:
      return [[{ jsonrpc: '2.0', id, error: { code: -32601, message: 'Method not found' } }], false];
  }
}

const mode = process.argv[2];
const token = process.argv[3];
const authorized = (req) => !token || req.headers.authorization === `Bearer ${token}`;
const readBody = (req) =>
  new Promise((resolve) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => resolve(body));
  });
const listen = (server) => server.listen(0, '127.0.0.1', () => console.log(`PORT ${server.address().port}`));

if (mode === '--http') {
  const sessions = new Set();
  let next = 1;
  listen(http.createServer(async (req, res) => {
    if (!authorized(req)) return res.writeHead(401).end('{"error":{"message":"bad token"}}');
    if (req.method === 'DELETE') {
      sessions.delete(req.headers['mcp-session-id']);
      return res.writeHead(200).end();
    }
    const msg = JSON.parse(await readBody(req));
    const headers = {};
    if (msg.method === 'initialize') {
      const sid = `s${next++}`;
      sessions.add(sid);
      headers['Mcp-Session-Id'] = sid;
    } else if (!sessions.has(req.headers['mcp-session-id'])) {
      return res.writeHead(404).end();
    }
    const [out, crash] = handle(msg);
    if (crash) sessions.clear();
    if (out.length === 0) return res.writeHead(202, headers).end();
    if (msg.method === 'tools/call') {
      res.writeHead(200, { ...headers, 'Content-Type': 'text/event-stream' });
      for (const m of out) res.write(`event: message\ndata: ${JSON.stringify(m)}\n\n`);
      return res.end();
    }
    res.writeHead(200, { ...headers, 'Content-Type': 'application/json' }).end(JSON.stringify(out[0]));
  }));
} else if (mode === '--sse') {
  const streams = new Map();
  let next = 1;
  listen(http.createServer(async (req, res) => {
    if (!authorized(req)) return res.writeHead(401).end();
    const url = new URL(req.url, 'http://x');
    if (req.method === 'GET' && url.pathname === '/sse') {
      const sid = `s${next++}`;
      streams.set(sid, res);
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      res.write(`event: endpoint\ndata: /messages?session=${sid}\n\n`);
      req.on('close', () => streams.delete(sid));
      return;
    }
    const stream = streams.get(url.searchParams.get('session'));
    if (!stream) return res.writeHead(404).end();
    const [out, crash] = handle(JSON.parse(await readBody(req)));
    res.writeHead(202).end();
    for (const m of out) stream.write(`event: message\ndata: ${JSON.stringify(m)}\n\n`);
    if (crash) {
      for (const s of streams.values()) s.end();
      streams.clear();
    }
  }));
} else {
  const send = (msg) => process.stdout.write(JSON.stringify(msg) + '\n');
  console.error('[mock-mcp] started');
  const rl = readline.createInterface({ input: process.stdin, terminal: false });
  rl.on('line', (line) => {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      return;
    }
    const [out, crash] = handle(msg);
    out.forEach(send);
    if (crash) process.exit(1);
  });
  rl.on('close', () => process.exit(0));
}
