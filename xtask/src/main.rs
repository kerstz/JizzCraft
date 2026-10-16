//! Repo automation: `cargo xtask fixtures | icons | screenshots`.
//! Everything generated here is synthetic and SFW.
//! The most explicit thing in the fixtures is a test pattern with ambitions.

mod art;
mod e2e;
mod perf;

use image::RgbImage;
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
        Some("e2e") => e2e::run(args.collect()),
        Some("perf") => perf::run(args.collect()),
        _ => Err(
            "usage: cargo xtask fixtures [OUT_DIR] | icons | screenshots | dist [--edition offline|online] [--browser] \
             [--prefix P] [--destdir D] [--no-build] [--tarball] | check-packaging | e2e [--allow-skips] [FILTER...] | perf [--player [FILE]]"
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
    /// Animated artwork rendered at (width, height, variant) and scaled up by
    /// GStreamer; `None` keeps the plain test pattern (bench clips).
    art: Option<(Art, u32, u32, u32)>,
}

type Art = fn(u32, u32, u32, f32) -> RgbImage;

/// Raw RGB frames of an animated artwork, for `rawvideoparse`.
fn art_frames(path: &Path, v: &VideoSpec) -> Result<Vec<String>, String> {
    use std::io::Write;
    let Some((render, w, h, variant)) = v.art else {
        return Ok(Vec::new());
    };
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut f = std::io::BufWriter::new(f);
    for i in 0..v.fps * v.secs {
        let img = render(w, h, variant, i as f32 / v.fps as f32);
        f.write_all(img.as_raw()).map_err(|e| e.to_string())?;
    }
    f.flush().map_err(|e| e.to_string())?;
    Ok(vec![
        "filesrc".into(),
        format!("location={}", path.to_string_lossy()),
        "!".into(),
        "rawvideoparse".into(),
        format!("width={w}"),
        format!("height={h}"),
        "format=rgb".into(),
        format!("framerate={}/1", v.fps),
        "!".into(),
        "videoconvert".into(),
        "!".into(),
        "videoscale".into(),
        "!".into(),
    ])
}

fn video(out: &Path, v: &VideoSpec) -> Result<(), String> {
    let path = out.join(v.file).to_string_lossy().into_owned();
    let frames = v.fps * v.secs;
    let raw = out.join(format!(".{}.rgb", v.file));
    let mut a = art_frames(&raw, v)?;
    if a.is_empty() {
        a.extend([
            "videotestsrc".into(),
            format!("num-buffers={frames}"),
            "pattern=ball".into(),
            // Craft palette: raised-surface background, accent-blue ball.
            "background-color=0xff1e2327".into(),
            "foreground-color=0xff7aa2f7".into(),
            "!".into(),
        ]);
    }
    a.extend([
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
    ]);
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
                "target-bitrate=2500000",
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
    let res = gst(&a);
    let _ = std::fs::remove_file(&raw);
    res
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
    // Six families of procedural artwork, four palettes each, so the grid,
    // the name filter and the viewer have something to show.
    let mut n = 0;
    for i in 0..24u32 {
        let (family, v) = (art::FAMILIES[i as usize % 6], i / 6);
        let (w, h) = sizes[(i + v) as usize % sizes.len()];
        let img = art::still(family, w, h, v);
        let ext = ["png", "jpg", "png", "bmp"][i as usize % 4];
        let sub = if i % 5 == 0 {
            "images/été ünï"
        } else {
            "images"
        };
        let path = out.join(sub).join(format!("{family}-{:02}.{ext}", v + 1));
        img.save(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        n += 1;
    }
    // A GIF and a WebP for format coverage.
    let g = art::rings(320, 240);
    g.save(out.join("images/rings.gif"))
        .map_err(|e| e.to_string())?;
    g.save(out.join("images/rings.webp"))
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
    // Without rawvideoparse (gst-plugins-bad) the clips fall back to the ball.
    let raw = has_element("rawvideoparse");
    let mut specs = vec![
        VideoSpec {
            file: "clip-320x240-vp8.webm",
            w: 320,
            h: 240,
            fps: 30,
            secs: 6,
            h264: false,
            audio: has_element("vorbisenc"),
            art: raw.then_some((art::bokeh as Art, 320, 240, 1)),
        },
        VideoSpec {
            file: "clip-640x360-vp8.webm",
            w: 640,
            h: 360,
            fps: 30,
            secs: 8,
            h264: false,
            audio: has_element("vorbisenc"),
            art: raw.then_some((art::sunset as Art, 640, 360, 1)),
        },
        VideoSpec {
            file: "clip-720p30-vp8.webm",
            w: 1280,
            h: 720,
            fps: 30,
            secs: 10,
            h264: false,
            audio: has_element("vorbisenc"),
            art: raw.then_some((art::aurora as Art, 640, 360, 0)),
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
            art: None,
        });
        specs.push(VideoSpec {
            file: "bench-1080p60.mp4",
            w: 1920,
            h: 1080,
            fps: 60,
            secs: 35,
            h264: true,
            audio: aac,
            art: None,
        });
        specs.push(VideoSpec {
            file: "bench-4k30.mp4",
            w: 3840,
            h: 2160,
            fps: 30,
            secs: 35,
            h264: true,
            audio: aac,
            art: None,
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
            art: None,
        });
        specs.push(VideoSpec {
            file: "bench-1080p60.webm",
            w: 1920,
            h: 1080,
            fps: 60,
            secs: 35,
            h264: false,
            audio: false,
            art: None,
        });
        specs.push(VideoSpec {
            file: "bench-4k30.webm",
            w: 3840,
            h: 2160,
            fps: 30,
            secs: 35,
            h264: false,
            audio: false,
            art: None,
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
        ("assets/icons/jizzcraft.svg", "io.github.kerstz.JizzCraft"),
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
        ("io.github.kerstz.JizzCraft", "assets/icons/jizzcraft.ico"),
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
    let peach = std::fs::read("assets/icons/hicolor/512x512/apps/io.github.kerstz.JizzCraft.png")
        .map_err(|e| e.to_string())?;
    std::fs::write("assets/icons/jizzcraft-512.png", peach).map_err(|e| e.to_string())?;
    let viewer = std::fs::read("assets/icons/hicolor/256x256/apps/local.Viewer.png")
        .map_err(|e| e.to_string())?;
    std::fs::write("assets/icons/viewer-256.png", viewer).map_err(|e| e.to_string())?;
    Ok(())
}

/// README screenshots: rendered by the app itself (`--screenshot`) on the
/// synthetic fixtures, with a throw-away library, config and cache, in English
/// at the default 1280x800 window (on a tiling compositor, run a nested one,
/// see `docs/media/make-demo.sh`). The library is imported once, so no shot
/// carries the import toast.
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
    let tmp = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join("target/xtask-screenshots");
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let bin = "target/release/jizzcraft";

    // The fixture images and the short artwork clips; the bench clips are
    // plain test patterns and would only fill the grid.
    let lib = tmp.join("Media");
    copy_tree(Path::new("fixtures/images"), &lib.join("images"))?;
    std::fs::create_dir_all(lib.join("videos")).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir("fixtures/videos").map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("clip-") {
            std::fs::copy(e.path(), lib.join("videos").join(&name)).map_err(|e| e.to_string())?;
        }
    }

    // Screenshot runs use the default config (inspector open) and never
    // read or write one; `--home` keeps the thumbnails out of the user's cache.
    let run = |db: &Path, adult: bool, args: &[&str], png: &Path| {
        let mut cmd = Command::new(bin);
        cmd.arg("--home")
            .arg(&home)
            .args(["--lang", "en", "--db"])
            .arg(db)
            .args(args)
            .arg("--screenshot")
            .arg(png);
        if adult {
            cmd.arg("--assume-adult");
        }
        let st = cmd.status().map_err(|e| e.to_string())?;
        if st.success() && png.exists() {
            Ok(())
        } else {
            Err(format!("screenshot {} failed", png.display()))
        }
    };
    let base = tmp.join("base.db");
    let lib_s = lib.to_string_lossy().into_owned();
    run(&base, true, &["--import", &lib_s], &tmp.join("import.png"))?;

    // Library sorted by name: aurora 0-3, bokeh 4-7, clips 8-10, dunes 11-14,
    // mesh 15-18, ribbons 19-22, rings 23-24, sunset 25-28.
    const FIRST: &str = r#"view.select.move={"dx":1,"dy":0}"#; // selects item 0
    const NEXT2: &str = r#"view.select.move={"dx":2,"dy":0}"#;
    struct Shot {
        file: &'static str,
        args: Vec<&'static str>,
        /// `false`: empty library, never imported.
        library: bool,
        adult: bool,
    }
    let shot = |file, args: &[&'static str]| Shot {
        file,
        args: args.to_vec(),
        library: true,
        adult: true,
    };
    let shots = [
        Shot {
            library: false,
            adult: false,
            ..shot("gate.png", &[])
        },
        Shot {
            library: false,
            ..shot("empty.png", &[])
        },
        shot(
            "library.png",
            &[
                "--cmd",
                FIRST,
                "--cmd",
                NEXT2,
                "--cmd",
                r#"media.rate={"rating":5}"#,
                "--cmd",
                "media.favorite.toggle",
            ],
        ),
        shot("viewer.png", &["--tab", "images", "--open", "23"]),
        // Without the inspector: the player bar needs the width.
        shot(
            "player.png",
            &[
                "--cmd",
                "inspector.toggle",
                "--tab",
                "videos",
                "--open",
                "2",
                "--cmd",
                r#"markers.add={"pos_ms":2500,"label":"glow"}"#,
                "--cmd",
                r#"markers.add={"pos_ms":7600,"label":"curtain"}"#,
                "--cmd",
                r#"player.loop={"a_ms":2000,"b_ms":8500}"#,
                "--cmd",
                r#"player.speed={"value":1.5}"#,
                "--cmd",
                r#"player.seek={"pos":5}"#,
                "--cmd",
                r#"player.preview={"pos":7.6}"#,
            ],
        ),
        shot(
            "triage.png",
            &["--cmd", "triage.start", "--cmd", "triage.keep"],
        ),
        shot(
            "rating.png",
            &[
                "--cmd",
                FIRST,
                "--cmd",
                NEXT2,
                "--cmd",
                r#"media.rate={"rating":4}"#,
            ],
        ),
        shot(
            "discreet.png",
            &[
                "--cmd",
                FIRST,
                "--cmd",
                NEXT2,
                "--cmd",
                "app.discreet.toggle",
            ],
        ),
    ];
    for s in shots {
        let db = tmp.join(format!("{}.db", s.file));
        if s.library {
            for ext in ["", "-wal", "-shm"] {
                let from = PathBuf::from(format!("{}{ext}", base.display()));
                if from.exists() {
                    std::fs::copy(&from, format!("{}{ext}", db.display()))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        run(&db, s.adult, &s.args, &out.join(s.file))?;
        println!("screenshot: {}", s.file);
    }

    // Settings, Privacy page, on the clear library: the vault switch.
    let privacy = tmp.join("privacy.db");
    for ext in ["", "-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{ext}", base.display()));
        if from.exists() {
            std::fs::copy(&from, format!("{}{ext}", privacy.display()))
                .map_err(|e| e.to_string())?;
        }
    }
    run(
        &privacy,
        true,
        &["--cmd", r#"app.settings={"tab":"privacy"}"#],
        &out.join("vault-settings.png"),
    )?;
    println!("screenshot: vault-settings.png");

    vault_lock_shot(&tmp, &lib, &out)
}

/// The synthetic passphrase of the throw-away screenshot vault. It lives in
/// `target/`, protects nothing, and only ever goes through a pipe.
const SHOT_PASSPHRASE: &str = "screenshot-vault-synthetic-passphrase";

/// Arguments of one app screenshot run in the vault home. Never a secret.
fn vault_shot_args(home: &Path, db: &Path, png: &Path, extra: &[&str]) -> Vec<std::ffi::OsString> {
    let mut v: Vec<std::ffi::OsString> = vec![
        "--home".into(),
        home.into(),
        "--lang".into(),
        "en".into(),
        "--db".into(),
        db.into(),
    ];
    v.extend(extra.iter().map(Into::into));
    v.extend(["--screenshot".into(), png.into(), "--assume-adult".into()]);
    v
}

/// Arguments of `jizzcraft-cli` that encrypt the vault library. The passphrase
/// is read from the piped stdin, so it is not among them.
fn vault_enable_args(home: &Path, db: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "--home".into(),
        home.into(),
        "--db".into(),
        db.into(),
        "--passphrase-stdin".into(),
        "vault".into(),
        "enable".into(),
        "--i-understand-no-recovery".into(),
    ]
}

/// Lock screen of an encrypted library, with a separate home. The vault is
/// enabled through the CLI with the passphrase on a pipe (never printed, never
/// in an argument); the app is then started without it, so it stays locked.
fn vault_lock_shot(tmp: &Path, lib: &Path, out: &Path) -> Result<(), String> {
    use std::io::Write;
    let st = Command::new("cargo")
        .args(["build", "--release", "-p", "jizzcraft-cli"])
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err("cli build failed".into());
    }
    let (home, db) = (tmp.join("vault-home"), tmp.join("vault.db"));
    let imp = Command::new("target/release/jizzcraft-cli")
        .arg("--home")
        .arg(&home)
        .arg("--db")
        .arg(&db)
        .arg("scan")
        .arg(lib)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !imp.success() {
        return Err("vault library import failed".into());
    }
    let mut child = Command::new("target/release/jizzcraft-cli")
        .args(vault_enable_args(&home, &db))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    {
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        writeln!(stdin, "{SHOT_PASSPHRASE}").map_err(|e| e.to_string())?;
    }
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("vault enable failed".into());
    }
    let png = out.join("vault-lock.png");
    let st = Command::new("target/release/jizzcraft")
        .args(vault_shot_args(&home, &db, &png, &[]))
        .status()
        .map_err(|e| e.to_string())?;
    if st.success() && png.exists() {
        println!("screenshot: vault-lock.png");
        Ok(())
    } else {
        Err("screenshot vault-lock.png failed".into())
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let dest = to.join(e.file_name());
        if e.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_tree(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), dest).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// The one staging tree every package is made from. Layout follows the FHS:
/// `DESTDIR/PREFIX/{bin,share/...}`.
fn dist(args: Vec<String>) -> Result<(), String> {
    let (mut network, mut no_build, mut tarball, mut browser) = (false, false, false, false);
    let (mut prefix, mut destdir) = ("/usr".to_owned(), PathBuf::from("target/stage"));
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            // Deprecated alias of `--edition online`.
            "--network" => {
                eprintln!("warning: --network is deprecated, use --edition online");
                network = true;
            }
            "--edition" => {
                network = match it.next().ok_or("--edition needs a value")?.as_str() {
                    "offline" => false,
                    "online" => true,
                    other => return Err(format!("unknown edition {other} (offline|online)")),
                };
            }
            "--browser" => browser = true,
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
            c.args(["--features", "jizzcraft/online,jizzcraft-cli/network"]);
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
    if browser && !network {
        return Err("--browser needs --edition online".into());
    }
    const BROWSER_BIN: &str = "target/release/jizzcraft-browser";
    let have_browser = browser && Path::new(BROWSER_BIN).exists();
    if browser && !have_browser {
        println!("dist: {BROWSER_BIN} not built, skipping the browser package");
    }
    if have_browser {
        put(BROWSER_BIN, "libexec/jizzcraft/jizzcraft-browser", 0o755)?;
    }
    put("target/release/jizzcraft", "bin/jizzcraft", 0o755)?;
    put("target/release/jizzcraft-cli", "bin/jizzcraft-cli", 0o755)?;
    for d in ["io.github.kerstz.JizzCraft.desktop", "local.Viewer.desktop"] {
        put(
            &format!("assets/{d}"),
            &format!("share/applications/{d}"),
            0o644,
        )?;
    }
    if network {
        put(
            "assets/io.github.kerstz.JizzCraft-magnet.desktop",
            "share/applications/io.github.kerstz.JizzCraft-magnet.desktop",
            0o644,
        )?;
    }
    put(
        "assets/io.github.kerstz.JizzCraft.metainfo.xml",
        "share/metainfo/io.github.kerstz.JizzCraft.metainfo.xml",
        0o644,
    )?;
    for size in [
        "16x16", "24x24", "32x32", "48x48", "64x64", "128x128", "256x256", "512x512",
    ] {
        for n in ["io.github.kerstz.JizzCraft", "local.Viewer"] {
            put(
                &format!("assets/icons/hicolor/{size}/apps/{n}.png"),
                &format!("share/icons/hicolor/{size}/apps/{n}.png"),
                0o644,
            )?;
        }
    }
    for n in ["io.github.kerstz.JizzCraft", "local.Viewer"] {
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
    put(
        "assets/browser-packages.tsv",
        "share/jizzcraft/browser-packages.tsv",
        0o644,
    )?;
    put("README.md", "share/doc/jizzcraft/README.md", 0o644)?;
    for l in ["NOTICE", "LICENSE-MIT", "LICENSE-APACHE"] {
        put(l, &format!("share/licenses/jizzcraft/{l}"), 0o644)?;
    }
    third_party(
        &root.join("share/licenses/jizzcraft/THIRD-PARTY.html"),
        network,
    )?;
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
            if network { "-online" } else { "" }
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
        // The browser binary ships in its own tarball, never in the main one.
        let _ = std::fs::remove_dir_all(top.join("libexec"));
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
        if have_browser {
            let bname = format!("jizzcraft-browser-{ver}-{arch}-linux");
            let btop = PathBuf::from("target/tarball").join(&bname);
            let _ = std::fs::remove_dir_all(&btop);
            let dst = btop.join("libexec/jizzcraft");
            std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
            std::fs::copy(BROWSER_BIN, dst.join("jizzcraft-browser")).map_err(|e| e.to_string())?;
            let st = Command::new("tar")
                .args(["-C", "target/tarball", "-cJf"])
                .arg(format!("dist/{bname}.tar.xz"))
                .arg(&bname)
                .status()
                .map_err(|e| e.to_string())?;
            if !st.success() {
                return Err("tar failed".into());
            }
            println!("dist: dist/{bname}.tar.xz");
        }
    }
    Ok(())
}

/// Third-party licence texts of the Rust dependencies, via cargo-about.
/// Missing tool: warn locally, fail in CI (a release must carry the texts).
fn third_party(out: &Path, network: bool) -> Result<(), String> {
    let ci = std::env::var("CI").is_ok_and(|v| v == "true");
    let have = Command::new("cargo")
        .args(["about", "--version"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !have {
        if ci {
            return Err(
                "cargo-about is required in CI: cargo install cargo-about --locked --features cli"
                    .into(),
            );
        }
        eprintln!(
            "warning: cargo-about not installed, THIRD-PARTY.html not generated (install: cargo install cargo-about --locked --features cli)"
        );
        return Ok(());
    }
    std::fs::create_dir_all(out.parent().ok_or("no parent")?).map_err(|e| e.to_string())?;
    let mut c = Command::new("cargo");
    c.args(["about", "generate", "--locked", "-c", "about.toml"]);
    if network {
        c.arg("--all-features");
    }
    c.arg("about.hbs").arg("-o").arg(out);
    if !c.status().map_err(|e| e.to_string())?.success() {
        return Err("cargo about generate failed".into());
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
        "share/licenses/jizzcraft/*",
    ] {
        if !deb.contains(needle) {
            missing.push(format!("deb metadata lacks {needle}"));
        }
    }
    for f in [
        "packaging/arch/jizzcraft/PKGBUILD",
        "packaging/arch/jizzcraft-online/PKGBUILD",
        "packaging/fedora/jizzcraft.spec",
        "packaging/fedora/jizzcraft-online.spec",
    ] {
        if !read(f)?.contains("xtask") {
            missing.push(format!("{f} does not stage with `cargo xtask dist`"));
        }
    }
    for a in [
        "about.toml",
        "about.hbs",
        "NOTICE",
        "assets/gst-packages.tsv",
        "assets/browser-packages.tsv",
        "assets/jizzcraft.1",
        "assets/icons/hicolor/scalable/apps/local.Viewer.svg",
    ] {
        if !Path::new(a).exists() {
            missing.push(format!("missing {a}"));
        }
    }
    for needle in [
        "[package.metadata.deb.variants.online]",
        "[package.metadata.deb.variants.browser]",
        "jizzcraft-online",
        "jizzcraft-browser",
    ] {
        if !deb.contains(needle) {
            missing.push(format!("deb metadata lacks {needle}"));
        }
    }
    if missing.is_empty() {
        println!("check-packaging: ok");
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_shot_args_never_carry_the_passphrase() {
        let (h, d, p) = (Path::new("h"), Path::new("v.db"), Path::new("o.png"));
        let all = vault_shot_args(h, d, p, &["--cmd", "x"])
            .into_iter()
            .chain(vault_enable_args(h, d));
        for a in all {
            let a = a.to_string_lossy().into_owned();
            assert!(!a.contains(SHOT_PASSPHRASE), "{a}");
            assert!(!SHOT_PASSPHRASE.contains(a.as_str()) || a.len() < 8, "{a}");
        }
    }
}
