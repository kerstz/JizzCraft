//! Repo automation: `cargo xtask fixtures | icons | screenshots`.
//! Everything generated here is synthetic and SFW.

use image::{Rgb, RgbImage};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let res = match args.next().as_deref() {
        Some("fixtures") => {
            let out = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("fixtures"));
            fixtures(&out)
        }
        Some("icons") => icons(),
        Some("screenshots") => screenshots(),
        _ => Err("usage: cargo xtask fixtures [OUT_DIR] | icons | screenshots".into()),
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn hsv(h: f32, s: f32, v: f32) -> Rgb<u8> {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 % 6 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Rgb([
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    ])
}

fn gradient(w: u32, h: u32, hue: f32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let t = x as f32 / w as f32;
        let u = y as f32 / h as f32;
        hsv(
            (hue + t * 60.0) % 360.0,
            0.55 + 0.3 * u,
            0.35 + 0.6 * (1.0 - u),
        )
    })
}

fn pattern(w: u32, h: u32, kind: u32, hue: f32) -> RgbImage {
    let a = hsv(hue, 0.6, 0.9);
    let b = hsv((hue + 180.0) % 360.0, 0.4, 0.2);
    RgbImage::from_fn(w, h, |x, y| {
        let on = match kind % 3 {
            0 => ((x / 40) + (y / 40)) % 2 == 0,
            1 => ((x + y) / 30) % 2 == 0,
            _ => {
                let (dx, dy) = (x as f32 - w as f32 / 2.0, y as f32 - h as f32 / 2.0);
                (((dx * dx + dy * dy).sqrt() / 36.0) as u32).is_multiple_of(2)
            }
        };
        if on { a } else { b }
    })
}

fn has_element(name: &str) -> bool {
    Command::new("gst-inspect-1.0")
        .args([name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gst(args: &[String]) -> Result<(), String> {
    let st = Command::new("gst-launch-1.0")
        .arg("-q")
        .args(args)
        .status()
        .map_err(|e| format!("gst-launch-1.0 not runnable: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("gst-launch-1.0 failed: {args:?}"))
    }
}

struct VideoSpec {
    file: &'static str,
    w: u32,
    h: u32,
    fps: u32,
    secs: u32,
    h264: bool,
    audio: bool,
}

fn video(out: &Path, v: &VideoSpec) -> Result<(), String> {
    let path = out.join(v.file).to_string_lossy().into_owned();
    let frames = v.fps * v.secs;
    let mut a: Vec<String> = vec![
        "videotestsrc".into(),
        format!("num-buffers={frames}"),
        "pattern=ball".into(),
        // Craft palette: raised-surface background, accent-blue ball.
        "background-color=0xff1e2327".into(),
        "foreground-color=0xff7aa2f7".into(),
        "!".into(),
        format!(
            "video/x-raw,width={},height={},framerate={}/1",
            v.w, v.h, v.fps
        ),
        "!".into(),
        "videoconvert".into(),
        "!".into(),
        // 4:2:0 like real-world files; hardware decoders only take this.
        "video/x-raw,format=I420".into(),
        "!".into(),
    ];
    let (venc, mux, aenc): (&[&str], &str, &[&str]) = if v.h264 {
        (
            &[
                "x264enc",
                "speed-preset=ultrafast",
                "tune=zerolatency",
                "key-int-max=60",
                "bitrate=8000",
                "!",
                "h264parse",
            ],
            "mp4mux",
            &["avenc_aac"],
        )
    } else {
        (
            &[
                "vp8enc",
                "deadline=1",
                "cpu-used=8",
                "keyframe-max-dist=60",
                "!",
                "queue",
            ],
            "webmmux",
            &["vorbisenc"],
        )
    };
    a.extend(venc.iter().map(|s| s.to_string()));
    a.extend([
        "!".into(),
        mux.into(),
        "name=m".into(),
        "!".into(),
        "filesink".into(),
        format!("location={path}"),
    ]);
    if v.audio {
        a.extend(
            [
                "audiotestsrc",
                "wave=sine",
                "freq=330",
                "volume=0.15",
                &format!("num-buffers={}", v.secs * 44100 / 1024 + 1),
                "!",
                "audioconvert",
                "!",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        a.extend(aenc.iter().map(|s| s.to_string()));
        a.extend(["!".into(), "m.".into()]);
    }
    gst(&a)
}

fn fixtures(out: &Path) -> Result<(), String> {
    std::fs::create_dir_all(out.join("images/été ünï")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(out.join("videos")).map_err(|e| e.to_string())?;

    let sizes = [
        (1280, 720),
        (720, 1080),
        (800, 800),
        (1920, 1080),
        (640, 480),
        (1024, 683),
    ];
    let mut n = 0;
    for i in 0..24u32 {
        let (w, h) = sizes[i as usize % sizes.len()];
        let hue = (i * 37) as f32 % 360.0;
        let img = if i % 2 == 0 {
            gradient(w, h, hue)
        } else {
            pattern(w, h, i, hue)
        };
        let ext = ["png", "jpg", "png", "bmp"][i as usize % 4];
        let sub = if i % 5 == 0 {
            "images/été ünï"
        } else {
            "images"
        };
        let kind = if i % 2 == 0 { "gradient" } else { "pattern" };
        let path = out.join(sub).join(format!("{kind}-{i:02}.{ext}"));
        img.save(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        n += 1;
    }
    // A GIF and a WebP for format coverage.
    let g = pattern(320, 240, 2, 200.0);
    g.save(out.join("images/circles.gif"))
        .map_err(|e| e.to_string())?;
    g.save(out.join("images/circles.webp"))
        .map_err(|e| e.to_string())?;
    n += 2;
    println!("images: {n}");

    for f in ["videotestsrc", "vp8enc", "webmmux"] {
        if !has_element(f) {
            return Err(format!(
                "GStreamer element `{f}` missing: install gst-plugins-base/good (see README)"
            ));
        }
    }
    let h264 = has_element("x264enc") && has_element("mp4mux") && has_element("h264parse");
    let aac = has_element("avenc_aac");
    let mut specs = vec![
        VideoSpec {
            file: "clip-320x240-vp8.webm",
            w: 320,
            h: 240,
            fps: 30,
            secs: 6,
            h264: false,
            audio: has_element("vorbisenc"),
        },
        VideoSpec {
            file: "clip-720p30-vp8.webm",
            w: 1280,
            h: 720,
            fps: 30,
            secs: 10,
            h264: false,
            audio: has_element("vorbisenc"),
        },
    ];
    if h264 {
        specs.push(VideoSpec {
            file: "bench-1080p30.mp4",
            w: 1920,
            h: 1080,
            fps: 30,
            secs: 35,
            h264: true,
            audio: aac,
        });
        specs.push(VideoSpec {
            file: "bench-1080p60.mp4",
            w: 1920,
            h: 1080,
            fps: 60,
            secs: 35,
            h264: true,
            audio: aac,
        });
        specs.push(VideoSpec {
            file: "bench-4k30.mp4",
            w: 3840,
            h: 2160,
            fps: 30,
            secs: 35,
            h264: true,
            audio: aac,
        });
    } else {
        println!("x264enc not available: encoding bench clips with vp8 instead");
        specs.push(VideoSpec {
            file: "bench-1080p30.webm",
            w: 1920,
            h: 1080,
            fps: 30,
            secs: 35,
            h264: false,
            audio: false,
        });
        specs.push(VideoSpec {
            file: "bench-1080p60.webm",
            w: 1920,
            h: 1080,
            fps: 60,
            secs: 35,
            h264: false,
            audio: false,
        });
        specs.push(VideoSpec {
            file: "bench-4k30.webm",
            w: 3840,
            h: 2160,
            fps: 30,
            secs: 35,
            h264: false,
            audio: false,
        });
    }
    for s in &specs {
        print!("video {} … ", s.file);
        video(&out.join("videos"), s)?;
        println!("ok");
    }
    println!("fixtures written to {}", out.display());
    Ok(())
}

/// Rasterise the SVG icons to hicolor PNGs with resvg.
fn icons() -> Result<(), String> {
    use resvg::{tiny_skia, usvg};
    const SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];
    let jobs = [
        ("assets/icons/jizzcraft.svg", "org.jizzcraft.JizzCraft"),
        ("assets/icons/viewer.svg", "org.jizzcraft.JizzCraft.viewer"),
    ];
    for (svg, name) in jobs {
        let data = std::fs::read(svg).map_err(|e| format!("{svg}: {e}"))?;
        let tree = usvg::Tree::from_data(&data, &usvg::Options::default())
            .map_err(|e| format!("{svg}: {e}"))?;
        for size in SIZES {
            let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("pixmap")?;
            let scale = size as f32 / tree.size().width();
            resvg::render(
                &tree,
                tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            let dir = PathBuf::from(format!("assets/icons/hicolor/{size}x{size}/apps"));
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            pixmap
                .save_png(dir.join(format!("{name}.png")))
                .map_err(|e| e.to_string())?;
        }
        println!("icons: {name} ({} sizes)", SIZES.len());
    }
    // Embedded in the binary (window + empty state).
    let peach = std::fs::read("assets/icons/hicolor/512x512/apps/org.jizzcraft.JizzCraft.png")
        .map_err(|e| e.to_string())?;
    std::fs::write("assets/icons/jizzcraft-512.png", peach).map_err(|e| e.to_string())?;
    let viewer =
        std::fs::read("assets/icons/hicolor/256x256/apps/org.jizzcraft.JizzCraft.viewer.png")
            .map_err(|e| e.to_string())?;
    std::fs::write("assets/icons/viewer-256.png", viewer).map_err(|e| e.to_string())?;
    Ok(())
}

/// README screenshots: rendered by the app itself (`--screenshot`) on the
/// synthetic fixtures, with a throw-away library and config.
fn screenshots() -> Result<(), String> {
    if !Path::new("fixtures/images").exists() {
        return Err("run `cargo xtask fixtures` first".into());
    }
    let st = Command::new("cargo")
        .args(["build", "--release", "-p", "jizzcraft"])
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err("build failed".into());
    }
    let out = PathBuf::from("docs/screenshots");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let tmp = PathBuf::from("target/xtask-screenshots");
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let bin = "target/release/jizzcraft";

    struct Shot {
        file: &'static str,
        args: Vec<&'static str>,
        fresh_db: bool,
        adult: bool,
    }
    let shots = [
        Shot {
            file: "gate.png",
            args: vec![],
            fresh_db: true,
            adult: false,
        },
        Shot {
            file: "empty.png",
            args: vec![],
            fresh_db: true,
            adult: true,
        },
        Shot {
            file: "library.png",
            args: vec!["--import", "fixtures"],
            fresh_db: true,
            adult: true,
        },
        Shot {
            file: "viewer.png",
            args: vec![
                "--import",
                "fixtures/images",
                "--tab",
                "images",
                "--open",
                "17",
            ],
            fresh_db: true,
            adult: true,
        },
        Shot {
            file: "player.png",
            args: vec![
                "--import",
                "fixtures/videos",
                "--tab",
                "videos",
                "--open",
                "1",
                "--cmd",
                "player.seek={\"pos\":12}",
                "--cmd",
                "markers.add={\"pos_ms\":20000,\"label\":\"scene\"}",
                "--cmd",
                "player.loop={\"a_ms\":8000,\"b_ms\":16000}",
                "--cmd",
                "player.speed={\"value\":1.5}",
                "--cmd",
                "player.preview={\"pos\":22}",
            ],
            fresh_db: true,
            adult: true,
        },
        Shot {
            file: "triage.png",
            args: vec!["--import", "fixtures/images", "--cmd", "triage.start"],
            fresh_db: true,
            adult: true,
        },
        Shot {
            file: "rating.png",
            args: vec![
                "--import",
                "fixtures/images",
                "--cmd",
                "view.select.move={\"dx\":2,\"dy\":1}",
                "--cmd",
                "media.rate={\"rating\":4}",
            ],
            fresh_db: true,
            adult: true,
        },
        Shot {
            file: "discreet.png",
            args: vec![
                "--import",
                "fixtures/images",
                "--cmd",
                "app.discreet.toggle",
            ],
            fresh_db: true,
            adult: true,
        },
    ];
    for s in shots {
        let db = tmp.join(format!("{}.db", s.file));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", db.display()));
        }
        let _ = s.fresh_db;
        let mut cmd = Command::new(bin);
        cmd.arg("--db")
            .arg(&db)
            .args(&s.args)
            .arg("--screenshot")
            .arg(out.join(s.file));
        if s.adult {
            cmd.arg("--assume-adult");
        }
        let st = cmd.status().map_err(|e| e.to_string())?;
        if !st.success() {
            return Err(format!("screenshot {} failed", s.file));
        }
        println!("screenshot: {}", s.file);
    }
    Ok(())
}
