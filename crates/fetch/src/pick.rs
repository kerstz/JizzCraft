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

#[cfg(test)]
mod tests {
    use super::*;

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
        // A sample is still better than nothing.
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
