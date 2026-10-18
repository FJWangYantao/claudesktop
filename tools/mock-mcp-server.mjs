#!/usr/bin/env node
// 测试用的 mock MCP 服务器。
//
//   node mock-mcp-server.mjs                 stdio 上换行分隔的 JSON-RPC 2.0
//   node mock-mcp-server.mjs --http [token]  Streamable HTTP（POST /mcp），stdout 第一行打印 `PORT <端口>`
//   node mock-mcp-server.mjs --sse [token]   旧版 HTTP + SSE（GET /sse + POST /messages），同上
//   node mock-mcp-server.mjs --http --oauth  Streamable HTTP + OAuth 2.1（见下）
//
// 给了 token 时远程模式要求 `Authorization: Bearer <token>`，否则回 401。
//
// --oauth 模式（MCP 2025-06-18 的授权流程，测试用）：没带 Bearer 就回 401 并在
// `WWW-Authenticate` 里给出资源元数据地址；实现了 PRM、AS 元数据、动态注册、`/authorize`
// （**自动批准**，直接 302 回 redirect_uri，省掉真浏览器）、`/token`（授权码 + refresh 两种）。
// access token 默认 2 秒过期，可换 3 次之后把这次登录作废（回 401 让客户端重新登录）。
// 下面是 debug 端点，测试用来推进状态：
//   POST /oauth/expire           让所有 access token 失效（下一次请求会走刷新）
//   POST /oauth/revoke           让所有 refresh token 也失效（状态回到「需要登录」）
//   GET  /oauth/log              至今的授权 / 刷新次数（JSON）
//
// 工具：`echo`（标了只读，原样返回 text）、`boom`（返回 isError）、`crash`（模拟断线：
// stdio 退出进程；HTTP 丢掉所有会话，之后带旧会话的请求回 404；SSE 关掉事件流）。
// 调 `echo` 且 text 为 "relist" 时，先发 `notifications/tools/list_changed`，
// 之后的 `tools/list` 里多出一个 `extra` 工具。HTTP 模式下 `tools/call` 用 SSE 流回（通知在响应前面），
// 其余请求回 JSON。
import http from 'node:http';
import readline from 'node:readline';
import crypto from 'node:crypto';

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
const oauth = process.argv.includes('--oauth');
const authorized = (req) => !token || req.headers.authorization === `Bearer ${token}`;
const readBody = (req) =>
  new Promise((resolve) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => resolve(body));
  });
const listen = (server) => server.listen(0, '127.0.0.1', () => console.log(`PORT ${server.address().port}`));

// --- OAuth 状态 -------------------------------------------------------------

const ACCESS_TTL_MS = Number(process.env.MOCK_OAUTH_TTL_MS ?? 2000);
const access = new Map(); // access_token → { expires, scope }
const refresh = new Map(); // refresh_token → { ok }
const codes = new Map(); // code → { challenge, redirect, clientId, resource }
const clients = new Map(); // client_id → { redirect_uris }
const stats = { registered: 0, authorize: 0, token: 0, refresh: 0, denied: 0 };
let generation = 0; // 作废全部登录时 +1（老的 token 都不认）
const start = crypto.randomBytes(6).toString('hex');

const liveAccess = (t) => {
  const entry = access.get(t);
  return entry && entry.gen === generation && entry.expires > Date.now() ? entry : null;
};
const s256 = (verifier) =>
  crypto.createHash('sha256').update(verifier).digest('base64url');

/** 发一对 token；refresh 时把老的 access 作废（OAuth 2.1 要求轮换） */
function issue(clientId, scope, resource, withRefresh) {
  const at = `at-${start}-${stats.token}`;
  access.set(at, { gen: generation, expires: Date.now() + ACCESS_TTL_MS, scope });
  let rt = null;
  if (withRefresh) {
    rt = `rt-${start}-${stats.token}`;
    refresh.set(rt, { gen: generation });
  }
  return {
    access_token: at,
    ...(rt ? { refresh_token: rt } : {}),
    token_type: 'Bearer',
    expires_in: Math.ceil(ACCESS_TTL_MS / 1000),
    scope,
  };
}

function oauthRoutes(req, res, url) {
  const base = `http://127.0.0.1:${req.socket.localPort}`;
  const json = (status, body) => {
    res.writeHead(status, { 'Content-Type': 'application/json' }).end(JSON.stringify(body));
  };
  switch (url.pathname) {
    // 资源元数据（RFC 9728）
    case '/.well-known/oauth-protected-resource':
      return json(200, {
        resource: `${base}/mcp`,
        authorization_servers: [base],
        scopes_supported: ['read', 'write'],
        bearer_methods_supported: ['header'],
      });
    // 授权服务器元数据（RFC 8414）
    case '/.well-known/oauth-authorization-server':
      return json(200, {
        issuer: base,
        authorization_endpoint: `${base}/authorize`,
        token_endpoint: `${base}/token`,
        registration_endpoint: `${base}/register`,
        response_types_supported: ['code'],
        grant_types_supported: ['authorization_code', 'refresh_token'],
        code_challenge_methods_supported: ['S256'],
        token_endpoint_auth_methods_supported: ['none'],
      });
    // 动态客户端注册（RFC 7591）
    case '/register': {
      stats.registered++;
      return readBody(req).then((body) => {
        let reg = {};
        try {
          reg = JSON.parse(body);
        } catch {}
        const id = `client-${start}-${stats.registered}`;
        clients.set(id, { redirect_uris: reg.redirect_uris ?? [] });
        json(201, {
          client_id: id,
          redirect_uris: reg.redirect_uris ?? [],
          token_endpoint_auth_method: 'none',
        });
      });
    }
    // 授权页：自动批准，直接 302 回去（真浏览器那一步省掉）
    case '/authorize': {
      stats.authorize++;
      const redirect = url.searchParams.get('redirect_uri');
      const state = url.searchParams.get('state');
      const code = `code-${start}-${stats.authorize}`;
      codes.set(code, {
        challenge: url.searchParams.get('code_challenge'),
        redirect,
        clientId: url.searchParams.get('client_id'),
        resource: url.searchParams.get('resource'),
        scope: url.searchParams.get('scope') ?? 'read',
      });
      const to = new URL(redirect);
      to.searchParams.set('code', code);
      if (state) to.searchParams.set('state', state);
      res.writeHead(302, { Location: to.toString() }).end();
      return;
    }
    // 换 token / 刷新
    case '/token': {
      return readBody(req).then((body) => {
        const form = new URLSearchParams(body);
        const grant = form.get('grant_type');
        if (grant === 'authorization_code') {
          const entry = codes.get(form.get('code'));
          if (!entry) return json(400, { error: 'invalid_grant' });
          if (entry.redirect !== form.get('redirect_uri')) {
            return json(400, { error: 'invalid_grant', error_description: 'redirect_uri' });
          }
          if (entry.resource !== form.get('resource')) {
            return json(400, { error: 'invalid_target', error_description: 'resource' });
          }
          // PKCE：challenge 必须等于 verifier 的 S256
          if (entry.challenge !== s256(form.get('code_verifier') ?? '')) {
            return json(400, { error: 'invalid_grant', error_description: 'code_verifier' });
          }
          codes.delete(form.get('code'));
          stats.token++;
          return json(200, issue(entry.clientId, entry.scope, entry.resource, true));
        }
        if (grant === 'refresh_token') {
          const rt = form.get('refresh_token');
          const entry = refresh.get(rt);
          if (!entry || entry.gen !== generation) return json(400, { error: 'invalid_grant' });
          refresh.delete(rt); // 轮换
          stats.refresh++;
          return json(200, issue(form.get('client_id'), 'read', form.get('resource'), true));
        }
        return json(400, { error: 'unsupported_grant_type' });
      });
    }
    // 测试钩子：让 access 全部失效（下一次请求走刷新）
    case '/oauth/expire':
      for (const e of access.values()) e.expires = 0;
      return json(200, { ok: true });
    // 测试钩子：让 refresh 也失效（状态回到「需要登录」）
    case '/oauth/revoke':
      generation++;
      refresh.clear();
      return json(200, { ok: true });
    case '/oauth/log':
      return json(200, stats);
    default:
      return false;
  }
}

if (mode === '--http') {
  const sessions = new Set();
  let next = 1;
  listen(http.createServer(async (req, res) => {
    const url = new URL(req.url, 'http://x');
    if (oauth) {
      // 401 也要给元数据地址，客户端据此发现授权服务器
      const challenge = () =>
        `Bearer resource_metadata="http://127.0.0.1:${req.socket.localPort}/.well-known/oauth-protected-resource"`;
      const auth = req.headers.authorization ?? '';
      const at = auth.startsWith('Bearer ') ? auth.slice(7) : '';
      if (url.pathname === '/mcp') {
        const ok = oauth && !liveAccess(at);
        if (ok) {
          stats.denied++;
          res.writeHead(401, { 'WWW-Authenticate': challenge(), 'Content-Type': 'application/json' });
          return res.end('{"error":{"message":"需要授权"}}');
        }
      } else if (oauthRoutes(req, res, url) !== false) {
        return;
      }
    }
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
