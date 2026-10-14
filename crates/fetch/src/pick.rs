//! Which file of a torrent to play. Pure: no I/O, no network.

const VIDEO_EXT: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv", "flv", "ts", "m2ts", "mpg", "mpeg", "ogv",
];

#[derive(Debug, PartialEq, Eq)]
pub enum Pick {
    /// The obvious file.
    One(usize),
    /// Several comparable videos (a pack): the user chooses.
    Ask(Vec<usize>),
    /// No video file in this torrent.
    None,
}

pub fn is_video(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| VIDEO_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

fn is_sample(name: &str) -> bool {
    name.to_ascii_lowercase().contains("sample")
}

/// `files` = (relative name, length). Samples and tiny leftovers are ignored;
/// when two or more videos are at least half the size of the largest, the
/// answer is `Ask` (season packs), otherwise the largest wins.
pub fn pick_video(files: &[(String, u64)]) -> Pick {
    let videos: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, (n, _))| is_video(n))
        .map(|(i, _)| i)
        .collect();
    if videos.is_empty() {
        return Pick::None;
    }
    let real: Vec<usize> = videos
        .iter()
        .copied()
        .filter(|&i| !is_sample(&files[i].0))
        .collect();
    let pool = if real.is_empty() { videos } else { real };
    let largest = pool.iter().map(|&i| files[i].1).max().unwrap_or(0);
    let mut big: Vec<usize> = pool
        .into_iter()
        .filter(|&i| files[i].1 >= largest / 2)
        .collect();
    if big.len() >= 2 {
        big.sort_by(|&a, &b| files[a].0.cmp(&files[b].0));
        return Pick::Ask(big);
    }
    Pick::One(big[0])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Prefer {
    #[default]
    Best,
    Smallest,
}

/// What the user wants from an adaptive stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Quality {
    pub max_height: Option<u32>,
    pub prefer: Prefer,
}

/// Choose a variant. `variants` = (height, bandwidth in bits/s). Variants
/// taller than `max_height` are dropped (the smallest survives if none is
/// left); with a throughput estimate, variants needing more than 80 % of it
/// are dropped too (same fallback). Then `Best` takes the highest bandwidth
/// and `Smallest` the lowest. Returns an index; 0 for an empty slice.
pub fn pick_variant(variants: &[(Option<u32>, u64)], q: Quality, est_bps: Option<u64>) -> usize {
    if variants.is_empty() {
        return 0;
    }
    let mut keep: Vec<usize> = (0..variants.len())
        .filter(|&i| match (q.max_height, variants[i].0) {
            (Some(m), Some(h)) => h <= m,
            _ => true,
        })
        .collect();
    if keep.is_empty() {
        let smallest = (0..variants.len())
            .min_by_key(|&i| (variants[i].0.unwrap_or(0), variants[i].1, i))
            .unwrap_or(0);
        keep.push(smallest);
    }
    if let Some(est) = est_bps {
        let budget = est / 10 * 8;
        let fit: Vec<usize> = keep
            .iter()
            .copied()
            .filter(|&i| variants[i].1 <= budget)
            .collect();
        if fit.is_empty() {
            keep = vec![
                keep.iter()
                    .copied()
                    .min_by_key(|&i| (variants[i].1, i))
                    .unwrap_or(0),
            ];
        } else {
            keep = fit;
        }
    }
    match q.prefer {
        Prefer::Best => keep.into_iter().max_by_key(|&i| {
            (
                variants[i].1,
                variants[i].0.unwrap_or(0),
                std::cmp::Reverse(i),
            )
        }),
        Prefer::Smallest => keep
            .into_iter()
            .min_by_key(|&i| (variants[i].1, variants[i].0.unwrap_or(u32::MAX), i)),
    }
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_choice_table() {
        let v = [
            (Some(360), 800_000),
            (Some(1080), 5_000_000),
            (Some(720), 2_500_000),
            (None, 1_000_000),
        ];
        let q = |m: Option<u32>, p| Quality {
            max_height: m,
            prefer: p,
        };
        let cases: [(Quality, Option<u64>, usize); 8] = [
            (q(None, Prefer::Best), None, 1),
            (q(None, Prefer::Smallest), None, 0),
            (q(Some(720), Prefer::Best), None, 2),
            (q(Some(720), Prefer::Smallest), None, 0),
            // An unknown height is never excluded by the cap.
            (q(Some(144), Prefer::Best), None, 3),
            // 80 % of 4 Mbit/s is 3.2: the 1080p one is dropped.
            (q(None, Prefer::Best), Some(4_000_000), 2),
            // Nothing fits the estimate: the lowest bandwidth.
            (q(None, Prefer::Best), Some(100_000), 0),
            (q(Some(720), Prefer::Best), Some(2_000_000), 3),
        ];
        for (i, (qq, est, want)) in cases.iter().enumerate() {
            assert_eq!(pick_variant(&v, *qq, *est), *want, "case {i}");
        }
        // Nothing fits the cap: the smallest survives.
        let known = [
            (Some(1080), 5_000_000),
            (Some(480), 900_000),
            (Some(720), 2_000_000),
        ];
        assert_eq!(pick_variant(&known, q(Some(144), Prefer::Best), None), 1);
        assert_eq!(pick_variant(&[], Quality::default(), None), 0);
        assert_eq!(Quality::default().prefer, Prefer::Best);
        let j: Quality = serde_json::from_str(r#"{"max_height":480}"#).unwrap();
        assert_eq!(j.max_height, Some(480));
    }

    fn f(v: &[(&str, u64)]) -> Vec<(String, u64)> {
        v.iter().map(|(n, s)| ((*n).to_owned(), *s)).collect()
    }

    #[test]
    fn single_movie_with_extras() {
        let p = pick_video(&f(&[("a.nfo", 10), ("a.mkv", 900), ("a.srt", 5)]));
        assert_eq!(p, Pick::One(1));
    }

    #[test]
    fn sample_is_ignored() {
        let p = pick_video(&f(&[("Sample/sample.mkv", 50), ("movie.mp4", 800)]));
        assert_eq!(p, Pick::One(1));
        // A sample is still better than nothing. Ask any supermarket.
        assert_eq!(pick_video(&f(&[("sample.mp4", 50)])), Pick::One(0));
    }

    #[test]
    fn pack_asks() {
        let p = pick_video(&f(&[
            ("s01e02.mkv", 400),
            ("s01e01.mkv", 410),
            ("tiny.mkv", 5),
        ]));
        assert_eq!(p, Pick::Ask(vec![1, 0]));
    }

    #[test]
    fn no_video() {
        assert_eq!(pick_video(&f(&[("a.txt", 1), ("b.zip", 9)])), Pick::None);
        assert_eq!(pick_video(&[]), Pick::None);
    }

    #[test]
    fn extension_is_case_insensitive() {
        assert!(is_video("CLIP.MP4"));
        assert!(!is_video("clip.mp4.part"));
        assert!(!is_video("noext"));
    }
}
