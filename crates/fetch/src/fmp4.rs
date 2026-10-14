//! Just enough ISO BMFF to put a video and an audio fragmented-MP4 track in
//! one byte stream. Pure: bytes in, bytes out, no I/O.
//!
//! The player takes one stream of container bytes. A DASH or HLS stream that
//! keeps picture and sound apart is therefore combined here: the two init
//! segments become one `moov` with two tracks (ids 1 and 2), and every media
//! fragment gets its `tfhd` track id rewritten to match. Sample data is never
//! touched, so nothing is decoded and nothing is decrypted.
//!
//! Anything that is not plain fragmented MP4 is refused with the fixed reason
//! `separate tracks`; protected tracks (`pssh`, `sinf`, `encv`, `enca`) with
//! `protected`.

/// Track id of the video track in the combined stream.
pub const VIDEO_ID: u32 = 1;
/// Track id of the audio track in the combined stream.
pub const AUDIO_ID: u32 = 2;

const SEPARATE: &str = "separate tracks";

/// One box: type, header length, and the byte range of the whole box.
#[derive(Clone, Copy)]
struct BoxRef {
    kind: [u8; 4],
    start: usize,
    hdr: usize,
    end: usize,
}

impl BoxRef {
    fn payload<'a>(&self, b: &'a [u8]) -> &'a [u8] {
        &b[self.start + self.hdr..self.end]
    }
}

fn boxes(b: &[u8]) -> Result<Vec<BoxRef>, &'static str> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < b.len() {
        if b.len() - p < 8 {
            return Err(SEPARATE);
        }
        let size32 = u32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]) as u64;
        let kind = [b[p + 4], b[p + 5], b[p + 6], b[p + 7]];
        let (hdr, size) = match size32 {
            0 => (8usize, (b.len() - p) as u64),
            1 => {
                if b.len() - p < 16 {
                    return Err(SEPARATE);
                }
                let mut s = [0u8; 8];
                s.copy_from_slice(&b[p + 8..p + 16]);
                (16usize, u64::from_be_bytes(s))
            }
            n => (8usize, n),
        };
        if size < hdr as u64 || size > (b.len() - p) as u64 {
            return Err(SEPARATE);
        }
        let end = p + size as usize;
        out.push(BoxRef {
            kind,
            start: p,
            hdr,
            end,
        });
        p = end;
    }
    Ok(out)
}

fn find(b: &[u8], kind: &[u8; 4]) -> Result<Option<BoxRef>, &'static str> {
    Ok(boxes(b)?.into_iter().find(|x| &x.kind == kind))
}

fn put_u32(b: &mut [u8], at: usize, v: u32) -> Result<(), &'static str> {
    b.get_mut(at..at + 4)
        .ok_or(SEPARATE)?
        .copy_from_slice(&v.to_be_bytes());
    Ok(())
}

fn get_u32(b: &[u8], at: usize) -> Result<u32, &'static str> {
    let s = b.get(at..at + 4).ok_or(SEPARATE)?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn wrap(kind: &[u8; 4], parts: &[&[u8]]) -> Result<Vec<u8>, &'static str> {
    let n: usize = parts.iter().map(|p| p.len()).sum::<usize>() + 8;
    let n = u32::try_from(n).map_err(|_| SEPARATE)?;
    let mut out = Vec::with_capacity(n as usize);
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(kind);
    for p in parts {
        out.extend_from_slice(p);
    }
    Ok(out)
}

/// What one init segment holds, after validation.
struct Init {
    ftyp: Option<Vec<u8>>,
    mvhd: Vec<u8>,
    trak: Vec<u8>,
    trex: Vec<u8>,
}

fn is_protected_format(k: &[u8; 4]) -> bool {
    matches!(k, b"encv" | b"enca" | b"encs" | b"enct" | b"sinf")
}

/// `stsd` entries whose format says the samples are encrypted.
fn trak_protected(trak: &[u8]) -> Result<bool, &'static str> {
    let mut cur = trak.to_vec();
    for name in [b"mdia", b"minf", b"stbl"] {
        let Some(bx) = find(&cur, name)? else {
            return Ok(false);
        };
        cur = bx.payload(&cur).to_vec();
    }
    let Some(stsd) = find(&cur, b"stsd")? else {
        return Ok(false);
    };
    let p = stsd.payload(&cur);
    // Full box header (4) and entry count (4), then the sample entries.
    let entries = p.get(8..).ok_or(SEPARATE)?;
    Ok(boxes(entries)?.iter().any(|e| is_protected_format(&e.kind)))
}

fn tkhd_id(tkhd_payload: &[u8]) -> Result<u32, &'static str> {
    let v = *tkhd_payload.first().ok_or(SEPARATE)?;
    get_u32(tkhd_payload, if v == 1 { 20 } else { 12 })
}

fn parse_init(b: &[u8]) -> Result<Init, &'static str> {
    let top = boxes(b)?;
    let ftyp = top
        .iter()
        .find(|x| &x.kind == b"ftyp")
        .map(|x| b[x.start..x.end].to_vec());
    let moov = top
        .iter()
        .find(|x| &x.kind == b"moov")
        .ok_or(SEPARATE)?
        .payload(b);
    let kids = boxes(moov)?;
    let mut mvhd = None;
    let mut traks = Vec::new();
    let mut trexes = Vec::new();
    for k in &kids {
        match &k.kind {
            b"mvhd" => mvhd = Some(moov[k.start..k.end].to_vec()),
            b"pssh" => return Err("protected"),
            b"trak" => traks.push(moov[k.start..k.end].to_vec()),
            b"mvex" => {
                let inner = k.payload(moov);
                for t in boxes(inner)? {
                    if &t.kind == b"trex" {
                        trexes.push(inner[t.start..t.end].to_vec());
                    }
                }
            }
            _ => {}
        }
    }
    // One track per init, and it must be a fragmented one.
    if traks.len() != 1 {
        return Err(SEPARATE);
    }
    let trak = traks.remove(0);
    let tpay = &trak[8..];
    if trak_protected(tpay)? {
        return Err("protected");
    }
    let tkhd = find(tpay, b"tkhd")?.ok_or(SEPARATE)?;
    let id = tkhd_id(tkhd.payload(tpay))?;
    let trex = trexes
        .into_iter()
        .find(|t| get_u32(t, 12).is_ok_and(|v| v == id))
        .ok_or(SEPARATE)?;
    Ok(Init {
        ftyp,
        mvhd: mvhd.ok_or(SEPARATE)?,
        trak,
        trex,
    })
}

/// Set the track id of a `trak` (inside its `tkhd`).
fn retag_trak(trak: &mut [u8], id: u32) -> Result<(), &'static str> {
    let tpay_off = 8;
    let tkhd = find(&trak[tpay_off..], b"tkhd")?.ok_or(SEPARATE)?;
    let base = tpay_off + tkhd.start + tkhd.hdr;
    let v = *trak.get(base).ok_or(SEPARATE)?;
    put_u32(trak, base + if v == 1 { 20 } else { 12 }, id)
}

/// One init segment holding both tracks: video is track 1, audio track 2.
pub fn merge_init(video: &[u8], audio: &[u8]) -> Result<Vec<u8>, &'static str> {
    let v = parse_init(video)?;
    let a = parse_init(audio)?;
    let mut mvhd = v.mvhd;
    // `next_track_ID` is the last field of `mvhd`, in both versions.
    let n = mvhd.len();
    put_u32(&mut mvhd, n - 4, AUDIO_ID + 1)?;
    let (mut vt, mut at) = (v.trak, a.trak);
    retag_trak(&mut vt, VIDEO_ID)?;
    retag_trak(&mut at, AUDIO_ID)?;
    let (mut vx, mut ax) = (v.trex, a.trex);
    put_u32(&mut vx, 12, VIDEO_ID)?;
    put_u32(&mut ax, 12, AUDIO_ID)?;
    let mvex = wrap(b"mvex", &[&vx, &ax])?;
    let moov = wrap(b"moov", &[&mvhd, &vt, &at, &mvex])?;
    let mut out = v.ftyp.unwrap_or_default();
    out.extend_from_slice(&moov);
    Ok(out)
}

/// Rewrite the track id in every `traf/tfhd` (and `sidx`) of a media
/// segment. At least one `moof` must be there: a transport stream or a WebM
/// cluster is not something this can combine.
pub fn retag_fragment(seg: &mut [u8], id: u32) -> Result<(), &'static str> {
    let mut found = 0usize;
    for top in boxes(seg)? {
        match &top.kind {
            b"moof" => {
                let inner_start = top.start + top.hdr;
                let inner = seg[inner_start..top.end].to_vec();
                for traf in boxes(&inner)? {
                    if &traf.kind != b"traf" {
                        continue;
                    }
                    let tp = &inner[traf.start + traf.hdr..traf.end];
                    for tf in boxes(tp)? {
                        if &tf.kind == b"tfhd" {
                            // Full box header (4), then the track id.
                            let at = inner_start + traf.start + traf.hdr + tf.start + tf.hdr + 4;
                            put_u32(seg, at, id)?;
                            found += 1;
                        }
                    }
                }
            }
            b"sidx" => {
                let at = top.start + top.hdr + 4;
                put_u32(seg, at, id)?;
            }
            _ => {}
        }
    }
    if found == 0 {
        return Err(SEPARATE);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Synthetic fragmented MP4 pieces, shared by unit and loopback tests.
    use super::wrap;

    fn full(kind: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![version, 0, 0, 0];
        p.extend_from_slice(body);
        wrap(kind, &[&p]).unwrap()
    }

    pub fn init(track_id: u32, handler: &[u8; 4], stsd_format: &[u8; 4]) -> Vec<u8> {
        let ftyp = wrap(b"ftyp", &[b"isom", &0u32.to_be_bytes(), b"isomiso6"]).unwrap();
        let mut mvhd_body = vec![0u8; 96];
        mvhd_body.extend_from_slice(&(track_id + 1).to_be_bytes());
        let mvhd = full(b"mvhd", 0, &mvhd_body);
        let mut tk = vec![0u8; 8];
        tk.extend_from_slice(&track_id.to_be_bytes());
        tk.extend_from_slice(&[0u8; 68]);
        let tkhd = full(b"tkhd", 0, &tk);
        let entry = wrap(stsd_format, &[&[0u8; 8]]).unwrap();
        let mut stsd_body = 1u32.to_be_bytes().to_vec();
        stsd_body.extend_from_slice(&entry);
        let stsd = full(b"stsd", 0, &stsd_body);
        let stbl = wrap(b"stbl", &[&stsd]).unwrap();
        let minf = wrap(b"minf", &[&stbl]).unwrap();
        let mut hd = vec![0u8; 4];
        hd.extend_from_slice(handler);
        hd.extend_from_slice(&[0u8; 12]);
        let hdlr = full(b"hdlr", 0, &hd);
        let mdia = wrap(b"mdia", &[&hdlr, &minf]).unwrap();
        let trak = wrap(b"trak", &[&tkhd, &mdia]).unwrap();
        let mut tx = track_id.to_be_bytes().to_vec();
        tx.extend_from_slice(&[0u8; 16]);
        let trex = full(b"trex", 0, &tx);
        let mvex = wrap(b"mvex", &[&trex]).unwrap();
        let moov = wrap(b"moov", &[&mvhd, &trak, &mvex]).unwrap();
        let mut out = ftyp;
        out.extend_from_slice(&moov);
        out
    }

    /// `moof` (mfhd + one traf/tfhd) followed by an `mdat` of `payload`.
    pub fn fragment(track_id: u32, seq: u32, payload: &[u8]) -> Vec<u8> {
        let mfhd = full(b"mfhd", 0, &seq.to_be_bytes());
        let tfhd = full(b"tfhd", 0, &track_id.to_be_bytes());
        let traf = wrap(b"traf", &[&tfhd]).unwrap();
        let moof = wrap(b"moof", &[&mfhd, &traf]).unwrap();
        let mdat = wrap(b"mdat", &[payload]).unwrap();
        let mut out = moof;
        out.extend_from_slice(&mdat);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{fragment, init};
    use super::*;

    fn track_ids(init: &[u8]) -> Vec<u32> {
        let moov = find(init, b"moov").unwrap().unwrap();
        let p = moov.payload(init);
        let mut ids = Vec::new();
        for k in boxes(p).unwrap() {
            if &k.kind == b"trak" {
                let tp = k.payload(p);
                let t = find(tp, b"tkhd").unwrap().unwrap();
                ids.push(tkhd_id(t.payload(tp)).unwrap());
            }
        }
        ids
    }

    #[test]
    fn two_inits_become_one_with_two_tracks() {
        let v = init(1, b"vide", b"avc1");
        let a = init(1, b"soun", b"mp4a");
        let m = merge_init(&v, &a).unwrap();
        assert_eq!(track_ids(&m), [VIDEO_ID, AUDIO_ID]);
        // Parses as ftyp + moov, with two trex.
        let top = boxes(&m).unwrap();
        assert_eq!(&top[0].kind, b"ftyp");
        let moov = top[1].payload(&m);
        let mvex = boxes(moov)
            .unwrap()
            .into_iter()
            .find(|k| &k.kind == b"mvex")
            .unwrap();
        let ids: Vec<u32> = boxes(mvex.payload(moov))
            .unwrap()
            .iter()
            .map(|t| get_u32(&mvex.payload(moov)[t.start..t.end], 12).unwrap())
            .collect();
        assert_eq!(ids, [1, 2]);
    }

    #[test]
    fn fragments_are_retagged_without_touching_samples() {
        let mut f = fragment(1, 7, b"sample-bytes");
        let before = f.clone();
        retag_fragment(&mut f, AUDIO_ID).unwrap();
        assert_ne!(f, before);
        assert_eq!(f.len(), before.len());
        assert!(f.ends_with(b"sample-bytes"));
        // Retagging back restores the original.
        retag_fragment(&mut f, 1).unwrap();
        assert_eq!(f, before);
    }

    #[test]
    fn non_fmp4_and_protected_tracks_are_refused() {
        let v = init(1, b"vide", b"avc1");
        assert_eq!(merge_init(&v, &[0x47; 188]), Err("separate tracks"));
        assert_eq!(merge_init(&v, &[]), Err("separate tracks"));
        let enc = init(1, b"soun", b"enca");
        assert_eq!(merge_init(&v, &enc), Err("protected"));
        let mut ts = vec![0x47u8; 188];
        assert_eq!(retag_fragment(&mut ts, 2), Err("separate tracks"));
        // A truncated box is not parsed past its end.
        let mut cut = fragment(1, 1, b"abcdef");
        cut.truncate(cut.len() - 3);
        assert!(retag_fragment(&mut cut, 2).is_err());
    }
}
