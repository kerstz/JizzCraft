//! Repo automation: `cargo xtask fixtures | icons | screenshots`.
//! Everything generated here is synthetic and SFW.
//! The most explicit thing in the fixtures is a test pattern with ambitions.

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
        Some("dist") => dist(args.collect()),
        Some("check-packaging") => check_packaging(),
        _ => Err(
            "usage: cargo xtask fixtures [OUT_DIR] | icons | screenshots | dist [--network] \
             [--prefix P] [--destdir D] [--no-build] [--tarball] | check-packaging"
                .into(),
        ),
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
        // Chroma subsampling: throwing away three quarters of the colour and
        // getting away with it since the eighties.
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
        ("assets/icons/viewer.svg", "local.Viewer"),
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
        // Scalable copy for desktops that prefer it.
        let sc = PathBuf::from("assets/icons/hicolor/scalable/apps");
        std::fs::create_dir_all(&sc).map_err(|e| e.to_string())?;
        std::fs::write(sc.join(format!("{name}.svg")), &data).map_err(|e| e.to_string())?;
        println!("icons: {name} ({} sizes + scalable)", SIZES.len());
    }
    // Windows .ico files: PNG-compressed entries, 16 to 256 px.
    for (name, out) in [
        ("org.jizzcraft.JizzCraft", "assets/icons/jizzcraft.ico"),
        ("local.Viewer", "assets/icons/viewer.ico"),
    ] {
        let sizes = [16u32, 32, 48, 64, 128, 256];
        let mut pngs = Vec::new();
        for s in sizes {
            pngs.push(
                std::fs::read(format!("assets/icons/hicolor/{s}x{s}/apps/{name}.png"))
                    .map_err(|e| e.to_string())?,
            );
        }
        let mut ico = Vec::new();
        ico.extend_from_slice(&[0, 0, 1, 0]);
        ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * sizes.len() as u32;
        for (s, png) in sizes.iter().zip(&pngs) {
            let dim = if *s >= 256 { 0u8 } else { *s as u8 };
            ico.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]);
            ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
            ico.extend_from_slice(&offset.to_le_bytes());
            offset += png.len() as u32;
        }
        for png in &pngs {
            ico.extend_from_slice(png);
        }
        std::fs::write(out, ico).map_err(|e| e.to_string())?;
    }
    // Embedded in the binary (window + empty state).
    let peach = std::fs::read("assets/icons/hicolor/512x512/apps/org.jizzcraft.JizzCraft.png")
        .map_err(|e| e.to_string())?;
    std::fs::write("assets/icons/jizzcraft-512.png", peach).map_err(|e| e.to_string())?;
    let viewer = std::fs::read("assets/icons/hicolor/256x256/apps/local.Viewer.png")
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

/// The one staging tree every package is made from. Layout follows the FHS:
/// `DESTDIR/PREFIX/{bin,share/...}`.
fn dist(args: Vec<String>) -> Result<(), String> {
    let (mut network, mut no_build, mut tarball) = (false, false, false);
    let (mut prefix, mut destdir) = ("/usr".to_owned(), PathBuf::from("target/stage"));
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--network" => network = true,
            "--no-build" => no_build = true,
            "--tarball" => tarball = true,
            "--prefix" => prefix = it.next().ok_or("--prefix needs a value")?,
            "--destdir" => {
                destdir = it
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--destdir needs a value")?
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if !no_build {
        let mut c = Command::new("cargo");
        c.args([
            "build",
            "--release",
            "--locked",
            "-p",
            "jizzcraft",
            "-p",
            "jizzcraft-cli",
        ]);
        if network {
            c.args(["--features", "jizzcraft/network,jizzcraft-cli/network"]);
        }
        if !c.status().map_err(|e| e.to_string())?.success() {
            return Err("build failed".into());
        }
    }
    let root = destdir.join(prefix.trim_start_matches('/'));
    let put = |from: &str, to: &str, mode: u32| -> Result<(), String> {
        let dst = root.join(to);
        std::fs::create_dir_all(dst.parent().ok_or("no parent")?).map_err(|e| e.to_string())?;
        std::fs::copy(from, &dst).map_err(|e| format!("{from}: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode))
                .map_err(|e| e.to_string())?;
        }
        let _ = mode;
        Ok(())
    };
    put("target/release/jizzcraft", "bin/jizzcraft", 0o755)?;
    put("target/release/jizzcraft-cli", "bin/jizzcraft-cli", 0o755)?;
    for d in ["org.jizzcraft.JizzCraft.desktop", "local.Viewer.desktop"] {
        put(
            &format!("assets/{d}"),
            &format!("share/applications/{d}"),
            0o644,
        )?;
    }
    if network {
        put(
            "assets/org.jizzcraft.JizzCraft-magnet.desktop",
            "share/applications/org.jizzcraft.JizzCraft-magnet.desktop",
            0o644,
        )?;
    }
    put(
        "assets/org.jizzcraft.JizzCraft.metainfo.xml",
        "share/metainfo/org.jizzcraft.JizzCraft.metainfo.xml",
        0o644,
    )?;
    for size in [
        "16x16", "24x24", "32x32", "48x48", "64x64", "128x128", "256x256", "512x512",
    ] {
        for n in ["org.jizzcraft.JizzCraft", "local.Viewer"] {
            put(
                &format!("assets/icons/hicolor/{size}/apps/{n}.png"),
                &format!("share/icons/hicolor/{size}/apps/{n}.png"),
                0o644,
            )?;
        }
    }
    for n in ["org.jizzcraft.JizzCraft", "local.Viewer"] {
        put(
            &format!("assets/icons/hicolor/scalable/apps/{n}.svg"),
            &format!("share/icons/hicolor/scalable/apps/{n}.svg"),
            0o644,
        )?;
    }
    put(
        "assets/gst-packages.tsv",
        "share/jizzcraft/gst-packages.tsv",
        0o644,
    )?;
    put("README.md", "share/doc/jizzcraft/README.md", 0o644)?;
    for l in ["LICENSE-MIT", "LICENSE-APACHE"] {
        if Path::new(l).exists() {
            put(l, &format!("share/licenses/jizzcraft/{l}"), 0o644)?;
        }
    }
    // Man page and completions come from the CLI itself.
    let cli = "target/release/jizzcraft-cli";
    let gen_out = |args: &[&str]| -> Result<Vec<u8>, String> {
        let o = Command::new(cli)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(format!("{cli} {args:?} failed"));
        }
        Ok(o.stdout)
    };
    let write = |rel: &str, bytes: &[u8]| -> Result<(), String> {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().ok_or("no parent")?).map_err(|e| e.to_string())?;
        std::fs::write(p, bytes).map_err(|e| e.to_string())
    };
    write("share/man/man1/jizzcraft-cli.1", &gen_out(&["gen", "man"])?)?;
    for (shell, rel) in [
        ("bash", "share/bash-completion/completions/jizzcraft-cli"),
        ("zsh", "share/zsh/site-functions/_jizzcraft-cli"),
        ("fish", "share/fish/vendor_completions.d/jizzcraft-cli.fish"),
    ] {
        write(rel, &gen_out(&["gen", "completions", shell])?)?;
    }
    // The desktop app has a short hand-written page.
    put("assets/jizzcraft.1", "share/man/man1/jizzcraft.1", 0o644)?;
    println!("dist: staged in {}", destdir.display());
    if tarball {
        let ver = env!("CARGO_PKG_VERSION");
        let arch = std::env::consts::ARCH;
        let name = format!(
            "jizzcraft{}-{ver}-{arch}-linux",
            if network { "-network" } else { "" }
        );
        std::fs::create_dir_all("dist").map_err(|e| e.to_string())?;
        let top = PathBuf::from("target/tarball").join(&name);
        let _ = std::fs::remove_dir_all(&top);
        std::fs::create_dir_all(top.parent().ok_or("no parent")?).map_err(|e| e.to_string())?;
        let st = Command::new("cp")
            .arg("-a")
            .arg(&root)
            .arg(&top)
            .status()
            .map_err(|e| e.to_string())?;
        if !st.success() {
            return Err("cp failed".into());
        }
        let st = Command::new("tar")
            .args(["-C", "target/tarball", "-cJf"])
            .arg(format!("dist/{name}.tar.xz"))
            .arg(&name)
            .status()
            .map_err(|e| e.to_string())?;
        if !st.success() {
            return Err("tar failed".into());
        }
        println!("dist: dist/{name}.tar.xz");
    }
    Ok(())
}

/// Packaging files must agree with each other: every file `dist` stages is
/// listed by the deb metadata, and the Arch and Fedora recipes call `dist`.
fn check_packaging() -> Result<(), String> {
    let read = |p: &str| std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"));
    let deb = read("apps/jizzcraft/Cargo.toml")?;
    let mut missing = Vec::new();
    for needle in [
        "stage/usr/bin/jizzcraft-cli",
        "share/applications/*.desktop",
        "share/metainfo/*.xml",
        "share/man/man1/*",
        "bash-completion/completions/*",
        "zsh/site-functions/*",
        "fish/vendor_completions.d/*",
        "share/jizzcraft/*",
    ] {
        if !deb.contains(needle) {
            missing.push(format!("deb metadata lacks {needle}"));
        }
    }
    for f in [
        "packaging/arch/jizzcraft/PKGBUILD",
        "packaging/fedora/jizzcraft.spec",
    ] {
        if !read(f)?.contains("xtask") {
            missing.push(format!("{f} does not stage with `cargo xtask dist`"));
        }
    }
    for a in [
        "assets/gst-packages.tsv",
        "assets/jizzcraft.1",
        "assets/icons/hicolor/scalable/apps/local.Viewer.svg",
    ] {
        if !Path::new(a).exists() {
            missing.push(format!("missing {a}"));
        }
    }
    if missing.is_empty() {
        println!("check-packaging: ok");
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}
