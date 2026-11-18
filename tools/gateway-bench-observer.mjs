import { Worker } from 'node:worker_threads';
import { createRequire } from 'node:module';
import { createReadStream } from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';
import assert from 'node:assert/strict';

export async function sha256File(file) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}

export async function executableComparison(requested, versions) {
  const comparison = requested ?? 'ab';
  assert(['aa', 'ab'].includes(comparison), 'Use --comparison=aa or --comparison=ab.');
  const hashes = Object.fromEntries(await Promise.all(Object.entries(versions).map(async ([name, file]) => [name, await sha256File(file)])));
  if (comparison === 'aa') {
    assert(versions.candidate, 'A/A requires both executable arguments.');
    assert.equal(hashes.baseline, hashes.candidate, 'A/A requires byte-identical executables.');
  }
  return { comparison, hashes };
}

export function pairPassed(pair, comparison, { requireLongMemory = false } = {}) {
  assert(['aa', 'ab'].includes(comparison));
  if (!pair.identical_request_bytes || !Number.isFinite(pair.extra_send_p95_ms) || !Number.isFinite(pair.extra_turn_p95_ms)) return false;
  if (comparison === 'aa') return Math.abs(pair.extra_send_p95_ms) <= 20 && Math.abs(pair.extra_turn_p95_ms) <= 20;
  return pair.peak_ratio <= 1.1 && pair.extra_send_p95_ms <= 20 && pair.extra_turn_p95_ms <= 20
    && (!requireLongMemory || pair.size === 'short' || pair.peak_saved_mib > 0);
}

export function configureReceiveBuffer(server, bytes, helper) {
  const observed = new WeakMap();
  if (bytes === null) return socket => observed.get(socket) ?? null;
  assert.equal(process.platform, 'win32');
  assert(Number.isInteger(bytes) && bytes >= 0 && bytes <= 4 * 1024 * 1024);
  assert(helper, 'Specify the native test helper with --tcp-buffer-helper.');
  const { configure } = createRequire(import.meta.url)(path.resolve(helper));
  server.on('connection', socket => {
    assert.equal(socket.localAddress, '127.0.0.1');
    assert.equal(socket.remoteAddress, '127.0.0.1');
    const before = configure(socket.localPort, socket.remotePort, 0);
    const after = configure(socket.localPort, socket.remotePort, bytes);
    assert(after >= bytes);
    observed.set(socket, { before, after });
  });
  return socket => observed.get(socket) ?? null;
}

export function configureBenchmarkClock(requested, helper) {
  if (requested === undefined) return null;
  assert.equal(requested, '1', 'Only --precise-timers=1 is supported.');
  assert.equal(process.platform, 'win32');
  assert(helper, 'Specify the native test helper with --tcp-buffer-helper.');
  const resolution = createRequire(import.meta.url)(path.resolve(helper)).preciseTimers();
  assert.equal(resolution, 1);
  return resolution;
}

export class GatewayObserver {
  #worker;
  #buffers;
  #pending = new Map();
  #queued = [];
  #next = 0;
  errors = [];
  checks = [];

  constructor(cwd, mode = 'background', { rawOnly = true } = {}) {
    assert(['background', 'idle'].includes(mode));
    this.cwd = cwd;
    this.mode = mode;
    this.rawOnly = rawOnly;
    this.poolSize = mode === 'idle' ? 4 : 3;
    this.#buffers = Array.from({ length: this.poolSize }, () => Buffer.allocUnsafe(32 * 1024 * 1024));
    if (mode === 'background') this.#start();
  }

  #start() {
    this.#worker = new Worker(new URL('./request-memory-worker.mjs', import.meta.url), {
      execArgv: process.execArgv.filter(argument => argument !== '--expose-gc'),
    });
    this.#worker.on('message', ({ id, error, buffer, ...result }) => {
      if (buffer) this.#buffers.push(Buffer.from(buffer));
      const job = this.#pending.get(id);
      this.#pending.delete(id);
      if (error) job.reject(new Error(error)); else job.resolve(result);
    });
    this.#worker.on('error', error => {
      for (const job of this.#pending.values()) job.reject(error);
      this.#pending.clear();
    });
  }

  acquire() {
    const buffer = this.#buffers.pop();
    assert(buffer, 'Validation fell behind: receive buffer pool exhausted.');
    return buffer;
  }

  async receive(request, receiver) {
    const receiveStartedAt = Date.now();
    const buffer = this.acquire();
    let length = 0;
    let chunks = 0;
    let lastChunkAt = performance.now();
    let maximumGap = 0;
    const append = chunk => {
      assert(length + chunk.length <= buffer.length);
      const now = performance.now();
      maximumGap = Math.max(maximumGap, now - lastChunkAt);
      lastChunkAt = now;
      chunk.copy(buffer, length);
      length += chunk.length;
      chunks++;
    };
    if (receiver === 'iterator') {
      for await (const chunk of request) append(chunk);
    } else {
      await new Promise((resolve, reject) => {
        const cleanup = () => {
          request.off('data', data);
          request.off('end', end);
          request.off('error', error);
          request.off('aborted', aborted);
        };
        const end = () => { cleanup(); resolve(); };
        const error = reason => { cleanup(); reject(reason); };
        const aborted = () => error(new Error('Request body was aborted.'));
        const data = chunk => {
          try { append(chunk); }
          catch (reason) { error(reason); request.destroy(); }
        };
        request.on('data', data);
        request.once('end', end);
        request.once('error', error);
        request.once('aborted', aborted);
      });
    }
    return {
      buffer, length,
      observation: {
        at_ms: Date.now(), receive_started_at_ms: receiveStartedAt,
        bytes: length, receive_chunks: chunks, maximum_receive_gap_ms: maximumGap,
      },
    };
  }

  inspect(buffer, length, observation, main, expectations) {
    if (this.mode === 'idle') {
      this.#queued.push({ buffer, length, observation, main, expectations });
    } else {
      this.#inspect(buffer, length, observation, main, expectations);
    }
  }

  #inspect(buffer, length, observation, main, expectations) {
    const id = ++this.#next;
    const bytes = buffer.subarray(0, length);
    const promise = new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#worker.postMessage({ id, bytes, cwd: this.cwd, raw_only: this.rawOnly, return_buffer: true, expectations }, [bytes.buffer]);
    }).then(result => {
      if (main !== undefined) assert.equal(result.stats.tools !== null, main, 'Fixture request kind differs from full JSON validation.');
      Object.assign(observation, result);
    }).catch(error => { this.errors.push(String(error)); });
    this.checks.push(promise);
  }

  async finish() {
    if (this.#queued.length) {
      this.#start();
      for (const job of this.#queued.splice(0)) {
        this.#inspect(job.buffer, job.length, job.observation, job.main, job.expectations);
      }
    }
    try {
      await Promise.all(this.checks);
      assert.deepEqual(this.errors, [], 'Request validation failed.');
      assert.equal(this.#pending.size, 0);
      assert.equal(this.#buffers.length, this.poolSize);
    } finally {
      if (this.mode === 'idle') await this.close();
    }
  }

  async reset() {
    assert.equal(this.#pending.size, 0);
    assert.equal(this.#queued.length, 0);
    assert.equal(this.#buffers.length, this.poolSize);
    await this.close();
    this.errors = [];
    this.checks = [];
    if (this.mode === 'background') this.#start();
  }

  async close() {
    await this.#worker?.terminate();
    this.#worker = undefined;
  }
}
