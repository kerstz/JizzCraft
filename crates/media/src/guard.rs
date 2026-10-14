//! Keeps GStreamer local. JizzCraft only hands GStreamer `file://` URIs, but a
//! file can still *describe* remote media: an HLS or DASH playlist saved as
//! `.mp4` makes the adaptive demuxers download every URL it lists.
//!
//! Two layers, both limited to this process (the plugin registry on disk is
//! never modified):
//!
//! 1. [`harden_registry`], once at start-up: URI handlers for anything but
//!    local schemes are removed from the registry, and every element that
//!    fetches what a file points to (adaptive, network and manifest demuxers)
//!    is ranked `NONE`, so decodebin never selects it.
//! 2. [`watch`], on every pipeline: if such an element, or any source other
//!    than `filesrc`, is created anyway, nothing reaches its sink pads, the
//!    pipeline posts a [`REFUSED`] error and its owner sets it to `Null`.
//!
//! One pipeline kind is allowed one more source: the in-process stream player
//! (`Player::open_stream`) feeds bytes it read from a local reader through an
//! `appsrc`. That is [`watch_with`] with `["appsrc"]`; the fetchers are still
//! refused there, so a playlist streamed through it is caught like any other.
//!
//! A playlist dressed up as an `.mp4` is still a playlist. The bouncer has
//! seen that costume before.

use gstreamer as gst;
use gstreamer::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Error text for media that refers to the network.
pub const REFUSED: &str =
    "refused: this file refers to network media, and JizzCraft only reads local files";

/// URI schemes that stay on this machine and that pipelines need.
const LOCAL_SCHEMES: &[&str] = &["file", "appsrc", "appsink"];

/// Media types that are lists of locations rather than media.
const MANIFEST_CAPS: &[&str] = &[
    "application/x-hls",
    "application/dash+xml",
    "application/vnd.ms-sstr+xml",
    "application/sdp",
    "text/uri-list",
];

/// Known fetchers, in case a future version changes their class or caps.
const FETCHERS: &[&str] = &[
    "hlsdemux",
    "hlsdemux2",
    "dashdemux",
    "dashdemux2",
    "mssdemux",
    "mssdemux2",
    "sdpdemux",
];

/// The only sources a pipeline may contain: the one that reads the file
/// (`urisourcebin` wraps it when playbin3 is selected).
const SOURCES: &[&str] = &["filesrc", "urisourcebin"];

fn klass_has(f: &gst::ElementFactory, part: &str) -> bool {
    f.metadata(gst::ELEMENT_METADATA_KLASS)
        .is_some_and(|k| k.split('/').any(|c| c == part))
}

fn is_uri_handler(f: &gst::ElementFactory) -> bool {
    f.uri_type() != gst::URIType::Unknown
}

/// Could this element reach the network, directly or by following a file?
pub fn can_reach_network(f: &gst::ElementFactory) -> bool {
    if is_uri_handler(f)
        && f.uri_protocols()
            .iter()
            .any(|p| !LOCAL_SCHEMES.contains(&p.to_ascii_lowercase().as_str()))
    {
        return true;
    }
    if FETCHERS.contains(&f.name().as_str()) || klass_has(f, "Network") || klass_has(f, "Adaptive")
    {
        return true;
    }
    f.static_pad_templates().iter().any(|t| {
        let caps = t.caps();
        t.direction() == gst::PadDirection::Sink
            && !caps.is_any()
            && caps
                .iter()
                .any(|s| MANIFEST_CAPS.contains(&s.name().as_str()))
    })
}

pub use jizzcraft_core::manifest::looks_like_manifest;

/// Does a missing-plugin installer detail ask for a streaming manifest?
pub fn is_manifest_detail(detail: &str) -> bool {
    MANIFEST_CAPS.iter().any(|c| detail.contains(c))
}

/// Take network-capable elements out of this process's registry. Ranks set
/// to `NONE` survive a plugin being loaded later; removed URI handlers would
/// come back only with their plugin, which [`watch`] then catches.
pub(crate) fn harden_registry() {
    let registry = gst::Registry::get();
    for feature in registry.features(gst::ElementFactory::static_type()) {
        let Ok(f) = feature.downcast::<gst::ElementFactory>() else {
            continue;
        };
        if !can_reach_network(&f) {
            continue;
        }
        f.set_rank(gst::Rank::NONE);
        if is_uri_handler(&f) {
            registry.remove_feature(&f);
        }
    }
}

fn starve(pad: &gst::Pad) {
    pad.add_probe(gst::PadProbeType::DATA_DOWNSTREAM, |_, _| {
        gst::PadProbeReturn::Drop
    });
}

/// Watch a whole pipeline. The flag turns `true` when an element was refused;
/// the pipeline has then posted a [`REFUSED`] error.
pub(crate) fn watch(pipeline: &gst::Bin) -> Arc<AtomicBool> {
    watch_with(pipeline, &[])
}

/// [`watch`], additionally allowing the named source elements.
pub(crate) fn watch_with(
    pipeline: &gst::Bin,
    extra_sources: &'static [&'static str],
) -> Arc<AtomicBool> {
    let refused = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&refused);
    pipeline.connect_deep_element_added(move |top, _, el| {
        let Some(f) = el.factory() else { return };
        let name = f.name();
        let stray_source = klass_has(&f, "Source")
            && !SOURCES.contains(&name.as_str())
            && !extra_sources.contains(&name.as_str());
        if !stray_source && !can_reach_network(&f) {
            return;
        }
        // Added before any data flows: starved, it never sees a location.
        for pad in el.sink_pads() {
            starve(&pad);
        }
        el.connect_pad_added(|_, pad| {
            if pad.direction() == gst::PadDirection::Sink {
                starve(pad);
            }
        });
        if !refused.swap(true, Ordering::SeqCst) {
            let msg = gst::message::Error::builder(gst::ResourceError::NotAuthorized, REFUSED)
                .src(top)
                .build();
            let _ = top.post_message(msg);
        }
    });
    flag
}

/// The top-level bin `el` lives in.
pub(crate) fn top_bin(el: &gst::Element) -> Option<gst::Bin> {
    let mut cur: gst::Object = el.clone().upcast();
    while let Some(p) = cur.parent() {
        cur = p;
    }
    cur.downcast::<gst::Bin>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_keeps_local_elements_and_drops_network_ones() {
        crate::init().unwrap();
        // Core elements always exist; the rest when gst-plugins-base does.
        let mut local = vec!["filesrc", "typefind"];
        if gst::ElementFactory::find("videoconvert").is_some() {
            local.extend(["appsink", "uridecodebin", "playbin"]);
        }
        for name in local {
            assert!(gst::ElementFactory::find(name).is_some(), "{name} removed");
        }
        for name in ["souphttpsrc", "curlhttpsrc", "giosrc", "rtspsrc", "udpsrc"] {
            assert!(gst::ElementFactory::find(name).is_none(), "{name} kept");
        }
        // Demuxers stay registered (a plugin load would re-add them) but are
        // never autoplugged.
        for name in FETCHERS {
            if let Some(f) = gst::ElementFactory::find(name) {
                assert_eq!(f.rank(), gst::Rank::NONE, "{name}");
            }
        }
    }

    #[test]
    fn manifests_are_recognised_by_their_head() {
        assert!(looks_like_manifest(b"#EXTM3U\n#EXT-X-VERSION:3\n"));
        assert!(looks_like_manifest(b"\xef\xbb\xbf  #EXTM3U\n"));
        assert!(looks_like_manifest(
            b"<?xml version=\"1.0\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\">"
        ));
        assert!(looks_like_manifest(b"v=0\no=- 1 1 IN IP4 127.0.0.1\ns=x\n"));
        assert!(looks_like_manifest(
            b"# list\nhttp://127.0.0.1/a.mp4\nhttp://127.0.0.1/b.mp4\n"
        ));
        // Media and ordinary text are not manifests.
        assert!(!looks_like_manifest(&[0x1a, 0x45, 0xdf, 0xa3, 0x01, 0x00]));
        assert!(!looks_like_manifest(b"\x00\x00\x00\x18ftypmp42"));
        assert!(!looks_like_manifest(b"hello world\n"));
        assert!(!looks_like_manifest(b""));
    }

    #[test]
    fn appsrc_is_refused_unless_allowed() {
        crate::init().unwrap();
        let (Ok(a), Ok(b), Ok(t)) = (
            gst::ElementFactory::make("appsrc").build(),
            gst::ElementFactory::make("appsrc").build(),
            gst::ElementFactory::make("videotestsrc").build(),
        ) else {
            return;
        };
        let strict = gst::Pipeline::new();
        let r1 = watch(strict.upcast_ref());
        strict.add(&a).unwrap();
        assert!(
            r1.load(Ordering::SeqCst),
            "appsrc must trip the strict watch"
        );

        let lenient = gst::Pipeline::new();
        let r2 = watch_with(lenient.upcast_ref(), &["appsrc"]);
        lenient.add(&b).unwrap();
        assert!(!r2.load(Ordering::SeqCst), "appsrc allowed for streams");
        lenient.add(&t).unwrap();
        assert!(r2.load(Ordering::SeqCst), "other sources still refused");
    }

    #[test]
    fn a_stray_source_trips_the_watch() {
        crate::init().unwrap();
        let Ok(src) = gst::ElementFactory::make("videotestsrc").build() else {
            return; // gst-plugins-base not installed
        };
        let pipeline = gst::Pipeline::new();
        let refused = watch(pipeline.upcast_ref());
        pipeline.add(&src).unwrap();
        assert!(refused.load(Ordering::SeqCst));
        let msg = pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(1), &[gst::MessageType::Error])
            .expect("error posted");
        let gst::MessageView::Error(e) = msg.view() else {
            unreachable!()
        };
        assert_eq!(e.error().message(), REFUSED);
        assert_eq!(
            top_bin(&src).as_ref(),
            Some(pipeline.upcast_ref::<gst::Bin>())
        );
    }
}
