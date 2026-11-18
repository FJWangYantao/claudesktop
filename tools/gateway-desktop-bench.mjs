import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { GatewayObserver, configureReceiveBuffer, configureBenchmarkClock, executableComparison, pairPassed } from './gateway-bench-observer.mjs';
import { createHash } from 'node:crypto';
import { gunzipSync, deflateSync } from 'node:zlib';
import { mkdir, mkdtemp, readFile, writeFile, readdir, rm, open, cp, rename } from 'node:fs/promises';
import { createWriteStream } from 'node:fs';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import path from 'node:path';
import assert from 'node:assert/strict';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = Object.fromEntries(process.argv.slice(2).map(arg => {
  const [key, ...value] = arg.replace(/^--/, '').split('=');
  return [key, value.join('=')];
}));
const controllerTimer = configureBenchmarkClock(args['precise-timers'], args['tcp-buffer-helper']);
const output = path.resolve(args.out || '.claudesktop/bench/gateway-memory/desktop-runs');
assert(output.startsWith(path.join(root, '.claudesktop/bench') + path.sep));
await mkdir(output, { recursive: true });
assert.equal((await readdir(output)).length, 0, 'Use a new output directory.');
const versions = args.baseline && args.candidate
  ? { baseline: path.resolve(args.baseline), candidate: path.resolve(args.candidate) }
  : { baseline: path.resolve(args.exe) };
const { comparison, hashes: executableHashes } = await executableComparison(args.comparison, versions);
const rounds = Number(args.rounds || 3);
const turns = Number(args.turns || 20);
const protocols = (args.protocols || 'openai,anthropic,responses').split(',');
const sizes = (args.sizes || 'long').split(',');
const saves = (args.save || 'on,off').split(',');
const receiver = args.receiver || 'iterator';
const observationMode = args.observer || 'background';
const mcpTools = Number(args['mcp-tools'] || 0);
const loadout = args['loaded-tools'] || 'all';
const windowLayout = args['window-layout'] || 'current';
assert(['current', 'legacy'].includes(windowLayout));
assert(Number.isInteger(mcpTools) && mcpTools >= 0 && mcpTools <= 1000);
assert(['none', 'small', 'all'].includes(loadout));
const loadedTools = loadout === 'all' ? mcpTools : loadout === 'small' ? Math.min(mcpTools, 3) : 0;
const highWaterMark = args['high-water-mark'] ? Number(args['high-water-mark']) : null;
const receiveBuffer = args['receive-buffer'] === undefined ? null : Number(args['receive-buffer']);
assert(highWaterMark === null || (Number.isInteger(highWaterMark) && highWaterMark >= 16384 && highWaterMark <= 4 * 1024 * 1024));
assert(['events', 'iterator'].includes(receiver));
assert(['background', 'idle'].includes(observationMode));
assert.equal(process.platform, 'win32');
assert(Number.isInteger(rounds) && rounds > 0 && Number.isInteger(turns) && turns >= 20);
assert(protocols.every(name => ['openai', 'anthropic', 'responses'].includes(name)));
assert(sizes.every(name => ['short', 'long'].includes(name)));
assert(saves.every(name => ['on', 'off'].includes(name)));
async function setAffinity(pid, mask) {
  assert(/^\d+$/.test(mask) && BigInt(mask) > 0n);
  const child = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(root, 'tools/gateway-bench-affinity.ps1'), '-ProcessId', String(pid), '-Mask', mask], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', bytes => { stdout += bytes; });
  child.stderr.on('data', bytes => { stderr += bytes; });
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
  assert.equal(code, 0, stderr);
  return JSON.parse(stdout.replace(/^\uFEFF/, ''));
}
const controlAffinity = args['control-affinity'] ? await setAffinity(process.pid, args['control-affinity']) : null;
assert(args['data-in-output'] !== '1' || args.profile === '1', 'Output-local data is restricted to diagnostic runs.');
assert(!args['profile-variant'] || (args.profile === '1' && ['baseline', 'candidate'].includes(args['profile-variant'])));
const fixture = await mkdtemp(path.join(tmpdir(), 'claudesktop-gateway-desktop-'));
const cwd = path.join(fixture, 'cwd');
await mkdir(cwd);
await writeFile(path.join(cwd, 'sample.txt'), 'roundtrip sample\n');
function chunk(name, data) {
  const type = Buffer.from(name);
  let crc = 0xffffffff;
  for (const byte of Buffer.concat([type, data])) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
  }
  const size = Buffer.alloc(4); size.writeUInt32BE(data.length);
  const checksum = Buffer.alloc(4); checksum.writeUInt32BE((crc ^ 0xffffffff) >>> 0);
  return Buffer.concat([size, type, data, checksum]);
}
const imageHeader = Buffer.alloc(13);
imageHeader.writeUInt32BE(1); imageHeader.writeUInt32BE(1, 4); imageHeader[8] = 8; imageHeader[9] = 6;
const png = Buffer.concat([Buffer.from('89504e470d0a1a0a', 'hex'), chunk('IHDR', imageHeader), chunk('IDAT', deflateSync(Buffer.from([0, 100, 120, 140, 255]))), chunk('IEND', Buffer.alloc(0))]);
await writeFile(path.join(cwd, 'sample.png'), png);
const historyImages = [];
for (let index = 0; index < 3; index++) {
  const pixels = Buffer.alloc((1024 * 3 + 1) * 1024, 60 + index * 40);
  for (let row = 0; row < 1024; row++) pixels[row * (1024 * 3 + 1)] = 0;
  const header = Buffer.from(imageHeader);
  header.writeUInt32BE(1024); header.writeUInt32BE(1024, 4); header[9] = 2;
  const image = Buffer.concat([Buffer.from('89504e470d0a1a0a', 'hex'), chunk('IHDR', header), chunk('IDAT', deflateSync(pixels, { level: 0 })), chunk('IEND', Buffer.alloc(0))]);
  const file = path.join(cwd, `history-${index}.png`);
  await writeFile(file, image);
  historyImages.push(file);
}
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const percentile = (values, p) => {
  const sorted = values.toSorted((a, b) => a - b);
  assert(sorted.length > 0);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * p) - 1)];
};
const cleanEnv = {
  ...process.env,
  HTTP_PROXY: '', HTTPS_PROXY: '', ALL_PROXY: '', http_proxy: '', https_proxy: '', all_proxy: '',
  NO_PROXY: '127.0.0.1,localhost', no_proxy: '127.0.0.1,localhost',
};
let observer;
const toolsKey = Buffer.from('"tools":');
function seed(size) {
  const event = (type, data) => ({ t: 'event', event: { type, data } });
  const records = [];
  for (let i = 0; i < (size === 'long' ? 48 : 2); i++) {
    records.push({ t: 'user', text: `seed request ${i}`, ...(size === 'long' && i === 0 ? { images: historyImages } : {}) });
    records.push(event('tool_call_started', { id: `seed-${i}`, title: 'read_file sample.txt', kind: 'read', status: 'in_progress' }));
    records.push(event('tool_call_updated', {
      id: `seed-${i}`, status: 'completed',
      raw_output: `seed-result-${i}\n` + 'text with a quote " and a slash \\.\n'.repeat(16),
      output: 'seed output preview',
    }));
    records.push(event('agent_message_chunk', { text: `seed answer ${i}` }));
    records.push(event('turn_ended', { stop_reason: 'end_turn' }));
  }
  if (loadedTools > 0) records.push({
    t: 'tool_loadout', names: Array.from({ length: loadedTools }, (_, index) => `mcp__bench__schema_${String(index).padStart(4, '0')}`),
  });
  return records.map(record => JSON.stringify(record)).join('\n') + '\n';
}
function response(protocol, index) {
  const tool = index % 3 < 2;
  const id = `desktop-call-${index}`;
  const parameters = JSON.stringify({ path: index % 3 === 0 ? 'sample.png' : 'sample.txt' });
  let events;
  if (protocol === 'anthropic') {
    events = [
      { type: 'message_start', message: { usage: { input_tokens: 100 } } },
      ...(tool ? [
        { type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: 'inspect fixture', signature: 'desktop-signature' } },
        { type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id, name: 'read_file', input: {} } },
        { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: parameters } },
      ] : [
        { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
        { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: `desktop complete ${Math.floor(index / 3)}` } },
      ]),
      { type: 'message_delta', delta: { stop_reason: tool ? 'tool_use' : 'end_turn' }, usage: { output_tokens: 10 } },
      { type: 'message_stop' },
    ];
  } else if (protocol === 'responses') {
    events = [
      ...(tool ? [
        { type: 'response.output_item.added', item: { type: 'function_call', id, call_id: id, name: 'read_file', arguments: parameters } },
        { type: 'response.output_item.done', item: { type: 'function_call', id } },
      ] : [{ type: 'response.output_text.delta', delta: `desktop complete ${Math.floor(index / 3)}` }]),
      { type: 'response.completed', response: { status: 'completed', usage: { input_tokens: 100, output_tokens: 10 } } },
    ];
  } else {
    events = [
      { choices: [{ index: 0, delta: tool ? { tool_calls: [{ index: 0, id, type: 'function', function: { name: 'read_file', arguments: parameters } }] } : { content: `desktop complete ${Math.floor(index / 3)}` }, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }] },
      { choices: [], usage: { prompt_tokens: 100, completion_tokens: 10 } },
    ];
  }
  return events.map(event => `data: ${JSON.stringify(event)}\n\n`).join('') + 'data: [DONE]\n\n';
}
const active = new Map();
const server = createServer(highWaterMark === null ? {} : { highWaterMark }, async (request, reply) => {
  try {
    const run = active.get(request.url.split('/')[2]);
    assert(run, request.url);
    if (request.method === 'GET') {
      reply.setHeader('content-type', 'application/json');
      reply.end(JSON.stringify({ data: [{ id: 'request-bench' }] }));
      return;
    }
    const { buffer: receiveBuffer, length, observation: observed } = await observer.receive(request, receiver);
    observed.read_high_water_mark = request.readableHighWaterMark;
    observed.socket_receive_buffer = receiveBufferOf(request.socket);
    const main = receiveBuffer.subarray(0, length).includes(toolsKey);
    if (main && run.size === 'long') assert(length >= 10 * 1024 * 1024, 'Long history was not present in the actual request.');
    const index = main ? run.requests.length : 2;
    (main ? run.requests : run.side_requests).push(observed);
    observer.inspect(receiveBuffer, length, observed, main,
      main && index > 0 ? {
        imageRgba: [100, 120, 140, 255],
        ...(run.protocol === 'anthropic' ? { signature: 'desktop-signature' } : {}),
      } : undefined);
    await delay(40);
    const body = response(run.protocol, index);
    observed.response_hash = hash(body);
    observed.response_at_ms = Date.now();
    reply.writeHead(200, { 'content-type': 'text/event-stream', 'content-length': Buffer.byteLength(body) });
    reply.end(body);
  } catch (error) {
    reply.writeHead(500, { 'content-type': 'application/json' });
    reply.end(JSON.stringify({ error: { message: String(error) } }));
  }
});
const receiveBufferOf = configureReceiveBuffer(server, receiveBuffer, args['tcp-buffer-helper']);
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
async function waitFor(read, label, timeout = Number(args.wait || 90_000)) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await read();
    if (value) return value;
    await delay(40);
  }
  throw new Error(`Timed out: ${label}`);
}
const json = file => readFile(file, 'utf8').then(JSON.parse).catch(() => null);
async function writeControl(file, value) {
  const temporary = `${file}.tmp`;
  await writeFile(temporary, value);
  for (let attempt = 0; ; attempt++) {
    try { await rename(temporary, file); return; }
    catch (error) {
      if (!['EBUSY', 'EPERM', 'EACCES'].includes(error.code) || attempt >= 50) throw error;
      await delay(10);
    }
  }
}
const runs = [];
async function run(version, round, protocol, size, save) {
  if (observer) await observer.reset();
  else observer = new GatewayObserver(cwd, observationMode);
  const id = `${protocol}-${size}-${save}-${round}-${version}`;
  const directory = path.join(output, id);
  const data = args['data-in-output'] === '1' ? path.join(directory, 'runtime-data') : path.join(fixture, id);
  const sessions = path.join(data, 'sessions');
  await mkdir(directory);
  await mkdir(sessions, { recursive: true });
  const seeded = seed(size);
  const sessionFile = path.join(sessions, 'gateway-fixture.jsonl');
  await writeFile(sessionFile, seeded);
  await writeFile(path.join(sessions, 'gateway-fixture.json'), JSON.stringify({
    id: 'gateway-fixture', title: 'Gateway fixture', cwd, agent: 'builtin:fixture',
    kernel_session_id: null, created_ms: 1794780000000, updated_ms: 1794780000000,
    mode: 'bypassPermissions',
  }));
  await writeFile(path.join(data, 'settings.json'), JSON.stringify({
    notifications: false, suggestions: false, memory_suggest: false,
    gateway_bodies: save === 'on',
  }));
  if (mcpTools > 0) await writeFile(path.join(data, 'mcp.json'), JSON.stringify({ mcpServers: { bench: {
    command: process.execPath, args: [path.join(root, 'tools/mock-mcp-server.mjs')],
    env: { MOCK_MCP_SCHEMA_TOOLS: String(mcpTools), MOCK_MCP_SCHEMA_FIELDS: '64' },
  } } }));
  const state = { protocol, size, requests: [], side_requests: [] };
  active.set(id, state);
  const stdout = createWriteStream(path.join(directory, 'stdout.log'));
  const stderr = createWriteStream(path.join(directory, 'stderr.log'));
  const child = spawn(versions[version], [], {
    cwd: root, windowsHide: true,
    env: {
      ...cleanEnv, CLAUDESKTOP_HEADLESS: '1', CLAUDESKTOP_DATA_DIR: data,
      CLAUDESKTOP_KERNEL: 'builtin', CLAUDESKTOP_BASE_URL: `http://127.0.0.1:${server.address().port}/run/${id}/v1`,
      CLAUDESKTOP_MODEL: 'request-bench', CLAUDESKTOP_PROTOCOL: protocol, CLAUDESKTOP_API_KEY: 'local-test-only',
      CLAUDESKTOP_GATEWAY: '1', CLAUDESKTOP_AUTODEMO: '1', CLAUDESKTOP_AUTODEMO_SESSION: '1',
      CLAUDESKTOP_AUTOPROMPT: 'benchmark turn 0', ACP_CWD: cwd,
      CLAUDESKTOP_BENCH_STARTUP: '1',
      ...(args.profile === '1' ? { CLAUDESKTOP_REQUEST_PROFILE: path.join(directory, 'profile.jsonl') } : {}),
    }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.pipe(stdout); child.stderr.pipe(stderr);
  const closed = new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
  const driverLog = createWriteStream(path.join(directory, 'driver.log'));
  const driver = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(root, 'tools/gateway-desktop-driver.ps1'), '-ProcessId', String(child.pid), '-Directory', directory, '-Layout', windowLayout], {
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
  });
  driver.stdout.pipe(driverLog); driver.stderr.pipe(driverLog);
  const driverClosed = new Promise((resolve, reject) => { driver.once('error', reject); driver.once('exit', resolve); });
  let commandId = 0;
  async function command(action, text) {
    const sent = { id: ++commandId, action, text };
    await writeControl(path.join(directory, 'command.json'), JSON.stringify(sent));
    return waitFor(async () => { const answer = await json(path.join(directory, 'response.json')); return answer?.id === sent.id ? answer : null; }, `window ${action}`, 15_000);
  }
  async function logs() {
    const gateway = path.join(data, 'gateway');
    const files = await readdir(gateway).catch(() => []);
    return (await Promise.all(files.filter(name => name.endsWith('.jsonl')).map(name => readFile(path.join(gateway, name), 'utf8')))).join('\n').split(/\r?\n/).filter(Boolean).map(JSON.parse).sort((a, b) => a.at_ms - b.at_ms || (a.req_id ?? 0) - (b.req_id ?? 0));
  }
  const sends = [];
  const completions = [];
  let productAffinity = null;
  let gatewayLogs;
  try {
    await waitFor(() => json(path.join(directory, 'ready.json')), 'hidden window');
    await command('status');
    for (let turn = 0; turn <= turns; turn++) {
      await writeControl(path.join(directory, 'phase.txt'), turn === 0 ? 'startup' : 'active');
      if (turn > 0) sends.push(await command('send', `benchmark turn ${turn}`));
      gatewayLogs = await waitFor(async () => {
        const records = await logs();
        const count = records.filter(log => log.purpose === 'turn').length;
        assert(count <= (turn + 1) * 3, `Unexpected request count: ${count}`);
        if (count < (turn + 1) * 3) return null;
        const file = await open(sessionFile, 'r');
        let transcript;
        try {
          const offset = Buffer.byteLength(seeded);
          const tail = Buffer.alloc(Math.max(0, (await file.stat()).size - offset));
          const { bytesRead } = await file.read(tail, 0, tail.length, offset);
          transcript = tail.subarray(0, bytesRead).toString('utf8');
        } finally { await file.close(); }
        const recordsAfterSeed = transcript.split(/\r?\n/).filter(Boolean).map(JSON.parse);
        assert(!recordsAfterSeed.some(record => record.event?.type === 'prompt_failed'), transcript.slice(-2000));
        const ended = recordsAfterSeed.filter(record => record.event?.type === 'turn_ended').length;
        if (ended !== turn + 1) return null;
        const completedAt = recordsAfterSeed.findLast(record => record.t === 'turn')?.at_ms;
        assert(Number.isFinite(completedAt), 'No turn completion timestamp');
        completions.push(completedAt);
        return records;
      }, `desktop turn ${turn}`);
      if (turn === 0) {
        await waitFor(async () => state.side_requests.length === 1 && (await logs()).some(log => log.purpose === 'title'), 'title request');
        if (mcpTools > 0) await waitFor(async () =>
          (await readFile(path.join(directory, 'stderr.log'), 'utf8')).includes('[perf] mcp_connect:bench:'),
        'large MCP connection');
        if (args['product-affinity']) productAffinity = await setAffinity(child.pid, args['product-affinity']);
      }
      await command('status');
      await writeControl(path.join(directory, 'phase.txt'), 'idle');
      if (observationMode === 'idle') await observer.finish();
      await delay(100);
    }
    await command('status');
    await command('close');
    assert.equal(await Promise.race([closed, delay(10_000).then(() => 'timeout')]), 0);
    assert.equal(await driverClosed, 0, 'Inspect driver.log');
  } catch (error) {
    await writeFile(path.join(directory, 'error.txt'), String(error.stack || error));
    await cp(data, path.join(directory, 'failed-data'), { recursive: true });
    await writeFile(path.join(directory, 'requests.json'), JSON.stringify(state.requests, null, 2));
    throw error;
  } finally {
    if (child.exitCode === null) child.kill();
    if (driver.exitCode === null) driver.kill();
    await Promise.race([Promise.allSettled([closed, driverClosed]), delay(5000)]);
    active.delete(id);
    await Promise.all([stdout, stderr, driverLog].map(stream => new Promise(resolve => stream.end(resolve))));
  }
  await observer.finish();
  await writeFile(path.join(directory, 'gateway-logs.json'), JSON.stringify(gatewayLogs, null, 2));
  await writeFile(path.join(directory, 'requests.json'), JSON.stringify(state.requests, null, 2));
  const measuredLogs = gatewayLogs.filter(log => log.purpose === 'turn');
  assert.equal(state.requests.length, (turns + 1) * 3);
  assert.equal(measuredLogs.length, (turns + 1) * 3);
  assert.equal(sends.length, turns);
  assert(state.requests.slice(3).every(request => request.stats.mcp_tools === loadedTools), 'Wrong desktop MCP loadout');
  for (const [index, log] of measuredLogs.entries()) {
    const observed = state.requests[index];
    assert.equal(log.model, observed.stats.model);
    assert.equal(log.messages ?? null, observed.stats.messages);
    assert.equal(log.tools ?? null, observed.stats.tools);
    assert.equal(log.body_bytes, observed.bytes);
    assert.equal(log.protocol, protocol);
    assert.equal(log.status, 200);
    assert.equal(log.input, 100); assert.equal(log.output, 10);
    if (save === 'on') {
      const base = path.join(data, 'gateway', log.body);
      assert.equal(hash(gunzipSync(await readFile(`${base}.req.json.gz`))), observed.raw_hash);
      assert.equal(hash(gunzipSync(await readFile(`${base}.resp.gz`))), observed.response_hash);
    } else assert.equal(log.body, undefined);
  }
  let profile = [];
  if (args.profile === '1') {
    profile = (await readFile(path.join(directory, 'profile.jsonl'), 'utf8')).trim().split(/\r?\n/).filter(Boolean).map(JSON.parse);
    assert(profile.length > 0, 'Diagnostic executable did not emit phase records.');
    assert(profile.every(record => Number.isFinite(record.start_us) && Number.isFinite(record.elapsed_us) && record.elapsed_us >= 0));
    const completedRequests = new Set(profile.filter(record => record.phase === 'gateway_write').map(record => record.request));
    assert(measuredLogs.every(log => completedRequests.has(log.req_id)), 'A gateway request has no completed phase record.');
    if (versions.candidate) assert(profile.every(record => record.variant === (args['profile-variant'] || version)), 'Diagnostic executable has the wrong variant.');
  }
  const rows = (await readFile(path.join(directory, 'memory.csv'), 'utf8')).trim().split(/\r?\n/);
  const header = rows.shift().split(',');
  const samples = rows.map(row => Object.fromEntries(row.split(',').map((value, index) => [header[index], index === 1 ? value : Number(value)])));
  const activeSamples = samples.filter(row => row.phase === 'active');
  assert(activeSamples.length > 0);
  const result = {
    version, round, protocol, size, save, mcp_tools: mcpTools, loaded_tools: loadedTools, pid: child.pid,
    warmup_turns: 1, measured_turns: turns,
    profile_records: profile.length,
    profile_variant: profile[0]?.variant ?? null,
    diagnostic_data_in_output: args['data-in-output'] === '1',
    sends, completions,
    history_log_bytes: Buffer.byteLength(seeded),
    peak_pb: Math.max(...samples.map(row => row.peak_pb)),
    peak_ws: Math.max(...samples.map(row => row.peak_ws)),
    active_pb: Math.max(...activeSamples.map(row => row.pb)),
    active_ws: Math.max(...activeSamples.map(row => row.ws)),
    peak_paged_pool: Math.max(...samples.map(row => row.peak_paged_pool)),
    peak_nonpaged_pool: Math.max(...samples.map(row => row.peak_nonpaged_pool)),
    active_nonpaged_pool: Math.max(...activeSamples.map(row => row.nonpaged_pool)),
    cpu_ms: samples.at(-1).cpu_ms - samples[0].cpu_ms,
    send_p95_ms: percentile(sends.map((sent, index) => state.requests[(index + 1) * 3].at_ms - sent.sent_at_ms), .95),
    turn_p95_ms: percentile(sends.map((sent, index) => completions[index + 1] - sent.sent_at_ms), .95),
    max_sample_gap_ms: Math.max(...samples.slice(1).map((row, index) => row.epoch_ms - samples[index].epoch_ms)),
    hidden: true, foreground: false, control_affinity: controlAffinity, product_affinity: productAffinity, side_requests: state.side_requests,
    verified_statistics: measuredLogs.length,
    verified_saved_bodies: save === 'on' ? measuredLogs.length : 0,
    requests: state.requests,
  };
  await writeFile(path.join(directory, 'gateway-logs.json'), JSON.stringify(gatewayLogs, null, 2));
  await writeFile(path.join(directory, 'result.json'), JSON.stringify(result, null, 2));
  await writeFile(path.join(directory, 'requests.json'), JSON.stringify(state.requests, null, 2));
  console.log(JSON.stringify({ id, peak_mib: result.peak_pb / 1048576, active_mib: result.active_pb / 1048576, cpu_ms: result.cpu_ms, send_p95_ms: result.send_p95_ms }));
  await rm(data, { recursive: true, force: true });
  return result;
}
try {
  for (let round = 1; round <= rounds; round++) for (const protocol of protocols) for (const size of sizes) for (const save of saves) {
    const order = Object.keys(versions);
    if (round % 2 === 0) order.reverse();
    for (const version of order) {
      runs.push(await run(version, round, protocol, size, save));
      await writeFile(path.join(output, 'runs.json'), JSON.stringify(runs, null, 2));
      if (globalThis.gc) {
        await new Promise(resolve => setImmediate(resolve));
        globalThis.gc();
        await new Promise(resolve => setImmediate(resolve));
        globalThis.gc();
      }
    }
  }
  const pairs = [];
  if (versions.candidate) for (const baseline of runs.filter(run => run.version === 'baseline')) {
    const candidate = runs.find(run => run.version === 'candidate' && run.round === baseline.round && run.protocol === baseline.protocol && run.size === baseline.size && run.save === baseline.save);
    const pair = {
      round: baseline.round, protocol: baseline.protocol, size: baseline.size, save: baseline.save,
      identical_request_bytes: baseline.requests.every((request, index) => request.raw_hash === candidate.requests[index].raw_hash),
      peak_saved_mib: (baseline.peak_pb - candidate.peak_pb) / 1048576,
      active_saved_mib: (baseline.active_pb - candidate.active_pb) / 1048576,
      peak_ratio: candidate.peak_pb / baseline.peak_pb,
      extra_send_p95_ms: candidate.send_p95_ms - baseline.send_p95_ms,
      extra_turn_p95_ms: candidate.turn_p95_ms - baseline.turn_p95_ms,
      cpu_saved_ms: baseline.cpu_ms - candidate.cpu_ms,
    };
    pair.passed = pairPassed(pair, comparison);
    pairs.push(pair);
  }
  const summary = { comparison, executable_sha256: executableHashes, versions, rounds, turns, mcp_tools: mcpTools, loaded_tools: loadedTools, window_send_method: `button-${windowLayout}`, profiling: args.profile === '1', receiver, high_water_mark: highWaterMark, endpoint_receive_buffer: receiveBuffer, controller_timer_ms: controllerTimer, tcp_buffer_helper: args['tcp-buffer-helper'] ?? null, observer: `pooled-${observationMode}`, control_affinity: controlAffinity, requested_product_affinity: args['product-affinity'] ?? null, observer_gc: typeof globalThis.gc === 'function', runs, pairs, passed: versions.candidate ? pairs.every(pair => pair.passed) : null };
  await writeFile(path.join(output, 'summary.json'), JSON.stringify(summary, null, 2));
  console.log(JSON.stringify({ passed: summary.passed, pairs }, null, 2));
  if (summary.passed === false) process.exitCode = 1;
} finally {
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
  await observer?.close();
  await rm(fixture, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
}
