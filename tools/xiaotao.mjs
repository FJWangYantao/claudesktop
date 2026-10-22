// 品牌形象「小陶」：一颗上了釉的赤陶小石子，两只眼睛。按 外形 × 脸 × 釉 × 底 × 表情 生成 256 画布的 SVG。
// 光从左上来：底色径向渐变 + 右下明暗交界 + 底边反光 + 两层高光（一片柔光、一块釉面锐光）。
// detail 分三档：full（≥64px，带陶土颗粒）/ mid（40~48px）/ small（≤32px：去掉釉线和细节，眼睛放大）。
// 由 tools/make-icon.mjs 调用出图标和界面里的标志；各选项的样子见 PR 里的探索图。

const f = (v) => +(+v).toFixed(2);

// ---------- 外形 ----------
// 超椭圆取点（上下半用不同指数：底更平才坐得住），再用 Catmull-Rom 连成平滑的三次贝塞尔
function superPts({ cx = 128, cy = 130, a, b, n = 2.3, nTop = 2.2, nBot = 2.6, egg = 0, wobble = 0, rot = 0, N = 72 }) {
  const pts = [];
  for (let i = 0; i < N; i++) {
    const t = (i / N) * Math.PI * 2;
    const c = Math.cos(t);
    const s = Math.sin(t);
    let x = a * Math.sign(c) * Math.pow(Math.abs(c), 2 / n);
    let y = b * Math.sign(s) * Math.pow(Math.abs(s), 2 / (s > 0 ? nBot : nTop));
    if (egg) x *= 1 - egg * Math.max(0, -y / b);
    if (wobble) {
      const k = 1 + wobble * (0.6 * Math.sin(2 * t + 0.7) + 0.4 * Math.sin(3 * t + 2.1));
      x *= k;
      y *= k;
    }
    if (rot) {
      const r = (rot * Math.PI) / 180;
      [x, y] = [x * Math.cos(r) - y * Math.sin(r), x * Math.sin(r) + y * Math.cos(r)];
    }
    pts.push([cx + x, cy + y]);
  }
  return pts;
}

function smooth(pts) {
  const n = pts.length;
  let d = `M${f(pts[0][0])} ${f(pts[0][1])}`;
  for (let i = 0; i < n; i++) {
    const [p0, p1, p2, p3] = [-1, 0, 1, 2].map((k) => pts[(i + k + n) % n]);
    d += `C${f(p1[0] + (p2[0] - p0[0]) / 6)} ${f(p1[1] + (p2[1] - p0[1]) / 6)} `;
    d += `${f(p2[0] - (p3[0] - p1[0]) / 6)} ${f(p2[1] - (p3[1] - p1[1]) / 6)} ${f(p2[0])} ${f(p2[1])}`;
  }
  return d + 'Z';
}

export const SHAPES = {
  round: { label: '圆石', p: { a: 78, b: 70, n: 2.2, nTop: 2.1, nBot: 2.7 } },
  mochi: { label: '麻薯', p: { a: 86, b: 62, cy: 136, n: 2.5, nTop: 2.0, nBot: 4.2 } },
  cube: { label: '方糖', p: { a: 74, b: 70, n: 4.2, nTop: 4.2, nBot: 4.8 } },
  egg: { label: '蛋形', p: { a: 68, b: 80, cy: 126, n: 2.0, nTop: 2.0, nBot: 2.4, egg: 0.16 } },
  hand: { label: '手捏', p: { a: 80, b: 68, n: 2.3, nTop: 2.1, nBot: 3.0, wobble: 0.05, rot: -5 } },
  jar: { label: '小罐', p: { a: 76, b: 62, cy: 142, n: 2.4, nTop: 2.6, nBot: 3.4 }, jar: true },
};

function shapeOf(kind, scale, [ox, oy] = [0, 0]) {
  const s = SHAPES[kind];
  const p = { ...s.p, a: s.p.a * scale, b: s.p.b * scale, cx: 128 + ox, cy: 128 + ((s.p.cy ?? 130) - 128) * scale + oy };
  const pts = superPts(p);
  const xs = pts.map((q) => q[0]);
  const ys = pts.map((q) => q[1]);
  const box = { x0: Math.min(...xs), x1: Math.max(...xs), y0: Math.min(...ys), y1: Math.max(...ys) };
  box.w = box.x1 - box.x0;
  box.h = box.y1 - box.y0;
  box.cx = (box.x0 + box.x1) / 2;
  return { d: smooth(pts), box, jar: s.jar };
}

// ---------- 釉 ----------
// base：从高光到暗部四档；core：明暗交界；bounce：底边反光；clay：浸釉时露出的陶土
export const GLAZES = {
  gloss: { label: '亮釉', base: ['#F7B99C', '#E68D67', '#D06A47', '#A44526'], core: '#7A2A15', bounce: '#FFCDB4', gloss: true },
  dip: {
    label: '浸釉', base: ['#F2A585', '#DD7B57', '#C25C3B', '#963B20'], core: '#6E2412', bounce: '#FFC8AE', gloss: true,
    clay: ['#DFA07D', '#C9805C', '#A3603F'],
  },
  cream: {
    label: '奶釉', base: ['#FFFFFF', '#FBF7EF', '#EDE5D6', '#CFC4B1'], core: '#8C7A62', bounce: '#FFF6EA', gloss: true,
    clay: ['#E9A07B', '#D67D58', '#B9603F'], light: true,
  },
  matte: { label: '素烧', base: ['#EFB596', '#DF9470', '#C9744F', '#A85A3A'], core: '#7A3A20', bounce: '#FFD4BD', gloss: false, speckAll: true },
};

// 固定种子的伪随机，每次出图一样
function rng(seed) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function speckles(B, n, seed, keep = () => true) {
  const r = rng(seed);
  let s = '';
  for (let i = 0; i < n; i++) {
    const x = B.x0 + r() * B.w;
    const y = B.y0 + r() * B.h;
    if (!keep(y)) continue;
    const dark = r() < 0.65;
    s += `<circle cx="${f(x)}" cy="${f(y)}" r="${f(0.4 + r() * 1.1)}" fill="${dark ? '#6E3520' : '#FFE6D4'}" opacity="${dark ? 0.28 : 0.35}"/>`;
  }
  return s;
}

function body(sh, g, detail) {
  const { box: B, d } = sh;
  const G = GLAZES[g];
  const W = B.w;
  const H = B.h;
  const blur = (id, sd) => `<filter id="${id}" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="${f(sd)}"/></filter>`;
  let defs = `<clipPath id="clip"><path d="${d}"/></clipPath>
<radialGradient id="base" gradientUnits="userSpaceOnUse" cx="${f(B.x0 + 0.32 * W)}" cy="${f(B.y0 + 0.22 * H)}" r="${f(Math.max(W, H))}">
<stop offset="0" stop-color="${G.base[0]}"/><stop offset=".32" stop-color="${G.base[1]}"/><stop offset=".66" stop-color="${G.base[2]}"/><stop offset="1" stop-color="${G.base[3]}"/></radialGradient>
<linearGradient id="coreG" gradientUnits="userSpaceOnUse" x1="${f(B.x0 + 0.2 * W)}" y1="${f(B.y0 + 0.1 * H)}" x2="${f(B.x1)}" y2="${f(B.y1)}"><stop offset=".35" stop-color="#000"/><stop offset="1" stop-color="#fff"/></linearGradient>
<mask id="coreM"><rect width="256" height="256" fill="url(#coreG)"/></mask>
<linearGradient id="bounceG" gradientUnits="userSpaceOnUse" x1="0" y1="${f(B.y0 + 0.6 * H)}" x2="0" y2="${f(B.y1)}"><stop offset="0" stop-color="#000"/><stop offset="1" stop-color="#fff"/></linearGradient>
<mask id="bounceM"><rect width="256" height="256" fill="url(#bounceG)"/></mask>
${blur('b1', 0.05 * W)}${blur('b2', 0.016 * W)}${blur('b3', 0.004 * W + 0.3)}`;
  let L = `<path d="${d}" fill="url(#base)"/>`;
  // 浸釉：下沿一圈没上釉的陶土，釉的边上积一道深色、上沿一线反光
  if (G.clay && detail !== 'small') {
    const dipY = (x) => B.y0 + 0.79 * H + 3 * Math.sin(0.055 * x + 0.8) + 1.6 * Math.sin(0.13 * x + 2);
    let wave = '';
    for (let x = B.x0 - 6, i = 0; x <= B.x1 + 6; x += 4, i++) wave += `${i ? 'L' : 'M'}${f(x)} ${f(dipY(x))}`;
    defs += `<radialGradient id="clay" gradientUnits="userSpaceOnUse" cx="${f(B.x0 + 0.35 * W)}" cy="${f(B.y0 + 0.55 * H)}" r="${f(0.8 * W)}">
<stop offset="0" stop-color="${G.clay[0]}"/><stop offset=".6" stop-color="${G.clay[1]}"/><stop offset="1" stop-color="${G.clay[2]}"/></radialGradient>`;
    L += `<g clip-path="url(#clip)"><path d="${wave}L${f(B.x1 + 6)} ${f(B.y1 + 8)}L${f(B.x0 - 6)} ${f(B.y1 + 8)}Z" fill="url(#clay)"/>`;
    if (detail === 'full') L += speckles(B, 90, 7, (y) => y > dipY(B.cx) - 2);
    L += `<path d="${wave}" fill="none" stroke="#000" stroke-opacity=".16" stroke-width="${f(0.03 * W)}" filter="url(#b2)" transform="translate(0 ${f(0.012 * H)})"/>`;
    L += `<path d="${wave}" fill="none" stroke="${G.light ? '#D9CDB8' : G.base[3]}" stroke-width="${f(0.017 * W)}" stroke-opacity=".8"/>`;
    L += `<path d="${wave}" fill="none" stroke="#fff" stroke-opacity="${G.light ? 0.7 : 0.35}" stroke-width="1.2" transform="translate(0 -1.6)"/></g>`;
  }
  if (G.speckAll && detail === 'full') L += `<g clip-path="url(#clip)">${speckles(B, 150, 3)}</g>`;
  // 明暗交界（右下）、底边反光、一片柔光、一块釉面锐光
  const hx = B.x0 + 0.34 * W;
  const hy = B.y0 + 0.25 * H;
  const gx = B.x0 + 0.27 * W;
  const gy = B.y0 + 0.2 * H;
  L += `<g clip-path="url(#clip)">
<path d="${d}" fill="none" stroke="${G.core}" stroke-opacity="${G.light ? 0.35 : 0.5}" stroke-width="${f(0.26 * Math.min(W, H))}" filter="url(#b1)" mask="url(#coreM)"/>
<path d="${d}" fill="none" stroke="${G.bounce}" stroke-opacity=".55" stroke-width="${f(0.05 * W)}" filter="url(#b2)" mask="url(#bounceM)"/>
<ellipse cx="${f(hx)}" cy="${f(hy)}" rx="${f(0.25 * W)}" ry="${f(0.13 * H)}" transform="rotate(-24 ${f(hx)} ${f(hy)})" fill="#fff" opacity="${G.gloss ? 0.38 : 0.2}" filter="url(#b1)"/>`;
  if (G.gloss && detail !== 'small') {
    L += `<ellipse cx="${f(gx)}" cy="${f(gy)}" rx="${f(0.075 * W)}" ry="${f(0.036 * H)}" transform="rotate(-36 ${f(gx)} ${f(gy)})" fill="#fff" opacity=".9" filter="url(#b3)"/>`;
  }
  L += '</g>';
  if (sh.jar) {
    // 罐盖：一道接缝 + 顶上一个小钮
    const y = B.y0 + 0.2 * H;
    L = `<rect x="${f(B.cx - 0.09 * W)}" y="${f(B.y0 - 0.1 * H)}" width="${f(0.18 * W)}" height="${f(0.16 * H)}" rx="${f(0.06 * W)}" fill="url(#base)"/>${L}
<g clip-path="url(#clip)"><path d="M${f(B.x0)} ${f(y)}Q${f(B.cx)} ${f(y + 0.09 * H)} ${f(B.x1)} ${f(y)}" fill="none" stroke="${G.core}" stroke-opacity=".45" stroke-width="2.4"/>
<path d="M${f(B.x0)} ${f(y + 2)}Q${f(B.cx)} ${f(y + 0.09 * H + 2)} ${f(B.x1)} ${f(y + 2)}" fill="none" stroke="#fff" stroke-opacity=".35" stroke-width="1.2"/></g>`;
  }
  return { defs, L };
}

// ---------- 脸 / 表情 ----------
export const FACES = {
  capsule: '竖眼', dots: '豆眼', dotsBlush: '豆眼 + 腮红', capsuleBlush: '竖眼 + 腮红', capsuleSmile: '竖眼 + 腮红 + 小嘴', happy: '笑眼',
};
export const EXPRS = { idle: '待命', think: '思考', focus: '专注', done: '完成', oops: '出错', sleep: '休息', surprised: '吃惊' };

// 运行状态行用的会动的表情：每个有几帧（frame 0..n-1），轮着放的顺序和节奏在界面那边（mascot.rs）。
// 画在 20px 上：道具（泡、问号、汗珠）按绝对尺寸画粗，身子缩小挪到左下给道具让位置（见 STATUS_LAYOUT）
export const MOODS = {
  think: { label: '琢磨中', frames: 4 }, // 往右上看，头顶的泡一个个冒出来
  talk: { label: '正在回复', frames: 3 }, // 嘴一张一合
  ask: { label: '等待你授权', frames: 2 }, // 睁大眼，问号上下跳
  sweat: { label: '自动重试', frames: 3 }, // 汗珠往下滑
  squish: { label: '压缩上下文', frames: 2 }, // 闭眼使劲，压扁再回弹
  wake: { label: '续接会话', frames: 3 }, // 眼睛慢慢睁开
  scan: { label: '准备 worktree', frames: 2 }, // 左右张望
};
export const STATUS_LAYOUT = { scale: 1.08, offset: [-14, 16] };

function face(sh, kind, g, detail, expr, frame = 0) {
  const B = sh.box;
  const W = B.w;
  const H = B.h;
  const light = GLAZES[g].light;
  const ink = '#2A1912';
  const ey = B.y0 + 0.57 * H;
  const dx = 0.155 * W;
  const cx = B.cx;
  // 尺寸越小眼睛越大，否则 16px 下只剩一个像素
  const k = { small: [1.3, 1.18], mid: [1.24, 1.12], full: [1, 1] }[detail];
  const ew = 0.092 * W * k[0];
  const eh = 0.2 * H * k[1];
  const defs = `<radialGradient id="eye" cx=".35" cy=".3" r=".9"><stop offset="0" stop-color="#4A3026"/><stop offset="1" stop-color="#160C08"/></radialGradient>`;
  const glint = (x, y, r) => (detail === 'small' ? '' : `<circle cx="${f(x)}" cy="${f(y)}" r="${f(r)}" fill="#fff" opacity=".92"/>`);
  const pill = (x, y, w = ew, h = eh) =>
    `<rect x="${f(x - w / 2)}" y="${f(y - h / 2)}" width="${f(w)}" height="${f(h)}" rx="${f(Math.min(w, h) / 2)}" fill="url(#eye)"/>` + glint(x - w * 0.12, y - h * 0.26, Math.min(w, h) * 0.22);
  const dot = (x, y, r = 0.045 * W * k[0]) => `<circle cx="${f(x)}" cy="${f(y)}" r="${f(r)}" fill="url(#eye)"/>` + glint(x - r * 0.3, y - r * 0.35, r * 0.32);
  const stroke = (d, w = 0.032 * W * (detail === 'small' ? 1.4 : 1)) =>
    `<path d="${d}" fill="none" stroke="${ink}" stroke-width="${f(w)}" stroke-linecap="round" stroke-linejoin="round"/>`;
  // up：笑眼 ^；!up：闭眼 ‿
  const arc = (x, y, up = true) =>
    stroke(`M${f(x - 0.055 * W)} ${f(y + (up ? 0.03 : -0.02) * H)}Q${f(x)} ${f(y + (up ? -0.07 : 0.06) * H)} ${f(x + 0.055 * W)} ${f(y + (up ? 0.03 : -0.02) * H)}`);
  const blush = () =>
    detail === 'small'
      ? ''
      : [-1, 1]
          .map((s) => `<ellipse cx="${f(cx + s * 0.285 * W)}" cy="${f(ey + 0.12 * H)}" rx="${f(0.062 * W)}" ry="${f(0.032 * H)}" fill="${light ? '#F29A86' : '#FFB4A0'}" opacity="${light ? 0.5 : 0.55}" filter="url(#b2)"/>`)
          .join('');
  const smile = () => stroke(`M${f(cx - 0.032 * W)} ${f(ey + 0.085 * H)}Q${f(cx)} ${f(ey + 0.125 * H)} ${f(cx + 0.032 * W)} ${f(ey + 0.085 * H)}`, 0.014 * W);
  const both = (fn) => fn(cx - dx) + fn(cx + dx);
  const round = kind.startsWith('dots');
  const eye = (x, y = ey, w = ew, h = eh) => (round ? dot(x, y) : pill(x, y, w, h));
  const blushed = kind.includes('Blush') || kind.includes('Smile') || kind === 'happy';
  let s = '';
  if (expr === 'idle') {
    if (kind === 'happy') s = blush() + both((x) => arc(x, ey));
    else s = (blushed ? blush() : '') + both((x) => eye(x)) + (kind === 'capsuleSmile' ? smile() : '');
  } else if (expr === 'focus') {
    // 眯成两道横着的短条
    s = both((x) => pill(x, ey + 0.03 * H, ew * 1.7, ew * 0.9));
  } else if (expr === 'done') {
    s = blush() + both((x) => arc(x, ey));
  } else if (expr === 'oops') {
    const chev = (x, dir) => stroke(`M${f(x - dir * 0.04 * W)} ${f(ey - 0.06 * H)}L${f(x + dir * 0.03 * W)} ${f(ey)}L${f(x - dir * 0.04 * W)} ${f(ey + 0.06 * H)}`, 0.03 * W);
    s = chev(cx - dx, 1) + chev(cx + dx, -1);
    s += `<path d="M${f(B.x1 - 0.08 * W)} ${f(B.y0 + 0.1 * H)}q${f(0.04 * W)} ${f(0.07 * H)} 0 ${f(0.1 * H)}q${f(-0.04 * W)} ${f(-0.03 * H)} 0 ${f(-0.1 * H)}z" fill="#E6F0F5" stroke="#A9C2CF" stroke-width="1.5"/>`;
  } else if (expr === 'sleep') {
    s = both((x) => arc(x, ey + 0.02 * H, false));
    // 两个 z 用折线画，不依赖字体
    const z = (x, y, w) => `<path d="M${f(x)} ${f(y)}h${f(w)}l${f(-w)} ${f(w)}h${f(w)}" fill="none" stroke="#B5AD9E" stroke-width="${f(w * 0.22)}" stroke-linecap="round" stroke-linejoin="round"/>`;
    s += z(B.x1 - 0.04 * W, B.y0 + 0.02 * H, 13) + z(B.x1 + 0.08 * W, B.y0 - 0.1 * H, 9);
  } else if (expr === 'surprised') {
    s = both((x) => dot(x, ey - 0.02 * H, 0.06 * W * k[0])) + `<ellipse cx="${f(cx)}" cy="${f(ey + 0.12 * H)}" rx="${f(0.035 * W)}" ry="${f(0.045 * H)}" fill="${ink}"/>`;
  } else if (expr === 'think') {
    s = both((x) => eye(x + 0.04 * W, ey - 0.06 * H, ew, eh * 0.85));
    s += [[B.x1 - 4, B.y0 - 4, 12], [B.x1 + 20, B.y0 - 30, 16], [B.x1 + 46, B.y0 - 58, 19]]
      .slice(0, frame)
      .map(([x, y, r]) => `<circle cx="${f(x)}" cy="${f(y)}" r="${r}" fill="#ADA596"/>`)
      .join('');
  } else if (expr === 'talk') {
    s = both((x) => eye(x, ey - 0.03 * H));
    const my = ey + 0.17 * H;
    if (frame === 2) s += stroke(`M${f(cx - 0.07 * W)} ${f(my - 0.02 * H)}Q${f(cx)} ${f(my + 0.05 * H)} ${f(cx + 0.07 * W)} ${f(my - 0.02 * H)}`, 0.045 * W);
    else s += `<ellipse cx="${f(cx)}" cy="${f(my)}" rx="${f((frame ? 0.06 : 0.08) * W)}" ry="${f((frame ? 0.045 : 0.085) * H)}" fill="${ink}"/>`;
  } else if (expr === 'ask') {
    s = both((x) => dot(x, ey - 0.02 * H, 0.062 * W * k[0]));
    // 问号：一道弯钩 + 一个点，赤红色（和“等待你授权”的红字同一个意思）
    const qx = B.x1 + 26;
    const qy = B.y0 - 30 - (frame ? 10 : 0);
    s += `<path d="M${qx - 18} ${qy - 10}q0-22 20-22q20 0 20 18q0 14-18 20v12" fill="none" stroke="#C0504D" stroke-width="17" stroke-linecap="round" stroke-linejoin="round"/>`;
    s += `<circle cx="${qx + 2}" cy="${qy + 42}" r="10" fill="#C0504D"/>`;
  } else if (expr === 'sweat') {
    s = both((x) => dot(x - 0.02 * W, ey + 0.03 * H));
    const y = B.y0 + 6 + frame * 18;
    const x = B.x1 + 8;
    s += `<path d="M${x} ${y}q22 30 0 44q-22-14 0-44z" fill="#A9CBDD" stroke="#5E8FA8" stroke-width="5" stroke-linejoin="round"/>`;
  } else if (expr === 'squish') {
    const chev = (x, dir) => stroke(`M${f(x - dir * 0.05 * W)} ${f(ey - 0.07 * H)}L${f(x + dir * 0.04 * W)} ${f(ey)}L${f(x - dir * 0.05 * W)} ${f(ey + 0.07 * H)}`, 0.05 * W);
    s = chev(cx - dx, 1) + chev(cx + dx, -1);
  } else if (expr === 'wake') {
    if (frame === 0) s = both((x) => arc(x, ey + 0.02 * H, false));
    else s = both((x) => pill(x, ey + (frame === 1 ? 0.04 * H : 0), ew, frame === 1 ? ew : eh));
  } else if (expr === 'scan') {
    s = both((x) => pill(x + (frame ? 0.05 : -0.05) * W, ey, ew * 1.15, eh * 0.75));
  }
  return { defs, s };
}

// ---------- 底 ----------
export const BGS = { paper: '纸底', sand: '沙底', ink: '墨底', coral: '赤陶底', none: '无底' };

function tile(bg) {
  const T = {
    paper: { top: '#FDFBF6', bottom: '#ECE5D5', edge: '#0000001A', grain: true },
    sand: { top: '#F3EEE3', bottom: '#DED5C2', edge: '#0000001F', grain: true },
    ink: { top: '#36322E', bottom: '#151413', edge: '#00000040', glow: true },
    coral: { top: '#E28A68', bottom: '#BE5534', edge: '#00000026' },
  }[bg];
  if (!T) return { defs: '', back: '', front: '' };
  const defs = `<linearGradient id="tileG" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${T.top}"/><stop offset="1" stop-color="${T.bottom}"/></linearGradient>
<linearGradient id="rim" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".75"/><stop offset=".22" stop-color="#fff" stop-opacity="0"/></linearGradient>
<filter id="grain" x="0" y="0" width="100%" height="100%"><feTurbulence type="fractalNoise" baseFrequency=".85" numOctaves="2" seed="4"/><feColorMatrix values="0 0 0 0 .45  0 0 0 0 .38  0 0 0 0 .3  0 0 0 .09 0"/></filter>
<clipPath id="tileClip"><rect x="8" y="8" width="240" height="240" rx="56"/></clipPath>
<radialGradient id="glowG" cx=".5" cy=".5" r=".5"><stop offset="0" stop-color="#E8835C" stop-opacity=".32"/><stop offset="1" stop-color="#E8835C" stop-opacity="0"/></radialGradient>`;
  let back = '<rect x="8" y="8" width="240" height="240" rx="56" fill="url(#tileG)"/>';
  if (T.grain) back += '<rect x="8" y="8" width="240" height="240" rx="56" filter="url(#grain)" clip-path="url(#tileClip)"/>';
  if (T.glow) back += '<circle cx="128" cy="124" r="118" fill="url(#glowG)"/>';
  const front = `<rect x="9.5" y="9.5" width="237" height="237" rx="54.5" fill="none" stroke="url(#rim)" stroke-width="3"/><rect x="8.5" y="8.5" width="239" height="239" rx="55.5" fill="none" stroke="${T.edge}"/>`;
  return { defs, back, front };
}

// 落在底上的影子：一圈宽的柔影 + 贴地的一道实影
function shadow(sh, bg, detail) {
  const B = sh.box;
  const dark = bg === 'ink';
  const col = dark ? '#000' : bg === 'coral' ? '#5A2010' : '#6B4330';
  if (detail === 'small') {
    return `<ellipse cx="${f(B.cx + 2)}" cy="${f(B.y1 + 3)}" rx="${f(0.42 * B.w)}" ry="${f(0.05 * B.h + 2)}" fill="${col}" opacity="${dark ? 0.45 : 0.22}" filter="url(#b2)"/>`;
  }
  return `<ellipse cx="${f(B.cx + 4)}" cy="${f(B.y1 + 6)}" rx="${f(0.5 * B.w)}" ry="${f(0.07 * B.h + 4)}" fill="${col}" opacity="${dark ? 0.5 : 0.2}" filter="url(#b1)"/>
<ellipse cx="${f(B.cx + 1)}" cy="${f(B.y1 + 1)}" rx="${f(0.36 * B.w)}" ry="${f(0.03 * B.h + 1.5)}" fill="${col}" opacity="${dark ? 0.6 : 0.3}" filter="url(#b2)"/>`;
}

/**
 * 生成一张小陶。size 给了就写进 width / height（gpui 按这个固有尺寸的 2 倍栅格化，界面里要和显示尺寸相当）。
 * @param {{shape?: string, face?: string, glaze?: string, bg?: string, detail?: 'full'|'mid'|'small', expr?: string, size?: number}} o
 */
export function xiaotao({ shape = 'round', face: fk = 'capsuleBlush', glaze = 'dip', bg = 'paper', detail = 'full', expr = 'idle', frame = 0, size, scale, offset = [0, 0] } = {}) {
  // 没有底时画满一些；小尺寸再放大一档
  scale ??= detail === 'small' ? (bg === 'none' ? 1.34 : 1.24) : bg === 'none' ? 1.24 : 1.08;
  const sh = shapeOf(shape, scale, offset);
  const t = tile(bg);
  const b = body(sh, glaze, detail);
  const fc = face(sh, fk, glaze, detail, expr, frame);
  const dim = size ? ` width="${size}" height="${size}"` : '';
  // 压扁：以底边中点为基准横向撑开、纵向压低（影子不跟着变）
  const B = sh.box;
  const squash = expr === 'squish' && frame === 1 ? ` transform="translate(${f(B.cx)} ${f(B.y1)}) scale(1.16 .8) translate(${f(-B.cx)} ${f(-B.y1)})"` : '';
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256"${dim}><defs>${t.defs}${b.defs}${fc.defs}</defs>${t.back}${shadow(sh, bg, detail)}<g${squash}>${b.L}${fc.s}</g>${t.front}</svg>\n`;
}
