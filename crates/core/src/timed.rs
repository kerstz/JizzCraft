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
