# 生成应用图标「桌面日出」：暖纸圆角方块里，赤陶半日带五道光芒，落在墨色桌沿上
# （desk-top 的谐音）。32px 及以下换小尺寸版：不画光芒、日和桌沿加粗、描边加深，免得糊成一团。
# 界面里的同款图形在 apps/desktop/src/app/widgets.rs 的 brand_glyph / brand_mark，几何照这里。
# 坐标按 256 的画布写，放大 4 倍画好再缩到各尺寸。
# 用法：python tools/make-icon.py apps/desktop/resources/claudesktop.ico（需要 Pillow）
import sys
from PIL import Image, ImageDraw

K = 4  # 超采样倍数
PAPER = (250, 249, 245, 255)
CORAL = (217, 119, 87, 255)
INK = (31, 30, 29, 255)


def bar(d, x1, y1, x2, y2, w, color):
    # 圆头线段
    s = lambda v: v * K
    d.line([s(x1), s(y1), s(x2), s(y2)], fill=color, width=round(s(w)))
    for x, y in ((x1, y1), (x2, y2)):
        d.ellipse([s(x - w / 2), s(y - w / 2), s(x + w / 2), s(y + w / 2)], fill=color)


def draw(small):
    img = Image.new("RGBA", (256 * K, 256 * K), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    s = lambda *v: [x * K for x in v]
    if small:
        d.rounded_rectangle(s(8, 8, 248, 248), radius=56 * K, fill=PAPER,
                            outline=(196, 190, 176, 255), width=10 * K)
        cx, cy, r = 128, 156, 68
        desk = (54, 190, 202, 190, 28)
    else:
        d.rounded_rectangle(s(12, 12, 244, 244), radius=56 * K, fill=PAPER,
                            outline=(211, 206, 193, 255), width=4 * K)
        cx, cy, r = 128, 160, 54
        desk = (52, 180, 204, 180, 16)
        for ray in ((128, 62, 128, 80), (68, 86, 80, 98), (188, 86, 176, 98),
                    (44, 134, 60, 134), (196, 134, 212, 134)):
            bar(d, *ray, 12, CORAL)
    d.pieslice(s(cx - r, cy - r, cx + r, cy + r), 180, 360, fill=CORAL)
    bar(d, *desk, INK)
    return img


sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
big, small = draw(False), draw(True)
imgs = [(small if n <= 32 else big).resize((n, n), Image.LANCZOS) for n in sizes]
out = sys.argv[1]
imgs[-1].save(out, sizes=[(n, n) for n in sizes], append_images=imgs[:-1])
print("ok", out)
