"""Draws the app icon and Store tile art (the six-slice ring)."""
import math, os
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
BLUE, WHITE, BG = (47, 129, 247, 255), (255, 255, 255, 255), (20, 24, 38, 255)

def ring(size, scale=4):
    s = size * scale
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    c, r_out, r_in = s / 2, s * 0.48, s * 0.22
    for i in range(6):
        start = -90 - 30 + i * 60
        d.pieslice([c - r_out, c - r_out, c + r_out, c + r_out], start, start + 60,
                   fill=WHITE if i == 0 else BLUE)
    # gaps between slices, then the hole
    for i in range(6):
        a = math.radians(-120 + i * 60)
        d.line([c, c, c + math.cos(a) * s, c + math.sin(a) * s], fill=(0, 0, 0, 0), width=max(int(s * 0.05), 1))
    d.ellipse([c - r_in, c - r_in, c + r_in, c + r_in], fill=(0, 0, 0, 0))
    return img.resize((size, size), Image.LANCZOS)

def tile(w, h, fill=0.62, background=BG):
    img = Image.new("RGBA", (w, h), background)
    side = max(int(min(w, h) * fill), 1)
    img.alpha_composite(ring(side), ((w - side) // 2, (h - side) // 2))
    return img

def main():
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    frames = [ring(n) for n in sizes]
    frames[-1].save(os.path.join(HERE, "app.ico"), sizes=[(n, n) for n in sizes], append_images=frames[:-1])
    pack = os.path.join(HERE, "..", "packaging", "Assets")
    os.makedirs(pack, exist_ok=True)
    for scale in (100, 200, 400):
        k = scale / 100
        for name, w, h, fill in [("Square44x44Logo", 44, 44, 0.8), ("Square71x71Logo", 71, 71, 0.66),
                                 ("Square150x150Logo", 150, 150, 0.56), ("Square310x310Logo", 310, 310, 0.56),
                                 ("Wide310x150Logo", 310, 150, 0.66), ("StoreLogo", 50, 50, 0.8),
                                 ("SplashScreen", 620, 300, 0.5)]:
            tile(int(w * k), int(h * k), fill).save(os.path.join(pack, f"{name}.scale-{scale}.png"))
    # Taskbar and Start list use the bare ring.
    for n in (16, 24, 32, 48, 256):
        ring(n).save(os.path.join(pack, f"Square44x44Logo.targetsize-{n}.png"))
        ring(n).save(os.path.join(pack, f"Square44x44Logo.targetsize-{n}_altform-unplated.png"))

if __name__ == "__main__":
    main()
