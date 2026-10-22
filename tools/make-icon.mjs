// 生成应用图标和界面里的品牌标志「小陶」（形象定义在 xiaotao.mjs）：
//   apps/desktop/resources/claudesktop.ico   exe 图标，16~256 每档按自己的细节级别单独栅格化
//   apps/desktop/resources/claudesktop.svg   图标原稿（256，纸底）
//   apps/desktop/assets/brand/xiaotao*.svg  界面里用的（无底；mascot.rs 的 brand_mark 内嵌）
//   apps/desktop/assets/brand/moods/*.svg    运行状态行会动的表情，每个状态几帧（mascot.rs 的 Mood）
// 用法：node tools/make-icon.mjs
// 需要 resvg 命令行：cargo install resvg --version 0.46.0 --locked（和 gpui 栅格化 SVG 的是同一版，图标和界面里看到的一致）
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { MOODS, STATUS_LAYOUT, xiaotao } from './xiaotao.mjs';

// 当前选定的组合，可选值见 xiaotao.mjs 的 SHAPES / FACES / GLAZES
const LOOK = { shape: 'round', face: 'capsuleBlush', glaze: 'dip' };

const desktop = join(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'desktop');
const detailFor = (px) => (px <= 32 ? 'small' : px < 64 ? 'mid' : 'full');

// 界面里：gpui 按 SVG 固有尺寸的 2 倍栅格化，固有尺寸写成常用的显示尺寸（侧栏 20、首页 40）
mkdirSync(join(desktop, 'assets', 'brand'), { recursive: true });
writeFileSync(join(desktop, 'assets', 'brand', 'xiaotao-small.svg'), xiaotao({ ...LOOK, bg: 'none', detail: 'small', size: 20 }));
writeFileSync(join(desktop, 'assets', 'brand', 'xiaotao.svg'), xiaotao({ ...LOOK, bg: 'none', detail: 'mid', size: 40 }));
writeFileSync(join(desktop, 'resources', 'claudesktop.svg'), xiaotao({ ...LOOK, bg: 'paper' }));
// 运行状态行：20px 的格子，身子缩小挪到左下，给头顶的道具让位置
mkdirSync(join(desktop, 'assets', 'brand', 'moods'), { recursive: true });
for (const [mood, { frames }] of Object.entries(MOODS)) {
  for (let frame = 0; frame < frames; frame++) {
    const svg = xiaotao({ ...LOOK, bg: 'none', detail: 'small', expr: mood, frame, size: 20, ...STATUS_LAYOUT });
    writeFileSync(join(desktop, 'assets', 'brand', 'moods', `${mood}-${frame}.svg`), svg);
  }
}

const tmp = mkdtempSync(join(tmpdir(), 'xiaotao-'));
try {
  const images = [16, 20, 24, 32, 40, 48, 64, 128, 256].map((px) => {
    const svg = join(tmp, `${px}.svg`);
    const png = join(tmp, `${px}.png`);
    writeFileSync(svg, xiaotao({ ...LOOK, bg: 'paper', detail: detailFor(px) }));
    execFileSync('resvg', ['-w', String(px), '-h', String(px), svg, png]);
    return { px, data: readFileSync(png) };
  });
  writeFileSync(join(desktop, 'resources', 'claudesktop.ico'), ico(images));
} finally {
  rmSync(tmp, { recursive: true, force: true });
}
console.log('ok');

// ico：6 字节文件头 + 每张 16 字节目录项 + PNG 原样拼在后面（Vista 起各尺寸都认 PNG）
function ico(images) {
  const head = Buffer.alloc(6 + 16 * images.length);
  head.writeUInt16LE(1, 2);
  head.writeUInt16LE(images.length, 4);
  let offset = head.length;
  images.forEach(({ px, data }, i) => {
    const e = 6 + 16 * i;
    head.writeUInt8(px % 256, e); // 256 记作 0
    head.writeUInt8(px % 256, e + 1);
    head.writeUInt16LE(1, e + 4); // 色彩平面
    head.writeUInt16LE(32, e + 6); // 位深
    head.writeUInt32LE(data.length, e + 8);
    head.writeUInt32LE(offset, e + 12);
    offset += data.length;
  });
  return Buffer.concat([head, ...images.map((i) => i.data)]);
}
