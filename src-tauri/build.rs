//! Tauri 2 构建脚本:
//! 1. 调用 tauri_build::build() 处理 Tauri 资源/上下文。
//! 2. 代码生成图标(深色圆角底 + 白色 `>` 提示符 + 蓝色光标下划线),免二进制资产入库:
//!    - 32x32 / 64x64 / 128x128 / 128x128@2x / 1024x1024 PNG
//!      (供 Tauri 打包 + 托盘 + 窗口图标)
//!    - icon.icns / icon.ico 是多尺寸容器格式,`png` crate 写不出来,
//!      由 `pnpm tauri icon icons/icon.png` 一次性重生成后入库(不进本脚本)。
//!
//! 图形语义:「终端等你敲下一条命令」——`>` 是提示符,蓝色下划线是闪烁光标,
//! 对应本项目「运行 / 管理 JS·TS 脚本」的核心动作。纯直线段,32px 下不糊。

use std::path::PathBuf;

const BASE: i32 = 64;
/// macOS 图标网格:四周留约 9% 透明边距,与系统内其他 App 视觉齐平。
const MARGIN: i32 = 6;
/// 圆角半径(逻辑网格)。
const RADIUS: i32 = 11;

#[derive(Clone, Copy)]
struct Rgba(u8, u8, u8, u8);

const BG: Rgba = Rgba(0x14, 0x1a, 0x2b, 255);
const FG: Rgba = Rgba(0xe8, 0xec, 0xf4, 255);
const ACCENT: Rgba = Rgba(0x5b, 0x8c, 0xff, 255);

/// 以 BASE=64 逻辑网格渲染图标,scale 为整数放大倍数(最近邻风格几何,
/// 线条粗细随 scale 加粗,保证高分下图标锐利)。
fn render(scale: i32) -> Vec<u8> {
    let w = BASE * scale;
    let h = BASE * scale;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let put = |buf: &mut [u8], x: i32, y: i32, c: Rgba| {
        if (0..w).contains(&x) && (0..h).contains(&y) {
            let i = (y * w + x) as usize * 4;
            let a = c.3 as u16;
            for (k, col) in [c.0, c.1, c.2, c.3].iter().enumerate() {
                let dst = buf[i + k] as u16;
                buf[i + k] = (((dst * (255 - a) + *col as u16 * a) / 255) & 0xff) as u8;
            }
        }
    };

    // 圆角矩形底:矩形内缩 MARGIN,四角按 RADIUS 倒角
    let (lo, hi, r) = (
        MARGIN * scale,
        (BASE - MARGIN) * scale - 1,
        RADIUS * scale,
    );
    for y in lo..=hi {
        for x in lo..=hi {
            let dx = if x < lo + r {
                lo + r - x
            } else if x > hi - r {
                x - (hi - r)
            } else {
                0
            };
            let dy = if y < lo + r {
                lo + r - y
            } else if y > hi - r {
                y - (hi - r)
            } else {
                0
            };
            if dx * dx + dy * dy <= r * r {
                put(&mut buf, x, y, BG);
            }
        }
    }

    // 画线(沿 Bresenham 路径盖圆盘),坐标与粗细均按 scale 放大
    let line = |buf: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32, t: i32, c: Rgba| {
        let (x0, y0, x1, y1, t) = (x0 * scale, y0 * scale, x1 * scale, y1 * scale, t * scale);
        let mut plot = |x: i32, y: i32| {
            let r2 = t * t;
            for dy in -t..=t {
                for dx in -t..=t {
                    if dx * dx + dy * dy <= r2 {
                        put(buf, x + dx, y + dy, c);
                    }
                }
            }
        };
        let (mut x, mut y) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            plot(x, y);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    };

    // `>` 提示符(两段直线,圆头笔触自动接成尖角)
    line(&mut buf, 21, 23, 29, 32, 2, FG);
    line(&mut buf, 29, 32, 21, 41, 2, FG);
    // 光标下划线:蓝色,图像里唯一的强调色
    line(&mut buf, 34, 41, 43, 41, 2, ACCENT);

    buf
}

/// 把 RGBA 缓冲编码为 PNG 字节。
fn encode_png(buf: &[u8], size: u32) -> Vec<u8> {
    let mut png_bytes = Vec::new();
    {
        use png::Encoder;
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        let mut enc = Encoder::new(&mut cursor, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(buf).unwrap();
    }
    png_bytes
}

/// 盒式下采样:把 size×size 的 RGBA 缓冲降到 (size/factor)²。
/// 用于生成小于 BASE 逻辑网格的尺寸(如 32x32)。直接按 1x 渲染会让
/// 线宽/圆角相对变大,所以先渲成 128 再逐块平均。
/// 注意先预乘 alpha 再平均:否则透明区域的全黑 RGB 会混进来,边缘发暗。
fn downsample(buf: &[u8], size: u32, factor: u32) -> (Vec<u8>, u32) {
    let out = size / factor;
    let n = factor * factor;
    let mut dst = vec![0u8; (out * out * 4) as usize];

    for oy in 0..out {
        for ox in 0..out {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..factor {
                for sx in 0..factor {
                    let i = (((oy * factor + sy) * size + ox * factor + sx) * 4) as usize;
                    let pa = buf[i + 3] as u32;
                    r += buf[i] as u32 * pa / 255;
                    g += buf[i + 1] as u32 * pa / 255;
                    b += buf[i + 2] as u32 * pa / 255;
                    a += pa;
                }
            }
            let avg_a = a / n;
            let j = ((oy * out + ox) * 4) as usize;
            dst[j + 3] = avg_a as u8;
            if avg_a > 0 {
                // 反预乘回到直通 alpha
                let un = |c: u32| ((c / n * 255 / avg_a).min(255)) as u8;
                dst[j] = un(r);
                dst[j + 1] = un(g);
                dst[j + 2] = un(b);
            }
        }
    }

    (dst, out)
}

/// 内容与现有文件一致时跳过写入。
/// 关键:build.rs 每次运行都会执行,若无条件 fs::write 会刷新图标 mtime,
/// tauri dev 的文件监视器检测到 icons/ 变化 → 触发重建 → build.rs 再写 →
/// 无限重建循环。内容未变时不更新 mtime 即可断开循环。
fn write_png_if_changed(path: &std::path::Path, buf: &[u8], size: u32) {
    let png_bytes = encode_png(buf, size);
    if let Ok(existing) = std::fs::read(path) {
        if existing == png_bytes {
            return;
        }
    }
    std::fs::write(path, &png_bytes).unwrap();
}

fn main() {
    // 1. Tauri 2 上下文/资源构建
    tauri_build::build();

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    println!("cargo:rerun-if-changed=build.rs");

    // 2. 生成图标 PNG 到 icons/ 目录(供 Tauri 打包 + 运行时托盘/窗口使用)
    let icons_dir = manifest_dir.join("icons");
    std::fs::create_dir_all(&icons_dir).unwrap();

    // 32x32:由 128 网格下采样,保证文件名与像素尺寸一致(Linux hicolor 按名归档)
    let (buf32, size32) = downsample(&render(2), (BASE * 2) as u32, 4);
    write_png_if_changed(&icons_dir.join("32x32.png"), &buf32, size32);
    // 64x64:BASE 网格 1x 整数倍,是 default_window_icon / 托盘的实际来源
    write_png_if_changed(&icons_dir.join("64x64.png"), &render(1), BASE as u32);
    write_png_if_changed(
        &icons_dir.join("128x128.png"),
        &render(2),
        (BASE * 2) as u32,
    );
    write_png_if_changed(
        &icons_dir.join("128x128@2x.png"),
        &render(4),
        (BASE * 4) as u32,
    );
    write_png_if_changed(&icons_dir.join("icon.png"), &render(16), (BASE * 16) as u32);
}
