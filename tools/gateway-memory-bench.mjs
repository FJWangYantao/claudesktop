import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { GatewayObserver, configureReceiveBuffer, configureBenchmarkClock, executableComparison, pairPassed } from './gateway-bench-observer.mjs';
import { mkdir, mkdtemp, readFile, writeFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { createWriteStream } from 'node:fs';
import { createHash } from 'node:crypto';
import { gunzipSync } from 'node:zlib';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = Object.fromEntries(process.argv.slice(2).map(arg => {
  const [key, ...value] = arg.replace(/^--/, '').split('=');
  return [key, value.join('=')];
}));
const controllerTimer = configureBenchmarkClock(args['precise-timers'], args['tcp-buffer-helper']);
const output = path.resolve(args.out || path.join(root, '.claudesktop/bench/gateway-memory/runs'));
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
const sizes = (args.sizes || 'short,long').split(',');
const saves = (args.save || 'on,off').split(',');
const receiver = args.receiver || 'iterator';
const observationMode = args.observer || 'background';
const highWaterMark = args['high-water-mark'] ? Number(args['high-water-mark']) : null;
const receiveBuffer = args['receive-buffer'] === undefined ? null : Number(args['receive-buffer']);
assert(highWaterMark === null || (Number.isInteger(highWaterMark) && highWaterMark >= 16384 && highWaterMark <= 4 * 1024 * 1024));
assert(['events', 'iterator'].includes(receiver));
assert(['background', 'idle'].includes(observationMode));
assert.equal(process.platform, 'win32');
assert(Number.isInteger(rounds) && rounds > 0);
assert(Number.isInteger(turns) && turns >= 20);
assert(protocols.every(name => ['openai', 'anthropic', 'responses'].includes(name)));
assert(sizes.every(name => ['short', 'long'].includes(name)));
assert(saves.every(name => ['on', 'off'].includes(name)));
const percentile = (values, p) => {
  const sorted = values.toSorted((a, b) => a - b);
  assert(sorted.length > 0);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * p) - 1)];
};
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const fixture = await mkdtemp(path.join(tmpdir(), 'claudesktop-gateway-memory-'));
const cwd = path.join(fixture, 'cwd');
const active = new Map();
const runs = [];
let observer;

function response(protocol, index) {
  const tool = index % 3 < 2;
  const id = `bench-call-${index}`;
  const parameters = JSON.stringify({ path: 'sample.txt' });
  let events;
  if (protocol === 'openai') {
    const delta = tool
      ? { tool_calls: [{ index: 0, id, type: 'function', function: { name: 'read_file', arguments: parameters } }] }
      : { content: 'benchmark complete' };
    events = [
      { choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }] },
      { choices: [], usage: { prompt_tokens: 100, completion_tokens: 10 } },
    ];
  } else if (protocol === 'anthropic') {
    const blocks = tool ? [
      { type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: 'request thought', signature: 'request-signature' } },
      { type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id, name: 'read_file', input: {} } },
      { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: parameters } },
    ] : [
      { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
      { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'benchmark complete' } },
    ];
    events = [
      { type: 'message_start', message: { usage: { input_tokens: 100 } } }, ...blocks,
      { type: 'message_delta', delta: { stop_reason: tool ? 'tool_use' : 'end_turn' }, usage: { output_tokens: 10 } },
      { type: 'message_stop' },
    ];
  } else {
    events = [
      ...(tool ? [
        { type: 'response.output_item.added', item: { type: 'function_call', id, call_id: id, name: 'read_file', arguments: parameters } },
        { type: 'response.output_item.done', item: { type: 'function_call', id } },
      ] : [{ type: 'response.output_text.delta', delta: 'benchmark complete' }]),
      { type: 'response.completed', response: { status: 'completed', usage: { input_tokens: 100, output_tokens: 10 } } },
    ];
  }
  return events.map(event => `data: ${JSON.stringify(event)}\n\n`).join('') + 'data: [DONE]\n\n';
}

let receiving = false;
const server = createServer(highWaterMark === null ? {} : { highWaterMark }, async (request, reply) => {
  try {
    const id = request.url.split('/')[2];
    const run = active.get(id);
    assert(run, request.url);
    assert(!receiving, 'The fixture expects sequential requests.');
    assert(!Object.keys(request.headers).some(name => name.startsWith('x-claudesktop-')));
    receiving = true;
    let received;
    try { received = await observer.receive(request, receiver); }
    finally { receiving = false; }
    const { buffer: receiveBuffer, length, observation } = received;
    observation.read_high_water_mark = request.readableHighWaterMark;
    observation.socket_receive_buffer = receiveBufferOf(request.socket);
    const index = run.requests.length;
    run.requests.push(observation);
    observer.inspect(receiveBuffer, length, observation);
    await new Promise(resolve => setTimeout(resolve, 40));
    const body = response(run.protocol, index);
    observation.response_hash = hash(body);
    observation.response_at_ms = Date.now();
    reply.writeHead(200, { 'content-type': 'text/event-stream', 'content-length': Buffer.byteLength(body) });
    reply.end(body);
  } catch (error) {
    reply.writeHead(500, { 'content-type': 'application/json' });
    reply.end(JSON.stringify({ error: { message: String(error) } }));
  }
});
let connections = 0;
server.on('connection', () => { connections++; });
const receiveBufferOf = configureReceiveBuffer(server, receiveBuffer, args['tcp-buffer-helper']);
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

async function run(version, round, protocol, size, save) {
  if (observer) await observer.reset();
  else observer = new GatewayObserver(cwd, observationMode);
  const id = `${protocol}-${size}-${save}-${round}-${version}`;
  const directory = path.join(output, id);
  const isolated = path.join(fixture, id);
  await mkdir(directory);
  const state = { protocol, requests: [] };
  const initialConnections = connections;
  active.set(id, state);
  const stdout = createWriteStream(path.join(directory, 'stdout.log'));
  const stderr = createWriteStream(path.join(directory, 'stderr.log'));
  const child = spawn(versions[version], ['--ignored', '--exact', 'request_memory::real_request_memory', '--nocapture', '--test-threads=1'], {
    cwd: root, windowsHide: true,
    env: {
      ...process.env,
      HTTP_PROXY: '', HTTPS_PROXY: '', ALL_PROXY: '', http_proxy: '', https_proxy: '', all_proxy: '',
      NO_PROXY: '127.0.0.1,localhost', no_proxy: '127.0.0.1,localhost',
      CLAUDESKTOP_DATA_DIR: path.join(isolated, 'data'),
      CLAUDESKTOP_REQUEST_BENCH_CWD: cwd,
      CLAUDESKTOP_REQUEST_BENCH_TURNS: String(turns),
      CLAUDESKTOP_REQUEST_BENCH_URL: `http://127.0.0.1:${server.address().port}/run/${id}/v1`,
      CLAUDESKTOP_REQUEST_BENCH_DIR: directory,
      CLAUDESKTOP_REQUEST_BENCH_PROTOCOL: protocol,
      CLAUDESKTOP_REQUEST_BENCH_SIZE: size,
      CLAUDESKTOP_REQUEST_BENCH_GATEWAY: '1',
      CLAUDESKTOP_REQUEST_BENCH_SAVE: save,
      CLAUDESKTOP_REQUEST_BENCH_OBSERVER: observationMode,
    }, stdio: [observationMode === 'idle' ? 'pipe' : 'ignore', 'pipe', 'pipe'],
  });
  child.stdout.pipe(stdout);
  child.stderr.pipe(stderr);
  const idleChecks = [];
  let idleCount = 0;
  let idleError;
  child.stdin?.on('error', error => { idleError = error; child.kill(); });
  const lines = createInterface({ input: child.stdout });
  lines.on('line', line => {
    const marker = /(?:^| )GATEWAY_BENCH_IDLE (\d+)$/.exec(line);
    if (!marker) return;
    const check = (async () => {
      assert.equal(observationMode, 'idle');
      assert.equal(Number(marker[1]), ++idleCount);
      await observer.finish();
      child.stdin.write('\n');
    })().catch(error => { idleError = error; child.kill(); });
    idleChecks.push(check);
  });
  const timeout = setTimeout(() => child.kill(), 150_000);
  let code;
  try {
    code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
  } finally {
    clearTimeout(timeout);
    active.delete(id);
    await Promise.all([stdout, stderr].map(stream => new Promise(resolve => stream.end(resolve))));
  }
  await Promise.all(idleChecks);
  lines.close();
  if (idleError) throw idleError;
  if (observationMode === 'idle') assert.equal(idleCount, turns);
  await observer.finish();
  await writeFile(path.join(directory, 'observed-requests.json'), JSON.stringify(state.requests, null, 2));
  assert.equal(code, 0, `${id}: see ${directory}`);
  assert.equal(state.requests.length, turns * 3);
  const metrics = JSON.parse(await readFile(path.join(directory, 'metrics.json'), 'utf8'));
  assert.equal(metrics.gateway_writes.length, turns * 3);
  assert.equal(metrics.gateway_logs.length, turns * 3);
  metrics.gateway_logs.sort((a, b) => a.at_ms - b.at_ms || (a.req_id ?? 0) - (b.req_id ?? 0));
  for (const [index, log] of metrics.gateway_logs.entries()) {
    const observation = state.requests[index];
    assert.equal(log.model, observation.stats.model);
    assert.equal(log.messages ?? null, observation.stats.messages);
    assert.equal(log.tools ?? null, observation.stats.tools);
    assert.equal(log.body_bytes, observation.bytes);
    assert.equal(log.session, metrics.session);
    assert.equal(log.turn, Math.floor(index / 3) + 1);
    assert.equal(log.purpose, 'turn');
    assert.equal(log.protocol, protocol);
    assert.equal(log.input, 100);
    assert.equal(log.output, 10);
    assert.equal(log.status, 200);
    if (save === 'on') {
      assert(log.body);
      const base = path.join(directory, 'gateway', log.body);
      assert.equal(hash(gunzipSync(await readFile(`${base}.req.json.gz`))), observation.raw_hash);
      assert.equal(hash(gunzipSync(await readFile(`${base}.resp.gz`))), observation.response_hash);
    } else {
      assert.equal(log.body, undefined);
    }
  }
  if (save === 'off') assert(!(await readdir(path.join(directory, 'gateway'))).includes('bodies'));
  const sending = metrics.samples.filter(sample => sample.phase % 2 === 1);
  const result = {
    version, round, protocol, size, save,
    tcp_connections: connections - initialConnections,
    history_bytes: metrics.history_bytes,
    before_pb: metrics.before.pb,
    peak_pb: metrics.after.peak_pb,
    peak_ws: metrics.after.peak_ws,
    active_pb: Math.max(...sending.map(sample => sample.pb)),
    after_pb: metrics.after.pb,
    after_ws: metrics.after.ws,
    cpu_ms: metrics.after.cpu_ms - metrics.before.cpu_ms,
    send_p95_ms: percentile(metrics.turns.map((turn, index) => state.requests[index * 3].at_ms - turn.sent_at_ms), .95),
    turn_p95_ms: percentile(metrics.turns.map(turn => turn.elapsed_ms), .95),
    parse_p95_ms: percentile(metrics.gateway_writes.map(write => write.parse_ms), .95),
    write_p95_ms: percentile(metrics.gateway_writes.map(write => write.elapsed_ms), .95),
    idle_growth_mib: (percentile(metrics.samples.filter(sample => sample.phase === turns * 2).map(sample => sample.pb), .5) - percentile(metrics.samples.filter(sample => sample.phase === 2).map(sample => sample.pb), .5)) / 1048576,
    requests: state.requests,
    verified_statistics: metrics.gateway_logs.length,
    verified_saved_bodies: save === 'on' ? metrics.gateway_logs.length : 0,
  };
  await writeFile(path.join(directory, 'result.json'), JSON.stringify(result, null, 2));
  console.log(JSON.stringify({ id, peak_mib: result.peak_pb / 1048576, cpu_ms: result.cpu_ms, send_p95_ms: result.send_p95_ms, parse_p95_ms: result.parse_p95_ms }));
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
    const same = baseline.requests.every((request, index) => request.raw_hash === candidate.requests[index].raw_hash);
    const pair = {
      round: baseline.round, protocol: baseline.protocol, size: baseline.size, save: baseline.save,
      identical_request_bytes: same,
      peak_saved_mib: (baseline.peak_pb - candidate.peak_pb) / 1048576,
      peak_ratio: candidate.peak_pb / baseline.peak_pb,
      extra_send_p95_ms: candidate.send_p95_ms - baseline.send_p95_ms,
      extra_turn_p95_ms: candidate.turn_p95_ms - baseline.turn_p95_ms,
      parse_p95_saved_ms: baseline.parse_p95_ms - candidate.parse_p95_ms,
      cpu_saved_ms: baseline.cpu_ms - candidate.cpu_ms,
    };
    pair.passed = pairPassed(pair, comparison, { requireLongMemory: true });
    pairs.push(pair);
  }
  const summary = { comparison, executable_sha256: executableHashes, versions, rounds, turns, receiver, high_water_mark: highWaterMark, endpoint_receive_buffer: receiveBuffer, controller_timer_ms: controllerTimer, tcp_buffer_helper: args['tcp-buffer-helper'] ?? null, observer: `pooled-${observationMode}`, observer_gc: typeof globalThis.gc === 'function', runs, pairs, passed: versions.candidate ? pairs.every(pair => pair.passed) : null };
  await writeFile(path.join(output, 'summary.json'), JSON.stringify(summary, null, 2));
  console.log(JSON.stringify({ passed: summary.passed, pairs }, null, 2));
  if (summary.passed === false) process.exitCode = 1;
} finally {
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
  await observer?.close();
  await rm(fixture, { recursive: true, force: true });
}
