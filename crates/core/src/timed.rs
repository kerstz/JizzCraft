//! A source of timed media chunks (segmented streams). Std only: the engine
//! knows the shape, `fetch` provides the bytes, the player consumes them.
//!
//! Contract: `pull` returns `Ok(None)` at the end of the stream and
//! `Err(WouldBlock)` when nothing is ready yet (ask again); any other error
//! is final.

use std::io;
use std::time::Duration;

/// One piece of container data, in presentation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub bytes: Vec<u8>,
    /// Presentation time of the first sample, when known (segment start).
    pub pts: Option<Duration>,
    /// Timeline break: after a seek, or where the source says so.
    pub discont: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantInfo {
    pub height: Option<u32>,
    pub bandwidth: u64,
}

/// One selectable audio track of a stream that keeps its audio apart.
/// Never a file name or an address: a language tag, or nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioTrackInfo {
    /// Language tag as the stream declares it (`fr`, `pt-BR`), cleaned.
    pub lang: Option<String>,
    /// The stream marks this one as its default.
    pub default: bool,
}

/// Keep a declared language tag only when it looks like one (ASCII letters,
/// digits and `-`, at most 12 characters). Anything else is dropped so that
/// a manifest can never smuggle a name or an address into a menu.
pub fn clean_lang(raw: &str) -> Option<String> {
    let s = raw.trim().replace('_', "-");
    let ok = !s.is_empty()
        && s.len() <= 12
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    ok.then_some(s)
}

pub trait TimedSource: Send {
    /// Total length; `None` for a live stream.
    fn duration(&self) -> Option<Duration>;
    /// Move to the segment containing `t`; returns the time actually reached
    /// (the segment start).
    fn seek(&mut self, t: Duration) -> Duration;
    fn pull(&mut self) -> io::Result<Option<Chunk>>;
    /// End of the data already downloaded, if known.
    fn buffered_until(&self) -> Option<Duration>;
    fn variants(&self) -> Vec<VariantInfo>;
    fn current_variant(&self) -> Option<usize>;
    /// `None` is automatic selection.
    fn select_variant(&mut self, v: Option<usize>);
    /// True once when the connection cannot keep up and a lower variant
    /// should be chosen (only in automatic mode).
    fn wants_downgrade(&mut self) -> bool;
    /// The audio tracks to choose from; empty when the stream has one audio
    /// (or none) and nothing to pick.
    fn audio_tracks(&self) -> Vec<AudioTrackInfo> {
        Vec::new()
    }
    /// Index in [`TimedSource::audio_tracks`] of the track being played.
    fn current_audio(&self) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<Chunk>);
    impl TimedSource for Fixed {
        fn duration(&self) -> Option<Duration> {
            None
        }
        fn seek(&mut self, t: Duration) -> Duration {
            t
        }
        fn pull(&mut self) -> io::Result<Option<Chunk>> {
            Ok(if self.0.is_empty() {
                None
            } else {
                Some(self.0.remove(0))
            })
        }
        fn buffered_until(&self) -> Option<Duration> {
            None
        }
        fn variants(&self) -> Vec<VariantInfo> {
            Vec::new()
        }
        fn current_variant(&self) -> Option<usize> {
            None
        }
        fn select_variant(&mut self, _: Option<usize>) {}
        fn wants_downgrade(&mut self) -> bool {
            false
        }
    }

    #[test]
    fn language_tags_are_cleaned() {
        assert_eq!(clean_lang(" pt_BR "), Some("pt-BR".to_owned()));
        assert_eq!(clean_lang("fr"), Some("fr".to_owned()));
        assert_eq!(clean_lang(""), None);
        assert_eq!(clean_lang("movie.mp4"), None);
        assert_eq!(clean_lang("http://x.invalid/a"), None);
        assert_eq!(clean_lang("averyveryverylongtag"), None);
        assert_eq!(clean_lang("1-2"), None);
    }

    #[test]
    fn trait_is_object_safe_and_send() {
        let mut s: Box<dyn TimedSource> = Box::new(Fixed(vec![Chunk {
            bytes: vec![1],
            pts: Some(Duration::ZERO),
            discont: true,
        }]));
        assert!(s.pull().unwrap().is_some());
        assert!(s.pull().unwrap().is_none());
        assert_eq!(s.duration(), None);
    }
}
