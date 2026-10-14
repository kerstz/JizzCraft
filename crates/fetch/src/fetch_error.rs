//! Typed stream failures. Every variant has one fixed sentence (see
//! [`crate::errors`]); no URL, title or path is ever part of it.

use crate::errors;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedKind {
    /// A live stream without an end.
    Live,
    /// Video and audio delivered as separate tracks.
    SeparateTracks,
    /// Any other format the app cannot read.
    Format,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    Unreachable,
    NotFound,
    Forbidden,
    Expired,
    SourceChanged,
    Timeout,
    ServerBusy,
    TooLarge,
    NotMedia,
    Unsupported(UnsupportedKind),
    Drm,
    ProxyRequired,
    ClockSkew,
    DiskFull,
    NoPeers,
    MetadataTimeout,
    NeedsFullDownload,
    Cancelled,
    ResolverMissing,
    ResolverUnsupported,
    AgeWall,
    LoginRequired,
    GeoBlocked,
    RateLimited,
    NeedsSaving,
}

impl FetchError {
    pub const ALL: &'static [FetchError] = &[
        Self::Unreachable,
        Self::NotFound,
        Self::Forbidden,
        Self::Expired,
        Self::SourceChanged,
        Self::Timeout,
        Self::ServerBusy,
        Self::TooLarge,
        Self::NotMedia,
        Self::Unsupported(UnsupportedKind::Live),
        Self::Unsupported(UnsupportedKind::SeparateTracks),
        Self::Unsupported(UnsupportedKind::Format),
        Self::Drm,
        Self::ProxyRequired,
        Self::ClockSkew,
        Self::DiskFull,
        Self::NoPeers,
        Self::MetadataTimeout,
        Self::NeedsFullDownload,
        Self::Cancelled,
        Self::ResolverMissing,
        Self::ResolverUnsupported,
        Self::AgeWall,
        Self::LoginRequired,
        Self::GeoBlocked,
        Self::RateLimited,
        Self::NeedsSaving,
    ];

    /// The fixed English sentence (the key of the translation tables).
    pub fn sentence(self) -> &'static str {
        match self {
            Self::Unreachable => errors::UNREACHABLE,
            Self::NotFound => errors::NOT_FOUND,
            Self::Forbidden => errors::FORBIDDEN,
            Self::Expired => errors::EXPIRED,
            Self::SourceChanged => errors::SOURCE_CHANGED,
            Self::Timeout => errors::TIMEOUT,
            Self::ServerBusy => errors::SERVER_BUSY,
            Self::TooLarge => errors::TOO_LARGE,
            Self::NotMedia => errors::NO_VIDEO_HERE,
            Self::Unsupported(UnsupportedKind::Live) => errors::LIVE_UNSUPPORTED,
            Self::Unsupported(UnsupportedKind::SeparateTracks) => errors::SEPARATE_TRACKS,
            Self::Unsupported(UnsupportedKind::Format) => errors::UNSUPPORTED_STREAM,
            Self::Drm => errors::PROTECTED,
            Self::ProxyRequired => errors::PROXY_REQUIRED,
            Self::ClockSkew => errors::CLOCK_SKEW,
            Self::DiskFull => errors::DISK_FULL,
            Self::NoPeers => errors::NO_PEERS,
            Self::MetadataTimeout => errors::METADATA_TIMEOUT,
            Self::NeedsFullDownload => errors::NEEDS_FULL_DOWNLOAD,
            Self::Cancelled => errors::CANCELLED,
            Self::ResolverMissing => errors::RESOLVER_MISSING,
            Self::ResolverUnsupported => errors::RESOLVER_UNSUPPORTED,
            Self::AgeWall => errors::AGE_WALL,
            Self::LoginRequired => errors::LOGIN_REQUIRED,
            Self::GeoBlocked => errors::GEO_BLOCKED,
            Self::RateLimited => errors::RATE_LIMITED,
            Self::NeedsSaving => errors::NEEDS_SAVING,
        }
    }

    /// The typed form of a raw internal error, when it has one.
    pub fn from_raw(raw: &str) -> Option<Self> {
        let s = errors::classify(raw);
        Self::ALL.iter().copied().find(|e| e.sentence() == s)
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.sentence())
    }
}

impl std::error::Error for FetchError {}

/// Carried inside the `io::Error` a plain-HTTP fallback reader returns when
/// the file cannot play before it is complete (index at the end, no `Range`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeedsFullDownloadProgress {
    pub written: u64,
    pub total: u64,
}

impl NeedsFullDownloadProgress {
    /// Percent downloaded, 0..=100.
    pub fn percent(&self) -> u8 {
        (self.written.min(self.total) * 100)
            .checked_div(self.total)
            .unwrap_or(0) as u8
    }

    /// Recover it from a reader error.
    pub fn from_io(e: &std::io::Error) -> Option<Self> {
        e.get_ref()?.downcast_ref::<Self>().copied()
    }
}

impl std::fmt::Display for NeedsFullDownloadProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "needs full download ({}%)", self.percent())
    }
}

impl std::error::Error for NeedsFullDownloadProgress {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_are_unique_and_listed() {
        let mut seen = std::collections::HashSet::new();
        for e in FetchError::ALL {
            assert!(errors::ALL.contains(&e.sentence()));
            assert!(seen.insert(e.sentence()), "{e:?}");
        }
    }

    #[test]
    fn raw_keywords_become_typed_errors() {
        assert_eq!(FetchError::from_raw("expired"), Some(FetchError::Expired));
        assert_eq!(FetchError::from_raw("no peers"), Some(FetchError::NoPeers));
        assert_eq!(FetchError::from_raw("HTTP 404"), Some(FetchError::NotFound));
        assert_eq!(
            FetchError::from_raw("cancelled"),
            Some(FetchError::Cancelled)
        );
        assert_eq!(
            FetchError::from_raw("resolver age wall"),
            Some(FetchError::AgeWall)
        );
        assert_eq!(
            FetchError::from_raw("resolver geo"),
            Some(FetchError::GeoBlocked)
        );
        assert_eq!(FetchError::from_raw("resolver drm"), Some(FetchError::Drm));
        assert_eq!(FetchError::from_raw("whatever"), None);
    }

    #[test]
    fn progress_roundtrips_through_io_error() {
        let p = NeedsFullDownloadProgress {
            written: 50,
            total: 200,
        };
        let e = std::io::Error::other(p);
        assert_eq!(NeedsFullDownloadProgress::from_io(&e), Some(p));
        assert_eq!(p.percent(), 25);
        assert_eq!(
            errors::classify(&e.to_string()),
            errors::NEEDS_FULL_DOWNLOAD
        );
    }
}
