//! What does the first block of a response look like? Pure.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniff {
    Hls,
    Dash,
    /// A container this app can read (MP4/MOV, Matroska/WebM, TS, AVI/WAV,
    /// Ogg, FLV).
    Media,
    /// Anything else: HTML, JSON, noise.
    Other,
}

pub fn sniff(head: &[u8]) -> Sniff {
    let text = String::from_utf8_lossy(&head[..head.len().min(4096)]);
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("#EXTM3U") {
        return Sniff::Hls;
    }
    if t.starts_with('<') && t.contains("<MPD") {
        return Sniff::Dash;
    }
    let at = |i: usize, m: &[u8]| head.len() >= i + m.len() && &head[i..i + m.len()] == m;
    if at(4, b"ftyp")
        || at(0, &[0x1A, 0x45, 0xDF, 0xA3])
        || (at(0, &[0x47]) && head.len() > 188 && head[188] == 0x47)
        || at(0, b"RIFF")
        || at(0, b"OggS")
        || at(0, b"FLV")
    {
        return Sniff::Media;
    }
    Sniff::Other
}

/// True when the first bytes of an MP4/MOV show `mdat` before any `moov`: the
/// index is at the end, so the file cannot play before it is complete unless
/// the server honours `Range`. Walks top-level atoms only; unknown = false.
pub fn mp4_index_at_end(head: &[u8]) -> bool {
    let mut off: u64 = 0;
    let mut first = true;
    while off + 8 <= head.len() as u64 {
        let o = off as usize;
        let size = u32::from_be_bytes([head[o], head[o + 1], head[o + 2], head[o + 3]]) as u64;
        let kind = &head[o + 4..o + 8];
        if first && kind != b"ftyp" {
            return false;
        }
        first = false;
        match kind {
            b"moov" => return false,
            b"mdat" => return true,
            _ => {}
        }
        let step = match size {
            0 => return false,
            1 => {
                if o + 16 > head.len() {
                    return false;
                }
                u64::from_be_bytes(head[o + 8..o + 16].try_into().unwrap_or([0; 8]))
            }
            n => n,
        };
        if step < 8 {
            return false;
        }
        off += step;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads() {
        assert_eq!(sniff(b"#EXTM3U\n#EXTINF"), Sniff::Hls);
        assert_eq!(sniff(b"\xEF\xBB\xBF \n#EXTM3U"), Sniff::Hls);
        assert_eq!(
            sniff(b"<?xml version=\"1.0\"?>\n<MPD a=\"b\">"),
            Sniff::Dash
        );
        assert_eq!(sniff(b"<html><body>hi</body></html>"), Sniff::Other);
        assert_eq!(sniff(b"\0\0\0\x18ftypisom"), Sniff::Media);
        assert_eq!(sniff(&[0x1A, 0x45, 0xDF, 0xA3, 1]), Sniff::Media);
        let mut ts = vec![0u8; 400];
        ts[0] = 0x47;
        assert_eq!(sniff(&ts), Sniff::Other);
        ts[188] = 0x47;
        assert_eq!(sniff(&ts), Sniff::Media);
        assert_eq!(sniff(b"RIFF\0\0\0\0AVI "), Sniff::Media);
        assert_eq!(sniff(b"OggS\0"), Sniff::Media);
        assert_eq!(sniff(b"FLV\x01"), Sniff::Media);
        assert_eq!(sniff(b""), Sniff::Other);
    }

    fn atom(kind: &[u8; 4], body: usize) -> Vec<u8> {
        let mut v = ((body + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.resize(body + 8, 0);
        v
    }

    #[test]
    fn index_position_is_found_by_walking_atoms() {
        let ftyp = atom(b"ftyp", 16);
        let mut tail_index = ftyp.clone();
        tail_index.extend(atom(b"free", 10));
        tail_index.extend(atom(b"mdat", 100));
        assert!(mp4_index_at_end(&tail_index));
        let mut fast = ftyp.clone();
        fast.extend(atom(b"moov", 40));
        fast.extend(atom(b"mdat", 100));
        assert!(!mp4_index_at_end(&fast));
        assert!(!mp4_index_at_end(&ftyp));
        assert!(!mp4_index_at_end(b"<html>not mp4 at all</html>"));
        assert!(!mp4_index_at_end(b""));
    }
}
