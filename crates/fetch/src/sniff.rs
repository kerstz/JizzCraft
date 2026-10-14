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
}
