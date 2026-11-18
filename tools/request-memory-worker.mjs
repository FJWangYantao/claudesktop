import { parentPort } from 'node:worker_threads';
import { createHash } from 'node:crypto';
import { inflateSync } from 'node:zlib';

function fixturePng(data, expected) {
  const header = Buffer.from(data.slice(0, 48), 'base64');
  if (header.length < 26 || header.subarray(0, 8).toString('hex') !== '89504e470d0a1a0a'
    || header.readUInt32BE(16) !== 1 || header.readUInt32BE(20) !== 1 || header[24] !== 8) return false;
  const bytes = Buffer.from(data, 'base64');
  const chunks = [];
  for (let offset = 8; offset + 12 <= bytes.length;) {
    const length = bytes.readUInt32BE(offset);
    if (offset + 12 + length > bytes.length) return false;
    if (bytes.toString('ascii', offset + 4, offset + 8) === 'IDAT') chunks.push(bytes.subarray(offset + 8, offset + 8 + length));
    offset += length + 12;
  }
  const pixels = inflateSync(Buffer.concat(chunks));
  const channels = header[25] === 6 ? 4 : header[25] === 2 ? 3 : 0;
  if (!channels || pixels.length !== channels + 1 || pixels[0] > 4) return false;
  const rgba = [...pixels.subarray(1)];
  if (channels === 3) rgba.push(255);
  return rgba.every((value, index) => value === expected[index]);
}

function hasFixtureImage(value, expected) {
  if (Array.isArray(value)) return value.some(item => hasFixtureImage(item, expected));
  if (!value || typeof value !== 'object') return false;
  const url = value.type === 'image_url' ? value.image_url?.url : value.type === 'input_image' ? value.image_url : null;
  const data = typeof url === 'string' && url.startsWith('data:image/png;base64,')
    ? url.slice('data:image/png;base64,'.length)
    : value.type === 'image' && value.source?.media_type === 'image/png' ? value.source.data : null;
  if (typeof data === 'string' && fixturePng(data, expected)) return true;
  return Object.values(value).some(item => hasFixtureImage(item, expected));
}

function normalize(value, cwd) {
  if (typeof value === 'string') return value.replaceAll(cwd, '<cwd>').replaceAll(cwd.replaceAll('\\', '/'), '<cwd>');
  if (Array.isArray(value)) return value.map(item => normalize(item, cwd));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.keys(value).sort().map(key => [key, normalize(value[key], cwd)]));
  }
  return value;
}

parentPort.on('message', ({ id, bytes, cwd, raw_only = false, return_buffer = false, expectations }) => {
  const send = result => return_buffer
    ? parentPort.postMessage({ ...result, buffer: bytes.buffer }, [bytes.buffer])
    : parentPort.postMessage(result);
  try {
    const raw = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const body = JSON.parse(raw);
    const payloadChecks = {};
    if (expectations?.imageRgba) {
      if (!hasFixtureImage(body.messages ?? body.input, expectations.imageRgba)) throw new Error('Tool image pixels did not reach the model.');
      payloadChecks.image = true;
    }
    if (expectations?.signature) {
      if (!raw.includes(`"signature":${JSON.stringify(expectations.signature)}`)) throw new Error('Thinking signature did not reach the model.');
      payloadChecks.signature = true;
    }
    const hash = createHash('sha256').update(raw_only ? bytes : JSON.stringify(normalize(body, cwd))).digest('hex');
    send({
      id, hash, ...(expectations ? { payload_checks: payloadChecks } : {}),
      stats: {
        model: typeof body.model === 'string' ? body.model : null,
        messages: Array.isArray(body.messages) ? body.messages.length : !Object.hasOwn(body, 'messages') && Array.isArray(body.input) ? body.input.length : null,
        tools: Array.isArray(body.tools) ? body.tools.length : null,
        mcp_tools: Array.isArray(body.tools) ? body.tools.filter(tool => {
          const name = tool?.function?.name ?? tool?.name;
          return typeof name === 'string' && name.startsWith('mcp__');
        }).length : 0,
      },
      ...(raw_only ? { raw_hash: hash } : {
        normalized_raw_hash: createHash('sha256').update(
          Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength).toString('utf8')
            .replaceAll(JSON.stringify(cwd).slice(1, -1), '<cwd>')
            .replaceAll(JSON.stringify(cwd.replaceAll('\\', '/')).slice(1, -1), '<cwd>'),
        ).digest('hex'),
      }),
    });
  } catch (error) {
    send({ id, error: String(error) });
  }
});
