import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { deflateSync } from 'node:zlib';
import { mkdir, writeFile, readFile, readdir } from 'node:fs/promises';
import { createWriteStream } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = Object.fromEntries(process.argv.slice(2).map(arg => {
  const [key, ...value] = arg.replace(/^--/, '').split('=');
  return [key, value.join('=') || 'true'];
}));
const exe = path.resolve(args.exe || 'D:/t3/release/claudesktop.exe');
const count = Number(args.sessions || 50);
const rounds = Number(args.rounds || 3);
const soakSeconds = Number(args.soak || 1800);
const mode = args.mode || 'all';
const displayComparison = args.compare === 'display';
const output = path.resolve(args.out || path.join(root, '.claudesktop', 'bench', new Date().toISOString().replace(/[:.]/g, '-')));
assert(Number.isInteger(count) && count >= 4 && count <= 200);
assert(Number.isInteger(rounds) && rounds >= 1);
assert(Number.isFinite(soakSeconds) && soakSeconds >= 0);
assert(['all', 'ab', 'soak', 'smoke', 'summary'].includes(mode));
assert(output.startsWith(path.join(root, '.claudesktop', 'bench') + path.sep));
await mkdir(output, { recursive: true });
if (mode !== 'summary' && !args.resume) assert.equal((await readdir(output)).length, 0, 'Use a new output directory for each measurement.');
if (args.resume) assert(['ab', 'all'].includes(mode), 'Resume is supported for paired measurements.');

const percentile = (values, p) => {
  assert(values.length > 0, 'empty measurement');
  const sorted = values.toSorted((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * p) - 1)];
};
const median = values => percentile(values, 0.5);
const hash = value => createHash('sha256').update(typeof value === 'string' ? value : JSON.stringify(value)).digest('hex');
const normalize = value => Array.isArray(value) ? value.map(normalize) : value && typeof value === 'object'
  ? Object.fromEntries(Object.entries(value).filter(([key]) => key !== 'cache_control').map(([key, item]) => [key, normalize(item)])) : value;
const textOf = content => typeof content === 'string' ? content : (content || []).filter(block => block.type === 'text').map(block => block.text).join('\n');
const reply = Array.from({ length: Number(args['reply-paragraphs'] || 64) }, (_, i) => `Paragraph ${i.toString().padStart(4, '0')}: ${'measured local session history with tools, images and signed thinking. '.repeat(3)}\n\n`).join('');
const thinking = 'Reasoning retained verbatim with its signature.\n'.repeat(16000);

function png() {
  const crc = buffer => {
    let n = 0xffffffff;
    for (const byte of buffer) {
      n ^= byte;
      for (let bit = 0; bit < 8; bit++) n = (n >>> 1) ^ ((n & 1) ? 0xedb88320 : 0);
    }
    return (n ^ 0xffffffff) >>> 0;
  };
  const chunk = (name, data) => {
    const type = Buffer.from(name);
    const size = Buffer.alloc(4); size.writeUInt32BE(data.length);
    const checksum = Buffer.alloc(4); checksum.writeUInt32BE(crc(Buffer.concat([type, data])));
    return Buffer.concat([size, type, data, checksum]);
  };
  const width = 384, height = 768;
  const pixels = Buffer.alloc((width * 3 + 1) * height);
  let random = 1234567;
  for (let y = 0; y < height; y++) for (let x = 1; x <= width * 3; x++) {
    random ^= random << 13; random ^= random >>> 17; random ^= random << 5;
    pixels[y * (width * 3 + 1) + x] = random & 255;
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width); header.writeUInt32BE(height, 4); header[8] = 8; header[9] = 2;
  return Buffer.concat([Buffer.from('89504e470d0a1a0a', 'hex'), chunk('IHDR', header), chunk('IDAT', deflateSync(pixels)), chunk('IEND', Buffer.alloc(0))]);
}
const image = { media_type: 'image/png', data: png().toString('base64') };
const runs = [];

async function run(label, enabled, soak = 0) {
  const dir = path.join(output, label);
  if (args.resume) {
    let files;
    try { files = await readdir(dir); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (files?.length) {
      assert(files.includes('result.json') && files.includes('summary.json'), `Preserve or move aside the incomplete run before retrying: ${dir}`);
      const result = JSON.parse(await readFile(path.join(dir, 'result.json'), 'utf8'));
      const summary = JSON.parse(await readFile(path.join(dir, 'summary.json'), 'utf8'));
      assert(result.passed && result.label === label && result.count === count && result.enabled === enabled);
      assert(summary.comparison_kind === (displayComparison ? 'display' : 'kernel') && summary.count === count && summary.enabled === enabled);
      assert(summary.displayChecks === (displayComparison ? count : 0));
      console.log(`${label}: reuse verified completed run`);
      return summary;
    }
  }
  const workspace = path.join(dir, 'workspace');
  const dataDir = path.join(dir, 'data');
  await mkdir(path.join(workspace, '.git'), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(path.join(workspace, 'fixture.txt'), 'fixture-marker: unchanged\n' + 'file payload checked by the real read_file tool\n'.repeat(1500));
  await writeFile(path.join(dataDir, 'settings.json'), JSON.stringify({ notifications: false, suggestions: false, memory_suggest: false, usage_gateway: false }));
  const requests = [];
  const expected = new Map();
  const rebuilt = new Map();
  const compactedSessions = new Set();
  const errors = [];
  let requestSequence = 0;
  let canceledStreams = 0;
  let strictChecks = 0;
  let displayChecks = 0;
  const displayViews = new Map();
  const log = createWriteStream(path.join(dir, 'requests.jsonl'));
  const server = createServer(async (req, res) => {
    try {
      if (req.method === 'GET') { res.end(JSON.stringify({ data: [] })); return; }
      const buffers = [];
      for await (const buffer of req) buffers.push(buffer);
      const body = JSON.parse(Buffer.concat(buffers).toString());
      assert.equal(body.model, 'local-hibernate-bench', 'session model changed');
      const messages = normalize(body.messages || []);
      const system = JSON.stringify(body.system || '');
      const compact = system.includes('You compress the history');
      let userIndex = -1;
      let tag;
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role !== 'user') continue;
        const match = textOf(messages[i].content).match(/bench:(\w+):(\d+):(\d+)/);
        if (match) { userIndex = i; tag = { kind: match[1], session: Number(match[2]), seq: Number(match[3]), text: match[0] }; break; }
      }
      assert(compact || tag, `unrecognized request: ${JSON.stringify(messages).slice(0, 250)}`);
      const requestId = ++requestSequence;
      const record = { epoch_ms: Date.now(), requestId, ...tag, compact, messages: messages.length, bytes: Buffer.concat(buffers).length, hash: hash(messages) };
      requests.push(record); log.write(JSON.stringify(record) + '\n');
      if (!compact && !['seed', 'child'].includes(tag.kind)) {
        const prefix = expected.get(tag.session);
        if (prefix) {
          assert.deepEqual(messages.slice(0, prefix.length).map(hash), prefix, `${tag.text}: history changed across wake`);
          assert(messages.some(message => textOf(message.content) === reply), 'final seed reply changed across wake');
          strictChecks++;
          const seedTag = `bench:seed:${tag.session}:0`;
          assert.equal(messages.filter(message => textOf(message.content).includes(seedTag)).length, 1, 'seed user duplicated');
        } else if (rebuilt.has(tag.session)) {
          const { surviving, removed } = rebuilt.get(tag.session);
          assert(messages.some(message => textOf(message.content).includes(surviving)), 'rewind lost the preceding turn');
          assert(!messages.some(message => textOf(message.content).includes(removed)), 'rewind kept the removed turn');
        }
        if (compactedSessions.has(tag.session)) assert(system.includes('Local benchmark summary:'), 'compaction summary missing after wake');
      }
      const tail = messages.slice(userIndex + 1);
      const used = tail.flatMap(message => Array.isArray(message.content) ? message.content : []).filter(block => block.type === 'tool_use').map(block => block.name);
      const results = tail.flatMap(message => Array.isArray(message.content) ? message.content : []).filter(block => block.type === 'tool_result');
      for (const result of results) assert(!result.is_error, `${tag?.text}: tool failed ${JSON.stringify(result).slice(0, 400)}`);
      const available = new Set((body.tools || []).map(tool => tool.name));
      const tool = (name, input) => ({ type: 'tool_use', id: `call-${label}-${requestId}`, name, input });
      let blocks;
      if (compact) blocks = [{ type: 'text', text: 'Local benchmark summary: preserve fixture-marker, prior successful turns, and continue the requested validation.' }];
      else if (tag.kind === 'seed' && !used.includes('read_file')) blocks = [
        { type: 'thinking', thinking, signature: `signed-thinking-${tag.session}` },
        { type: 'redacted_thinking', data: `opaque-thinking-${tag.session}` },
        tool('read_file', { path: 'fixture.txt' }),
      ];
      else if (tag.kind === 'seed') {
        assert(results.some(result => JSON.stringify(result).includes('fixture-marker')), 'read_file result missing');
        expected.set(tag.session, messages.map(hash));
        blocks = [{ type: 'text', text: reply }];
      } else {
        const target = { read: 'read_file', write: 'write_file', question: 'ask_user', subagent: 'task', child: 'read_file', background: 'shell_background' }[tag.kind];
        if (target && !available.has(target) && !used.includes('tool_search')) blocks = [tool('tool_search', { query: target })];
        else if (target && !used.includes(target)) {
          assert(available.has(target), `tool_search did not expose ${target}`);
          const input = {
            read_file: { path: 'fixture.txt', limit: 3 },
            write_file: { path: `written-${tag.session}.txt`, content: `validated ${tag.text}\n` },
            ask_user: { questions: [{ question: '继续验收吗？', options: [{ label: '继续' }, { label: '停止' }] }] },
            task: { description: 'Local fixture check', prompt: `bench:child:${tag.session}:${tag.seq} Read fixture.txt and report its marker.`, subagent_type: 'explore' },
            shell_background: { command: 'node -e "setTimeout(()=>console.log(\'background-ok\'),7000)"' },
          }[target];
          blocks = [tool(target, input)];
        } else blocks = [{ type: 'text', text: `Completed ${tag.text}; fixture-marker preserved.` }];
      }
      res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
      const event = value => res.write(`event: ${value.type}\ndata: ${JSON.stringify(value)}\n\n`);
      event({ type: 'message_start', message: { id: `msg-${requestId}`, role: 'assistant', usage: { input_tokens: 800, output_tokens: 0 } } });
      let index = 0;
      for (const block of blocks) {
        const type = block.type;
        event({ type: 'content_block_start', index, content_block: type === 'tool_use' ? { ...block, input: {} } : type === 'thinking' ? { type, thinking: '', signature: '' } : type === 'redacted_thinking' ? block : { type, text: '' } });
        if (type === 'tool_use') event({ type: 'content_block_delta', index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } });
        if (type === 'text' || type === 'thinking') {
          const text = type === 'thinking' ? block.thinking : block.text;
          for (let offset = 0; offset < text.length; offset += 32768) {
            if (res.destroyed) return;
            event({ type: 'content_block_delta', index, delta: type === 'thinking' ? { type: 'thinking_delta', thinking: text.slice(offset, offset + 32768) } : { type: 'text_delta', text: text.slice(offset, offset + 32768) } });
          }
          if (type === 'thinking') event({ type: 'content_block_delta', index, delta: { type: 'signature_delta', signature: block.signature } });
        }
        event({ type: 'content_block_stop', index }); index++;
      }
      if (tag?.kind === 'slow') {
        let ended = false;
        res.once('close', () => { if (!ended) canceledStreams++; });
        for (let i = 0; i < 100; i++) {
          await delay(50);
          if (res.destroyed) return;
          event({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'streaming work\n' } });
        }
        ended = true;
      }
      event({ type: 'message_delta', delta: { stop_reason: blocks.some(block => block.type === 'tool_use') ? 'tool_use' : 'end_turn' }, usage: { output_tokens: 100 } });
      event({ type: 'message_stop' }); res.end();
    } catch (error) {
      errors.push(error.stack); log.write(JSON.stringify({ error: error.stack }) + '\n');
      if (!res.headersSent) res.writeHead(500, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ error: { message: error.message } }));
    }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('CLAUDESKTOP_') && !key.startsWith('ACP_')));
  Object.assign(env, {
    CLAUDESKTOP_HIBERNATE_BENCH: dir, CLAUDESKTOP_DATA_DIR: dataDir,
    CLAUDESKTOP_KERNEL: 'builtin', CLAUDESKTOP_BASE_URL: `http://127.0.0.1:${server.address().port}/v1`,
    CLAUDESKTOP_MODEL: 'local-hibernate-bench', CLAUDESKTOP_API_KEY: 'local-test-key',
    CLAUDESKTOP_PROTOCOL: 'anthropic', CLAUDESKTOP_GATEWAY: '0',
    CLAUDESKTOP_HIBERNATE: enabled ? '1' : '0', CLAUDESKTOP_HIBERNATE_IDLE_SECS: '2',
    CLAUDESKTOP_DISABLE_RECLAIM: '1', ACP_CWD: workspace,
  });
  if (enabled && args.activation === 'default') delete env.CLAUDESKTOP_HIBERNATE;
  if (displayComparison) {
    env.CLAUDESKTOP_HIBERNATE = '1';
    delete env.CLAUDESKTOP_DISABLE_RECLAIM;
    if (!enabled) env.CLAUDESKTOP_DISABLE_DISPLAY_RECLAIM = '1';
  }
  const stderr = createWriteStream(path.join(dir, 'app.log'));
  const app = spawn(exe, [], { cwd: workspace, env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  app.stdout.pipe(stderr); app.stderr.pipe(stderr);
  let appExit;
  const appClosed = new Promise(resolve => app.once('exit', (code, signal) => { appExit = { code, signal }; resolve(appExit); }));
  const samplerLog = createWriteStream(path.join(dir, 'sampler.log'));
  const sampler = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(root, 'tools', 'hibernate-sample.ps1'), '-ProcessId', String(app.pid), '-Directory', dir], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  sampler.stdout.pipe(samplerLog); sampler.stderr.pipe(samplerLog);
  const samplerClosed = new Promise(resolve => sampler.once('exit', (code, signal) => resolve({ code, signal })));
  let commandId = 0;
  const command = async (action, options = {}) => {
    const id = ++commandId;
    await writeFile(path.join(dir, 'command.json'), JSON.stringify({ id, action, ...options }));
    const deadline = Date.now() + 60000;
    while (Date.now() < deadline) {
      assert(!appExit, `app exited ${JSON.stringify(appExit)}`);
      let response;
      try { response = JSON.parse(await readFile(path.join(dir, 'response.json'), 'utf8')); } catch {}
      if (response?.id === id) { assert(response.ok, response.error); return response.value; }
      await delay(10);
    }
    throw new Error(`command timeout: ${action}`);
  };
  const phases = [];
  const phase = async name => {
    await writeFile(path.join(dir, 'phase.txt'), name);
    phases.push({ phase: name, epoch_ms: Date.now() });
    console.log(`${label}: ${name}`);
  };
  const wait = async (predicate, timeout = 60000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      assert.equal(errors.length, 0, errors.join('\n'));
      const status = await command('status');
      for (const s of status.sessions) {
        assert.equal(s.failures.length, 0, JSON.stringify(s));
        assert(!s.history_error, JSON.stringify(s));
      }
      if (predicate(status)) return status;
      await delay(50);
    }
    throw new Error('state wait timeout');
  };
  const tokens = [];
  const identities = new Map();
  const sends = [];
  let operation = 0;
  const turn = async (token, kind, session, { seed = false, cancel = false } = {}) => {
    const seq = seed ? 0 : ++operation;
    const tag = `bench:${kind}:${session}:${seq}`;
    if (displayComparison) await wait(status => status.sessions.find(s => s.token === token)?.history_loaded);
    const before = (await command('status')).sessions.find(s => s.token === token);
    assert(before?.ready && !before.busy && !before.rewinding);
    const sent = await command('send', { token, text: tag, ...(seed ? { images: [image] } : {}) });
    if (cancel) {
      await wait(() => requests.some(request => request.text === tag));
      await delay(200);
      await command('cancel', { token });
    }
    const done = await wait(status => {
      const s = status.sessions.find(s => s.token === token);
      return s && !s.busy && !s.stopping && !s.rewinding;
    });
    const after = done.sessions.find(s => s.token === token);
    assert.equal(after.users, before.users + 1, 'send duplicated/lost the UI user message');
    assert.equal(after.id, before.id, 'wake recreated a kernel session');
    assert.equal(after.mode, before.mode, 'wake changed permission mode');
    assert.equal(after.restored, false, 'wake used log resume');
    const received = requests.find(request => request.text === tag);
    assert(received, 'send did not reach provider');
    if (kind === 'probe') sends.push({ token, session, seq, send_to_wire_ms: received.epoch_ms - sent.result.sent_at_ms });
    return after;
  };
  const turnWithAnswers = async (token, kind, session) => {
    const before = (await command('status')).sessions.find(s => s.token === token);
    const tag = `bench:${kind}:${session}:${++operation}`;
    await command('send', { token, text: tag });
    let answered = false;
    const end = Date.now() + 60000;
    while (Date.now() < end) {
      assert.equal(errors.length, 0, errors.join('\n'));
      const s = (await command('status')).sessions.find(s => s.token === token);
      assert.equal(s.failures.length, 0, JSON.stringify(s));
      if (s.permission || s.question) {
        const value = await command('answer', { token });
        assert.equal(value.result, kind === 'question' ? 'question' : 'permission');
        answered = true;
      }
      if (!s.busy && !s.stopping) {
        assert(answered, `${kind} did not exercise user approval`);
        assert.equal(s.users, before.users + 1);
        if (kind === 'write') assert((await readFile(path.join(workspace, `written-${session}.txt`), 'utf8')).includes(tag));
        return;
      }
      await delay(50);
    }
    throw new Error('answer turn timeout');
  };
  let passed = false;
  try {
    await phase('seed');
    for (let i = 0; i < count; i++) {
      const token = (await command('open')).result.token;
      tokens.push(token);
      await wait(status => status.sessions.some(s => s.token === token && s.ready));
      const s = await turn(token, 'seed', i, { seed: true });
      identities.set(token, { id: s.id, mode: s.mode });
      if (displayComparison) displayViews.set(token, (await command('inspect', { token })).result);
    }
    assert.equal(requests.filter(request => request.kind === 'seed').length, count * 2);
    await phase('resident');
    for (let i = 0; i < 20; i++) { await command('status'); await delay(250); }
    await phase('idle');
    await command('hibernate');
    await wait(status => {
      const dormant = displayComparison || enabled;
      return (dormant ? status.dormant === count - 3 : status.resident >= count)
        && status.transitioning === 0
        && (!displayComparison || status.sessions.every(s => !s.history_loading)
          && status.display_loaded === (enabled ? 3 : count));
    });
    await phase('dormant');
    for (let i = 0; i < 30; i++) { await command('status'); await delay(250); }
    await phase('probe');
    for (let i = 0; i < count; i++) {
      await command('activate', { token: tokens[i] });
      await delay(200);
      if (displayComparison) {
        await wait(status => status.sessions.find(s => s.token === tokens[i])?.history_loaded);
        assert.deepEqual((await command('inspect', { token: tokens[i] })).result, displayViews.get(tokens[i]), 'UI display changed across reclaim');
        displayChecks++;
      }
      await turn(tokens[i], 'probe', i);
      if (enabled) await command('hibernate');
    }
    if (displayComparison && enabled) {
      const token = tokens[0];
      await delay(2200);
      await command('hibernate');
      await wait(status => status.sessions.find(s => s.token === token)?.display_cached && !status.sessions.find(s => s.token === token)?.history_loading);
      const tag = `bench:queued_display:0:${++operation}`;
      await command('send', { token, text: tag });
      const queued = await wait(status => {
        const s = status.sessions.find(s => s.token === token);
        return s.history_loaded && !s.busy && !s.history_loading && s.users === 3;
      });
      const s = queued.sessions.find(s => s.token === token);
      assert.equal(s.id, identities.get(token).id);
      assert.equal(s.mode, identities.get(token).mode);
      assert.equal(s.restored, false);
      assert.equal(requests.filter(r => r.text === tag).length, 1);
    }
    if (displayComparison && enabled && mode === 'smoke') {
      const token = tokens[1];
      await delay(2200);
      await command('hibernate');
      await wait(status => status.sessions.find(s => s.token === token)?.display_cached && !status.sessions.find(s => s.token === token)?.history_loading);
      const snapshotDir = path.join(dataDir, 'display-cache');
      let snapshotPath, original;
      for (const name of await readdir(snapshotDir)) {
        if (!name.endsWith('.json')) continue;
        const candidate = path.join(snapshotDir, name);
        const bytes = await readFile(candidate);
        if (JSON.parse(bytes).token === token) { snapshotPath = candidate; original = bytes; break; }
      }
      assert(snapshotPath, 'UI snapshot not found for fault check');
      await writeFile(snapshotPath, '{invalid');
      await command('activate', { token });
      const failed = await command('status');
      let failure = failed.sessions.find(s => s.token === token);
      for (let n = 0; n < 100 && !failure.history_error; n++) { await delay(50); failure = (await command('status')).sessions.find(s => s.token === token); }
      assert(failure.history_error && !failure.history_loaded && failure.ready);
      const tag = `bench:after_display_retry:1:${++operation}`;
      await command('send', { token, text: tag });
      await delay(200);
      assert.equal(requests.filter(r => r.text === tag).length, 0, 'corrupt UI fell back to a new kernel session');
      assert.equal((await command('status')).sessions.find(s => s.token === token).queued, 1);
      await writeFile(snapshotPath, original);
      await command('activate', { token });
      const recovered = await wait(status => {
        const s = status.sessions.find(s => s.token === token);
        return s.history_loaded && !s.busy && s.users === 3;
      });
      assert.equal(recovered.sessions.find(s => s.token === token).id, identities.get(token).id);
      assert.equal(requests.filter(r => r.text === tag).length, 1, 'UI retry duplicated the queued prompt');
    }
    if (soak > 0 || mode === 'smoke') {
      await phase('mixed');
      const mixedStart = Date.now();
      const counts = { read: 0, write: 0, question: 0, subagent: 0, background: 0, cancel: 0, compact: 0, rewind: 0, close: 0 };
      let cycle = 0;
      do {
        const i = cycle % tokens.length, token = tokens[i];
        await command('activate', { token });
        await turn(token, 'read', i); counts.read++;
        await turnWithAnswers(token, 'write', i); counts.write++;
        await turnWithAnswers(token, 'question', i); counts.question++;
        const child = await turn(token, 'subagent', i);
        assert(child.subagents > 0, 'no visible subagent'); counts.subagent++;
        await turnWithAnswers(token, 'background', i);
        const background = (await command('status')).tasks.findLast(task => task.session === identities.get(token).id && task.running);
        assert(background, 'background process did not start');
        for (let n = 1; n <= 3; n++) await command('activate', { token: tokens[(i + n) % tokens.length] });
        await delay(2200);
        const during = await command('hibernate');
        assert(during.tasks.some(task => task.id === background.id && task.running), 'background task ended before protection check');
        assert(!during.snapshots.some(name => name.startsWith(background.session + '-')), 'running background task was hibernated');
        const completed = await wait(status => status.tasks.some(task => task.id === background.id && !task.running));
        const task = completed.tasks.find(task => task.id === background.id);
        assert.equal(task.exit_code, 0); assert(task.output_tail.includes('background-ok')); counts.background++;
        await command('activate', { token });
        await turn(token, 'slow', i, { cancel: true }); counts.cancel++;
        await command('hibernate');
        await turn(token, 'after_cancel', i);
        const beforeRewind = (await command('status')).sessions.find(s => s.token === token);
        expected.delete(i);
        rebuilt.set(i, {
          surviving: requests.findLast(request => request.session === i && request.kind === 'slow').text,
          removed: requests.findLast(request => request.session === i && request.kind === 'after_cancel').text,
        });
        await command('rewind', { token });
        const rewound = await wait(status => !status.sessions.find(s => s.token === token).rewinding);
        assert.equal(rewound.sessions.find(s => s.token === token).users, beforeRewind.users - 1); counts.rewind++;
        await turn(token, 'after_rewind', i);
        rebuilt.delete(i);
        await command('compact', { token });
        const compacted = await wait(status => !status.sessions.find(s => s.token === token).busy);
        assert(compacted.sessions.find(s => s.token === token).compactions > 0); counts.compact++;
        compactedSessions.add(i);
        await command('hibernate');
        await turn(token, 'after_compact', i);
        const replacement = (await command('open')).result.token;
        await wait(status => status.sessions.some(s => s.token === replacement && s.ready));
        await turn(replacement, 'probe', 100000 + cycle);
        await command('close', { token: replacement }); counts.close++;
        cycle++;
        if (mode === 'smoke') break;
        const nextCycle = Math.min(mixedStart + soak * 1000, mixedStart + cycle * 30000);
        while (Date.now() < nextCycle) { await command('hibernate'); await delay(1000); }
        if (cycle % 5 === 0) console.log(`${label}: mixed ${Math.round((Date.now() - mixedStart) / 1000)}s, cycles=${cycle}`);
      } while (Date.now() - mixedStart < soak * 1000);
      const mixedElapsed = (Date.now() - mixedStart) / 1000;
      if (mode !== 'smoke') assert(mixedElapsed >= soak);
      assert(canceledStreams > 0, 'no provider stream cancellation observed');
      await writeFile(path.join(dir, 'mixed.json'), JSON.stringify({ counts, elapsed_seconds: mixedElapsed, canceledStreams }, null, 2));
    }
    await phase('finished');
    const final = await command('status');
    for (const token of tokens) {
      const s = final.sessions.find(s => s.token === token);
      assert.deepEqual({ id: s.id, mode: s.mode }, identities.get(token));
    }
    assert(strictChecks >= count, 'insufficient lossless history checks');
    const events = (await readFile(path.join(dir, 'events.jsonl'), 'utf8')).trim().split('\n').map(JSON.parse);
    assert.equal(events.filter(event => ['fatal', 'prompt_failed', 'session_failed', 'compact_failed'].includes(event.event)).length, 0);
    const started = events.filter(event => event.event === 'started').map(event => event.session);
    assert.equal(new Set(started).size, started.length, 'duplicate Started on wake');
    await command('quit');
    await Promise.race([appClosed, delay(10000).then(() => { throw new Error('app exit timeout'); })]);
    assert.equal(appExit.code, 0);
    const snapshots = await readdir(path.join(dataDir, 'session-hibernation')).catch(error => { if (error.code === 'ENOENT') return []; throw error; });
    assert.equal(snapshots.length, 0, 'shutdown left history snapshots');
    const displays = await readdir(path.join(dataDir, 'display-cache')).catch(error => { if (error.code === 'ENOENT') return []; throw error; });
    assert.equal(displays.length, 0, 'shutdown left UI snapshots');
    passed = true;
    await writeFile(path.join(dir, 'result.json'), JSON.stringify({ label, enabled, count, phases, sends, strictChecks, displayChecks, requests: requests.length, canceledStreams, passed }, null, 2));
  } finally {
    await writeFile(path.join(dir, 'sample.stop'), 'stop');
    if (!appExit) app.kill();
    await appClosed;
    const sampleExit = await samplerClosed;
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    log.end();
    if (passed) assert.equal(sampleExit.code, 0, 'memory sampler failed; inspect sampler.log');
    if (!passed) await writeFile(path.join(dir, 'FAILED'), errors.join('\n') || 'See controller error and app.log');
  }
  const csv = (await readFile(path.join(dir, 'memory.csv'), 'utf8')).trim().split(/\r?\n/);
  const columns = csv.shift().split(',');
  const samples = csv.map(line => Object.fromEntries(line.split(',').map((value, i) => [columns[i], i === 1 ? value : Number(value)])));
  for (const name of ['resident', 'dormant']) assert(samples.filter(row => row.phase === name).length >= 10, `insufficient ${name} samples`);
  const selected = name => samples.filter(row => row.phase === name);
  const summarize = name => Object.fromEntries(['pb', 'ws', 'tree_pb', 'tree_ws', 'tree_count', 'cpu_ms', 'read_bytes', 'write_bytes'].map(key => [key, median(selected(name).map(row => row[key]))]));
  const appLog = await readFile(path.join(dir, 'app.log'), 'utf8');
  const preflight = appLog.split(/\r?\n/).filter(line => line.startsWith('[hibernate_send] ')).map(line => JSON.parse(line.slice('[hibernate_send] '.length)));
  const wake = [...appLog.matchAll(/session_wake bytes=\d+ read_ms=([\d.]+) parse_ms=([\d.]+) wait_ms=([\d.]+) total_ms=([\d.]+)/g)].map(match => Number(match[3]) + Number(match[4]));
  const dormantSamples = selected('dormant');
  const firstDormant = dormantSamples[0], lastDormant = dormantSamples.at(-1);
  const result = {
    label, enabled, count, comparison_kind: displayComparison ? 'display' : 'kernel', activation: env.CLAUDESKTOP_HIBERNATE ?? 'default',
    resident: summarize('resident'), dormant: summarize('dormant'),
    dormant_cpu_one_core_percent: (lastDormant.cpu_ms - firstDormant.cpu_ms) / (lastDormant.epoch_ms - firstDormant.epoch_ms) * 100,
    dormant_read_bytes: lastDormant.read_bytes - firstDormant.read_bytes,
    dormant_write_bytes: lastDormant.write_bytes - firstDormant.write_bytes,
    max_sample_gap_ms: Math.max(...samples.slice(1).map((row, i) => row.epoch_ms - samples[i].epoch_ms)),
    snapshot_bytes_per_session: Math.max(...[...appLog.matchAll(/session_hibernate bytes=(\d+)/g)].map(match => Number(match[1])), 0),
    peak_pb: Math.max(...samples.map(row => row.peak_pb)), peak_ws: Math.max(...samples.map(row => row.peak_ws)),
    sampled_tree_peak: Math.max(...samples.map(row => row.tree_pb)), samples: samples.length,
    send_to_wire_p95_ms: percentile(sends.map(send => send.send_to_wire_ms), 0.95),
    probe_preflight_p95_ms: percentile(preflight.slice(count, count * 2).map(row => row.preflight_ms), 0.95),
    wake_p95_ms: wake.length ? percentile(wake, 0.95) : null, wake_count: wake.length, strictChecks, displayChecks,
    display_wake_p95_ms: (() => { const values = [...appLog.matchAll(/display_wake total_ms=([\d.]+)/g)].map(m => Number(m[1])); return values.length ? percentile(values, 0.95) : null; })(),
  };
  await writeFile(path.join(dir, 'summary.json'), JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
  return result;
}

try {
  if (mode === 'summary') runs.push(...JSON.parse(await readFile(path.join(output, 'summary.json'), 'utf8')).runs);
  else if (mode === 'smoke') runs.push(await run('smoke', true));
  else {
    if (mode !== 'soak') for (let round = 0; round < rounds; round++) {
      for (const enabled of round % 2 ? [true, false] : [false, true]) runs.push(await run(`ab-${round + 1}-${enabled ? 'on' : 'off'}`, enabled));
    }
    if (mode !== 'ab') runs.push(await run('soak-on', true, soakSeconds));
  }
  const off = runs.filter(result => !result.enabled), on = runs.filter(result => result.label.startsWith('ab-') && result.enabled);
  let comparison;
  if (off.length && on.length) {
    const difference = key => (median(off.map(result => result.dormant[key])) - median(on.map(result => result.dormant[key]))) / 1048576;
    const pairs = on.map(enabled => {
      const disabled = off.find(result => result.label === enabled.label.replace(/on$/, 'off'));
      assert(disabled, 'missing paired baseline');
      return {
        round: enabled.label.split('-')[1],
        pb_saved_mib: (disabled.dormant.pb - enabled.dormant.pb) / 1048576,
        ws_saved_mib: (disabled.dormant.ws - enabled.dormant.ws) / 1048576,
        peak_ratio: enabled.peak_pb / disabled.peak_pb,
        wake_p95_ms: enabled.wake_p95_ms,
        extra_send_to_wire_p95_ms: enabled.send_to_wire_p95_ms - disabled.send_to_wire_p95_ms,
        extra_preflight_p95_ms: enabled.probe_preflight_p95_ms - disabled.probe_preflight_p95_ms,
      };
    });
    comparison = {
      pairs,
      pb_saved_mib: difference('pb'), ws_saved_mib: difference('ws'),
      tree_pb_saved_mib: difference('tree_pb'), tree_ws_saved_mib: difference('tree_ws'),
      peak_ratio: Math.max(...pairs.map(pair => pair.peak_ratio)),
      wake_p95_ms: Math.max(...pairs.map(pair => pair.wake_p95_ms)),
      extra_send_to_wire_p95_ms: Math.max(...pairs.map(pair => pair.extra_send_to_wire_p95_ms)),
      extra_preflight_p95_ms: Math.max(...pairs.map(pair => pair.extra_preflight_p95_ms)),
    };
    comparison.thresholds_passed = pairs.every(pair => pair.pb_saved_mib >= (displayComparison ? 0 : 30) && pair.ws_saved_mib >= (displayComparison ? 0 : 20) && pair.peak_ratio <= 1.1 && pair.wake_p95_ms <= 150 && pair.extra_send_to_wire_p95_ms <= 20);
  }
  await writeFile(path.join(output, 'summary.json'), JSON.stringify({ exe, count, rounds, soakSeconds, comparison_kind: displayComparison ? 'display' : 'kernel', runs, comparison }, null, 2));
  console.log(JSON.stringify({ output, comparison }));
} catch (error) {
  await writeFile(path.join(output, 'error.txt'), error.stack);
  console.error(error.stack); process.exitCode = 1;
}
