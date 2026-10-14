//! Synthetic clips, made with `gst-launch-1.0` from test sources and cached
//! for the process.

use std::any::Any;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

static CACHE: Mutex<Option<HashMap<String, Arc<dyn Any + Send + Sync>>>> = Mutex::new(None);

fn cached<T: Any + Send + Sync>(key: &str, make: impl FnOnce() -> Option<T>) -> Option<Arc<T>> {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(v) = map.get(key) {
        return v.clone().downcast::<T>().ok();
    }
    let v = Arc::new(make()?);
    map.insert(key.to_owned(), v.clone());
    Some(v)
}

fn gst(args: &[String], out: &Path) -> Option<Vec<u8>> {
    let st = Command::new("gst-launch-1.0")
        .arg("-q")
        .args(args)
        .arg("!")
        .arg("filesink")
        .arg(format!("location={}", out.display()))
        .status()
        .ok()?;
    if !st.success() {
        return None;
    }
    std::fs::read(out).ok()
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

fn raw(frames: u32, w: u32, h: u32, pattern: &str) -> Vec<String> {
    let mut a = s(&["videotestsrc"]);
    a.push(format!("num-buffers={frames}"));
    a.push(format!("pattern={pattern}"));
    a.extend(s(&["!"]));
    a.push(format!("video/x-raw,width={w},height={h},framerate=30/1"));
    a.extend(s(&["!", "videoconvert", "!"]));
    a
}

/// A VP8 + Opus WebM at 30 fps. `bitrate` is the encoder target in bit/s.
/// `snow` does not compress: the file fills the budget (about 2 MB for 6 s at
/// 2 Mbit/s), which is several 256 KiB request units.
pub fn webm(frames: u32, w: u32, h: u32, pattern: &str, bitrate: u32) -> Option<Arc<Vec<u8>>> {
    cached(
        &format!("webm-{frames}-{w}-{h}-{pattern}-{bitrate}"),
        || {
            let tmp = tempfile::tempdir().ok()?;
            let mut a = s(&["webmmux", "name=mux", "!", "filesink"]);
            a.push(format!("location={}", tmp.path().join("c.webm").display()));
            // Video and audio branches both end in the muxer; a single
            // gst-launch description keeps it simple.
            let mut v = raw(frames, w, h, pattern);
            v.extend(s(&["vp8enc", "deadline=1", "keyframe-max-dist=15"]));
            v.push(format!("target-bitrate={bitrate}"));
            v.extend(s(&["!", "mux."]));
            let secs = frames as f64 / 30.0;
            let mut au = s(&["audiotestsrc", "wave=sine"]);
            au.push(format!("num-buffers={}", (secs * 43.0).ceil() as u32));
            au.extend(s(&[
                "!",
                "audioconvert",
                "!",
                "audioresample",
                "!",
                "opusenc",
                "!",
                "mux.",
            ]));
            let mut full = a;
            full.extend(v);
            full.extend(au);
            let st = Command::new("gst-launch-1.0")
                .arg("-q")
                .args(&full)
                .status()
                .ok()?;
            if !st.success() {
                return None;
            }
            std::fs::read(tmp.path().join("c.webm")).ok()
        },
    )
}

/// One H.264 MPEG-TS per quality, cut before every keyframe packet (with the
/// PAT/PMT that precede it) so that each piece decodes on its own, like an HLS
/// segment. Keyframes every second: `seconds` segments of 1 s.
pub fn ts_segments(w: u32, h: u32, seconds: u32) -> Option<Arc<Vec<Vec<u8>>>> {
    cached(&format!("ts-{w}-{h}-{seconds}"), || {
        let tmp = tempfile::tempdir().ok()?;
        let mut a = raw(seconds * 30, w, h, "ball");
        a.extend(s(&[
            "x264enc",
            "key-int-max=30",
            "bframes=0",
            "speed-preset=ultrafast",
            "!",
            "h264parse",
            "!",
            "mpegtsmux",
        ]));
        let data = gst(&a, &tmp.path().join("c.ts"))?;
        let segs = split_ts(&data);
        (segs.len() + 1 >= seconds as usize).then_some(segs)
    })
}

/// The pieces of a fragmented MP4.
pub struct Fmp4 {
    pub init: Vec<u8>,
    pub fragments: Vec<Vec<u8>>,
}

/// Fragmented MP4: the init segment (`ftyp` + `moov`) and one `moof` + `mdat`
/// fragment per keyframe interval (1 s).
pub fn fmp4(w: u32, h: u32, seconds: u32) -> Option<Arc<Fmp4>> {
    cached(&format!("fmp4-{w}-{h}-{seconds}"), || {
        let tmp = tempfile::tempdir().ok()?;
        let mut a = raw(seconds * 30, w, h, "ball");
        a.extend(s(&[
            "x264enc",
            "key-int-max=30",
            "bframes=0",
            "speed-preset=ultrafast",
            "!",
            "h264parse",
            "!",
            "mp4mux",
            "fragment-duration=1000",
        ]));
        let data = gst(&a, &tmp.path().join("c.mp4"))?;
        split_fmp4(&data, seconds)
    })
}

/// The audio-only counterpart of [`fmp4`]: AAC in fragmented MP4, one
/// fragment per second. Played together with a video track it exercises the
/// separate audio path.
pub fn fmp4_audio(seconds: u32) -> Option<Arc<Fmp4>> {
    cached(&format!("fmp4-audio-{seconds}"), || {
        let tmp = tempfile::tempdir().ok()?;
        let mut a = s(&["audiotestsrc", "wave=sine"]);
        a.push(format!("num-buffers={}", seconds * 44));
        a.push("samplesperbuffer=1024".to_owned());
        a.extend(s(&[
            "!",
            "audio/x-raw,rate=44100,channels=2",
            "!",
            "audioconvert",
            "!",
            "avenc_aac",
            "!",
            "aacparse",
            "!",
            "mp4mux",
            "fragment-duration=1000",
        ]));
        let data = gst(&a, &tmp.path().join("a.mp4"))?;
        split_fmp4(&data, seconds.saturating_sub(1))
    })
}

fn split_fmp4(data: &[u8], seconds: u32) -> Option<Fmp4> {
    let (mut init, mut frags) = (Vec::new(), Vec::new());
    let mut off = 0usize;
    let mut cur: Vec<u8> = Vec::new();
    while off + 8 <= data.len() {
        let size = u32::from_be_bytes(data[off..off + 4].try_into().ok()?) as usize;
        let kind = &data[off + 4..off + 8];
        if size < 8 || off + size > data.len() {
            return None;
        }
        let atom = &data[off..off + size];
        match kind {
            b"ftyp" | b"moov" => init.extend_from_slice(atom),
            b"moof" => {
                if !cur.is_empty() {
                    frags.push(std::mem::take(&mut cur));
                }
                cur.extend_from_slice(atom);
            }
            b"mdat" => cur.extend_from_slice(atom),
            _ => {}
        }
        off += size;
    }
    if !cur.is_empty() {
        frags.push(cur);
    }
    (!init.is_empty() && frags.len() + 1 >= seconds as usize).then_some(Fmp4 {
        init,
        fragments: frags,
    })
}

/// Cut a TS stream before every keyframe packet (with its PAT/PMT).
fn split_ts(data: &[u8]) -> Vec<Vec<u8>> {
    let pkts: Vec<&[u8]> = data.as_chunks::<188>().0.iter().map(|c| &c[..]).collect();
    let pid = |p: &[u8]| (((p[1] & 0x1f) as u16) << 8) | p[2] as u16;
    let rai = |p: &[u8]| p[3] & 0x20 != 0 && p[4] > 0 && p[5] & 0x40 != 0;
    let mut cuts = Vec::new();
    for (i, p) in pkts.iter().enumerate() {
        if p[0] == 0x47 && pid(p) != 0 && rai(p) {
            let video = pid(p);
            let mut c = i;
            while c > 0 && pid(pkts[c - 1]) != video {
                c -= 1;
            }
            cuts.push(c);
        }
    }
    cuts.dedup();
    if cuts.first() != Some(&0) {
        cuts.insert(0, 0);
    }
    cuts.push(pkts.len());
    cuts.windows(2)
        .map(|w| pkts[w[0]..w[1]].concat())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A media playlist for `n` segments of `dur` seconds named `{prefix}{i}.{ext}`.
pub fn playlist(n: usize, dur: f32, prefix: &str, ext: &str, extra: &str) -> String {
    let mut p = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n{extra}",
        dur.ceil() as u32
    );
    for i in 0..n {
        p.push_str(&format!("#EXTINF:{dur:.3},\n{prefix}{i}.{ext}\n"));
    }
    p.push_str("#EXT-X-ENDLIST\n");
    p
}
