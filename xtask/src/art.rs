//! Procedural, SFW "artwork" for the fixtures: skies, dunes, soft lights.
//! Pure functions of (size, variant, time), so every run is identical. They
//! exist so the README media look like a library rather than a test card.

use image::{Rgb, RgbImage};

type C = [f32; 3];

const fn hex(v: u32) -> C {
    [
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
    ]
}

fn mix(a: C, b: C, t: f32) -> C {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn add(a: C, b: C, k: f32) -> C {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}

/// Piecewise-linear colour ramp over sorted stops.
fn ramp(stops: &[(f32, C)], t: f32) -> C {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        let ((t0, c0), (t1, c1)) = (w[0], w[1]);
        if t <= t1 {
            return mix(c0, c1, (t - t0) / (t1 - t0).max(1e-6));
        }
    }
    stops[stops.len() - 1].1
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Deterministic 0..1 hash of a cell.
fn hash(x: u32, y: u32, s: u32) -> f32 {
    let mut h = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263))
        .wrapping_add(s.wrapping_mul(2_246_822_519));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h & 0x00ff_ffff) as f32 / 16_777_216.0
}

fn px(c: C) -> Rgb<u8> {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Rgb([q(c[0]), q(c[1]), q(c[2])])
}

/// A ridge line in 0..1 image height for horizontal position `u`.
fn ridge(u: f32, base: f32, amp: f32, seed: f32) -> f32 {
    base - amp
        * (0.55 * (u * 5.1 + seed).sin()
            + 0.3 * (u * 11.7 + seed * 2.3).sin()
            + 0.15 * (u * 23.0 + seed * 4.1).sin())
}

/// Concentric flat-colour rings (GIF friendly: few colours).
pub fn rings(w: u32, h: u32) -> RgbImage {
    let pal = [0x2b1a4a, 0x5b2a7a, 0xa0306a, 0xff5c8a, 0xff8a6b, 0xffc48a];
    RgbImage::from_fn(w, h, |x, y| {
        let (dx, dy) = (x as f32 - w as f32 * 0.5, y as f32 - h as f32 * 0.62);
        let r = (dx * dx + dy * dy).sqrt() / h as f32;
        let k = ((r * 9.0) as usize).min(pal.len() - 1);
        px(hex(pal[pal.len() - 1 - k]))
    })
}

pub const FAMILIES: [&str; 6] = ["aurora", "bokeh", "dunes", "mesh", "ribbons", "sunset"];

/// One still: `family` from `FAMILIES`, `v` the variant (palette).
pub fn still(family: &str, w: u32, h: u32, v: u32) -> RgbImage {
    match family {
        "aurora" => aurora(w, h, v, 0.0),
        "bokeh" => bokeh(w, h, v, 0.0),
        "dunes" => dunes(w, h, v),
        "mesh" => mesh(w, h, v),
        "ribbons" => ribbons(w, h, v),
        _ => sunset(w, h, v, 0.0),
    }
}

/// Night sky with curtains of light over dark hills; `time` in seconds moves them.
pub fn aurora(w: u32, h: u32, v: u32, time: f32) -> RgbImage {
    let bands: [[C; 3]; 4] = [
        [hex(0x3dffa8), hex(0x29c7ff), hex(0xb06bff)],
        [hex(0x7cff6b), hex(0x3dffd0), hex(0xff6bd6)],
        [hex(0x29c7ff), hex(0x9b7bff), hex(0x3dffa8)],
        [hex(0xff8a6b), hex(0xffd36b), hex(0x6bd0ff)],
    ];
    let cols = bands[v as usize % 4];
    let a = w as f32 / h as f32;
    RgbImage::from_fn(w, h, |x, y| {
        let (u, t) = (x as f32 / w as f32, y as f32 / h as f32);
        let mut c = ramp(
            &[
                (0.0, hex(0x03050f)),
                (0.6, hex(0x0a1430)),
                (1.0, hex(0x112244)),
            ],
            t,
        );
        // Stars, sparser near the horizon.
        let s = hash(x / 2, y / 2, 7 + v);
        if s > 0.9965 - 0.002 * t {
            c = add(c, [1.0, 1.0, 1.0], (s - 0.996) * 180.0 * (1.0 - t));
        }
        let ua = u * a;
        for (k, col) in cols.iter().enumerate() {
            let kf = k as f32;
            let yc = 0.36
                + 0.08 * kf
                + 0.08 * (ua * 1.7 + kf * 1.7 + time * 0.9).sin()
                + 0.03 * (ua * 4.3 + kf + time * 1.7).sin();
            let d = t - yc;
            let fall = if d < 0.0 {
                (d * 4.0).exp()
            } else {
                (-d * 40.0).exp()
            };
            let warp = 3.0 * (ua * 1.3 + kf * 2.0 + time * 1.1).sin();
            let rays = 0.55
                + 0.3 * (ua * 17.0 + warp + kf * 1.1).sin()
                + 0.15 * (ua * 43.0 + warp * 1.7).sin();
            c = add(c, *col, fall * rays * (0.55 - 0.1 * kf));
        }
        let hill = ridge(u * a / 1.6, 0.86, 0.05, 1.3 + v as f32);
        if t > hill {
            c = mix(hex(0x050810), hex(0x0b1324), (t - hill) * 3.0);
        }
        px(c)
    })
}

/// Soft out-of-focus lights; `time` drifts them.
pub fn bokeh(w: u32, h: u32, v: u32, time: f32) -> RgbImage {
    let pals: [[C; 3]; 4] = [
        [hex(0xff8a6b), hex(0xffd36b), hex(0xff5c8a)],
        [hex(0x6bd0ff), hex(0x9b7bff), hex(0x3dffd0)],
        [hex(0xffb36b), hex(0xff6bd6), hex(0x8a6bff)],
        [hex(0x7cff6b), hex(0x29c7ff), hex(0xffd36b)],
    ];
    let pal = pals[v as usize % 4];
    let bg = [hex(0x1a1033), hex(0x081a2b), hex(0x22101c), hex(0x0b1f1a)][v as usize % 4];
    let a = w as f32 / h as f32;
    let lights: Vec<(f32, f32, f32, C)> = (0..22u32)
        .map(|i| {
            let x = hash(i, 1, v) * a + 0.03 * (time * 0.7 + i as f32).sin();
            let y = hash(i, 2, v) + 0.04 * (time * 0.5 + i as f32 * 1.3).cos();
            let r = 0.04 + 0.13 * hash(i, 3, v).powi(2);
            (x, y, r, pal[i as usize % 3])
        })
        .collect();
    RgbImage::from_fn(w, h, |x, y| {
        let (u, t) = (x as f32 / h as f32, y as f32 / h as f32);
        let mut c = mix(mix(bg, [1.0; 3], 0.06), hex(0x05060a), (t + u / a) * 0.45);
        for &(lx, ly, r, col) in &lights {
            let d = ((u - lx).powi(2) + (t - ly).powi(2)).sqrt();
            let disc = smooth(r, r * 0.9, d);
            let rim = smooth(r * 0.08, 0.0, (d - r * 0.93).abs()) * 0.5;
            c = add(c, col, (disc * 0.38 + rim * 0.4) * (0.6 + 0.4 * r / 0.17));
        }
        px(c)
    })
}

/// Layered dunes under a soft sky.
pub fn dunes(w: u32, h: u32, v: u32) -> RgbImage {
    let pals: [(C, C, C, C); 4] = [
        (hex(0xffd9a8), hex(0xffefd6), hex(0xe9965a), hex(0x7a3b22)),
        (hex(0xf6b2c8), hex(0xffe3d1), hex(0xd8708a), hex(0x4a1f3b)),
        (hex(0x9fc3ff), hex(0xe4ecff), hex(0x6f7fd8), hex(0x1d2150)),
        (hex(0xffc58a), hex(0xfff1c9), hex(0xd98a3a), hex(0x5a2a10)),
    ];
    let (sky0, sky1, lit, shade) = pals[v as usize % 4];
    let a = w as f32 / h as f32;
    RgbImage::from_fn(w, h, |x, y| {
        let (u, t) = (x as f32 / w as f32 * a, y as f32 / h as f32);
        let mut c = mix(sky1, sky0, t * 1.6);
        for k in 0..5 {
            let kf = k as f32;
            let top = ridge(
                u * (0.6 + 0.15 * kf),
                0.45 + 0.11 * kf,
                0.06,
                kf * 2.7 + v as f32,
            );
            if t > top {
                let depth = kf / 4.0;
                let base = mix(mix(lit, sky0, 0.45 - depth * 0.45), shade, depth * 0.85);
                // Lit just under the crest, shading towards the next layer.
                c = mix(mix(base, sky1, 0.25 * (1.0 - depth)), base, (t - top) * 6.0);
                let slope = (u * (0.6 + 0.15 * kf) * 5.1 + kf * 2.7 + v as f32).cos();
                c = mix(c, shade, (0.18 * slope).max(0.0));
            }
        }
        px(c)
    })
}

/// A smooth mesh gradient between four coloured points, with film grain.
pub fn mesh(w: u32, h: u32, v: u32) -> RgbImage {
    let pals: [[C; 4]; 4] = [
        [hex(0xff8a6b), hex(0x6b5bff), hex(0xffd36b), hex(0x1e1440)],
        [hex(0x3dffd0), hex(0x1b3dff), hex(0xff6bd6), hex(0x0a1430)],
        [hex(0xffb3c7), hex(0xff6b3d), hex(0x8affc1), hex(0x40143a)],
        [hex(0x9b7bff), hex(0x29c7ff), hex(0xffe08a), hex(0x101c3a)],
    ];
    let pal = pals[v as usize % 4];
    let pts = [(0.15, 0.2), (0.85, 0.15), (0.75, 0.85), (0.2, 0.8)];
    RgbImage::from_fn(w, h, |x, y| {
        let (u, t) = (x as f32 / w as f32, y as f32 / h as f32);
        let (u2, t2) = (
            u + 0.06 * (t * 6.0 + v as f32).sin(),
            t + 0.06 * (u * 5.0 + v as f32).cos(),
        );
        let mut acc = [0.0; 3];
        let mut wsum = 0.0;
        for (i, &(px_, py)) in pts.iter().enumerate() {
            let d2 = (u2 - px_).powi(2) + (t2 - py).powi(2);
            let wgt = 1.0 / (d2 + 0.02).powf(1.6);
            acc = add(acc, pal[i], wgt);
            wsum += wgt;
        }
        let g = (hash(x, y, v) - 0.5) * 0.035;
        px([acc[0] / wsum + g, acc[1] / wsum + g, acc[2] / wsum + g])
    })
}

/// Flowing layered bands, like paper cut-outs.
pub fn ribbons(w: u32, h: u32, v: u32) -> RgbImage {
    let pals: [[u32; 6]; 4] = [
        [0x1b1036, 0x3b1d6e, 0x7a2f8f, 0xc2427e, 0xff7a6b, 0xffc48a],
        [0x062033, 0x0b4a5e, 0x12807f, 0x2fb59a, 0x8fe3b0, 0xe6f7c8],
        [0x2a0f0f, 0x6e1f2b, 0xb8342f, 0xf06a3a, 0xffa94d, 0xffe08a],
        [0x0d0f2e, 0x1f2b78, 0x3a52c4, 0x6b8cff, 0xa8c4ff, 0xe8efff],
    ];
    let pal = pals[v as usize % 4];
    let a = w as f32 / h as f32;
    RgbImage::from_fn(w, h, |x, y| {
        let (u, t) = (x as f32 / w as f32 * a, y as f32 / h as f32);
        let s = t
            + 0.09 * (u * 2.2 + v as f32).sin()
            + 0.05 * (u * 4.7 + t * 3.0 + v as f32 * 2.0).sin();
        let n = s * 5.5;
        let band = (n.floor().clamp(0.0, 5.0)) as usize;
        let f = n - n.floor();
        let base = hex(pal[band]);
        // A soft shadow under each edge sells the layering.
        let c = mix(mix(base, [0.0; 3], 0.35), base, smooth(0.0, 0.25, f));
        px(c)
    })
}

/// Sun over layered mountains; `time` lowers the sun.
pub fn sunset(w: u32, h: u32, v: u32, time: f32) -> RgbImage {
    let skies: [[u32; 4]; 4] = [
        [0x2b1a4a, 0x8a3b6b, 0xff7a59, 0xffc78a],
        [0x0f2350, 0x5b3b8a, 0xff6b8a, 0xffd0a0],
        [0x1a2a4a, 0x3a6b9a, 0xffa86b, 0xfff0c0],
        [0x2a0f3a, 0xa0306a, 0xff8a3d, 0xffe08a],
    ];
    let sk = skies[v as usize % 4];
    let mount = [hex(0x6b3b6b), hex(0x45244f), hex(0x251530)];
    let a = w as f32 / h as f32;
    let (sx, sy) = (0.3 + 0.13 * v as f32, 0.48 + time * 0.02);
    RgbImage::from_fn(w, h, |x, y| {
        let (u, t) = (x as f32 / w as f32, y as f32 / h as f32);
        let mut c = ramp(
            &[
                (0.0, hex(sk[0])),
                (0.3, hex(sk[1])),
                (0.55, hex(sk[2])),
                (0.68, hex(sk[3])),
            ],
            t,
        );
        let d = (((u - sx) * a).powi(2) + (t - sy).powi(2)).sqrt();
        c = add(c, hex(0xffe7b0), (-d * 7.0).exp() * 0.45);
        c = mix(c, hex(0xfff3d6), smooth(0.075, 0.07, d));
        for (k, m) in mount.iter().enumerate() {
            let kf = k as f32;
            let top = ridge(
                u * a * (0.7 + 0.3 * kf),
                0.58 + 0.1 * kf,
                0.08 - 0.015 * kf,
                kf * 3.1 + v as f32,
            );
            if t > top {
                let haze = mix(*m, hex(sk[2]), 0.35 - 0.15 * kf);
                c = mix(haze, *m, (t - top) * 4.0);
            }
        }
        px(c)
    })
}
