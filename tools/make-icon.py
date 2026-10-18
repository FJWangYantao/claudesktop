# 生成应用图标：赤陶色圆角方块 + 白色内凹四角星（1024 画好再缩到各尺寸）
# 用法：python tools/make-icon.py apps/desktop/resources/claudesktop.ico（需要 Pillow）
import math, sys
from PIL import Image, ImageDraw

S = 1024
CORAL = (217, 119, 87, 255)
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
pad = 40
d.rounded_rectangle([pad, pad, S - pad, S - pad], radius=220, fill=CORAL)

def star(cx, cy, r, k=2.4, n=720):
    # 超椭圆 |x|^(2/k) + |y|^(2/k) = 1 的参数式，k>1 时四边内凹成四角星
    pts = []
    for i in range(n):
        t = 2 * math.pi * i / n
        c, s = math.cos(t), math.sin(t)
        x = math.copysign(abs(c) ** k, c)
        y = math.copysign(abs(s) ** k, s)
        pts.append((cx + r * x, cy + r * y))
    return pts

d.polygon(star(S * 0.5, S * 0.52, S * 0.32), fill=(255, 255, 255, 255))
# 右上角的小星，呼应界面里的 Sparkles
d.polygon(star(S * 0.745, S * 0.27, S * 0.085), fill=(255, 255, 255, 255))

out = sys.argv[1]
img.save(out, sizes=[(16, 16), (20, 20), (24, 24), (32, 32), (40, 40), (48, 48), (64, 64), (128, 128), (256, 256)])
print("ok", out)
