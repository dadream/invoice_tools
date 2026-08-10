#!/usr/bin/env python3
"""生成 Tauri 打包所需的占位图标。设计稿到位后替换 src-tauri/icons/ 即可。"""
from pathlib import Path
from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"
OUT.mkdir(parents=True, exist_ok=True)

BG = (0, 112, 243, 255)   # #0070f3
FG = (255, 255, 255, 255)


def render(size: int) -> Image.Image:
    """蓝底白色票据轮廓。"""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    r = max(2, size // 8)
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=r, fill=BG)
    m = size // 4
    d.rectangle([m, m, size - m - 1, size - m - 1], outline=FG, width=max(1, size // 32))
    for i in (1, 2):
        y = m + (size - 2 * m) * i // 3
        d.line([m + size // 12, y, size - m - size // 12, y], fill=FG, width=max(1, size // 40))
    return img


png_sizes = {
    "32x32.png": 32,
    "128x128.png": 128,
    "128x128@2x.png": 256,
    "icon.png": 512,
    "Square30x30Logo.png": 30,
    "Square44x44Logo.png": 44,
    "Square71x71Logo.png": 71,
    "Square89x89Logo.png": 89,
    "Square107x107Logo.png": 107,
    "Square142x142Logo.png": 142,
    "Square150x150Logo.png": 150,
    "Square284x284Logo.png": 284,
    "Square310x310Logo.png": 310,
    "StoreLogo.png": 50,
}
for name, size in png_sizes.items():
    render(size).save(OUT / name)

# .ico 多尺寸
render(256).save(OUT / "icon.ico", sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
# .icns：Pillow 要求最小 1024 的方图输入
render(1024).save(OUT / "icon.icns")

print(f"已生成 {len(list(OUT.iterdir()))} 个图标 -> {OUT}")
