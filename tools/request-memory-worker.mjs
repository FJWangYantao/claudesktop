import { parentPort } from 'node:worker_threads';
import { createHash } from 'node:crypto';

function normalize(value, cwd) {
  if (typeof value === 'string') return value.replaceAll(cwd, '<cwd>').replaceAll(cwd.replaceAll('\\', '/'), '<cwd>');
  if (Array.isArray(value)) return value.map(item => normalize(item, cwd));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.keys(value).sort().map(key => [key, normalize(value[key], cwd)]));
  }
  return value;
}

parentPort.on('message', ({ id, bytes, cwd, raw_only = false, return_buffer = false }) => {
  const send = result => return_buffer
    ? parentPort.postMessage({ ...result, buffer: bytes.buffer }, [bytes.buffer])
    : parentPort.postMessage(result);
  try {
    const body = JSON.parse(Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength));
    const hash = createHash('sha256').update(raw_only ? bytes : JSON.stringify(normalize(body, cwd))).digest('hex');
    send({
      id, hash,
      ...(raw_only ? {
        raw_hash: hash,
        stats: {
          model: typeof body.model === 'string' ? body.model : null,
          messages: Array.isArray(body.messages) ? body.messages.length : !Object.hasOwn(body, 'messages') && Array.isArray(body.input) ? body.input.length : null,
          tools: Array.isArray(body.tools) ? body.tools.length : null,
        },
      } : {}),
    });
  } catch (error) {
    send({ id, error: String(error) });
  }
});
