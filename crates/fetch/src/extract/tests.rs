use super::*;

fn urls(it: &RawItem) -> Vec<&str> {
    it.candidates.iter().map(|c| c.url.as_str()).collect()
}

#[test]
fn og_video_and_twitter_stream() {
    let p = scan(
        br#"<html><head><title>Plain &amp; title</title>
        <meta property="og:title" content="OG title">
        <meta property="og:video:secure_url" content="https://v.invalid/a.mp4">
        <meta property="og:video:height" content="720">
        <meta property="og:image" content="/t.jpg">
        <meta property="video:duration" content="93">
        <meta name="twitter:player:stream" content="https://v.invalid/tw.mp4">
        </head></html>"#,
    );
    assert_eq!(p.items.len(), 1);
    let it = &p.items[0];
    assert_eq!(it.meta.title.as_deref(), Some("OG title"));
    assert_eq!(it.meta.duration_secs, Some(93));
    assert_eq!(it.meta.thumbnail.as_deref(), Some("/t.jpg"));
    assert_eq!(
        urls(it),
        ["https://v.invalid/a.mp4", "https://v.invalid/tw.mp4"]
    );
    assert_eq!(it.candidates[0].origin, Origin::OpenGraph);
    assert_eq!(it.candidates[0].height, Some(720));
    assert_eq!(it.candidates[1].origin, Origin::Twitter);
}

#[test]
fn og_video_of_an_html_type_is_an_embed_not_a_file() {
    let p = scan(
        br#"<meta property="og:video" content="https://e.invalid/player/1">
        <meta property="og:video:type" content="text/html">"#,
    );
    assert!(p.items[0].candidates.is_empty());
    assert_eq!(p.iframes, ["https://e.invalid/player/1"]);
}

#[test]
fn jsonld_graph_itemlist_makes_items() {
    let p = scan(
        br#"<script type="application/ld+json">{"@context":"x","@graph":[
          {"@type":"WebPage","name":"page"},
          {"@type":"VideoObject","name":"One","contentUrl":"https://v.invalid/1.mp4",
           "duration":"PT1M5S","thumbnailUrl":["https://i.invalid/1.jpg"],"height":"720","bitrate":"2000 kbps"},
          {"@type":"ItemList","itemListElement":[
            {"@type":"ListItem","item":{"@type":"VideoObject","name":"Two","contentUrl":"https://v.invalid/2.m3u8"}},
            {"@type":"ListItem","item":{"@type":["Thing","VideoObject"],"name":"Three",
              "embedUrl":"https://e.invalid/3"}}]}]}</script>"#,
    );
    assert_eq!(p.items.len(), 2);
    assert_eq!(p.items[0].meta.title.as_deref(), Some("One"));
    assert_eq!(p.items[0].meta.duration_secs, Some(65));
    assert_eq!(p.items[0].candidates[0].height, Some(720));
    assert_eq!(p.items[0].candidates[0].bitrate, Some(2_000_000));
    assert_eq!(p.items[1].candidates[0].proto, Proto::Hls);
    assert_eq!(p.items[1].candidates[0].origin, Origin::JsonLd);
    assert_eq!(p.iframes, ["https://e.invalid/3"]);
}

#[test]
fn one_jsonld_video_joins_the_page_item() {
    let p = scan(
        br#"<meta property="og:video" content="https://v.invalid/1.mp4">
        <script type="application/ld+json">{"@type":"VideoObject","name":"N",
        "contentUrl":"https://v.invalid/1.mp4"}</script>"#,
    );
    assert_eq!(p.items.len(), 1);
    assert_eq!(urls(&p.items[0]), ["https://v.invalid/1.mp4"]);
    assert_eq!(p.items[0].candidates[0].origin, Origin::JsonLd);
    assert_eq!(p.items[0].meta.title.as_deref(), Some("N"));
}

#[test]
fn video_and_source_tags_with_labels() {
    let p = scan(
        br#"<video controls poster="/p.jpg"><source src="/lo.mp4" type="video/mp4" label="360p">
        <source src="/hi.mp4" type="video/mp4" res="1280x720">
        <source src="/s.m4a" type="audio/mp4"><source src="/m" type="application/x-mpegURL"></video>
        <source src="/orphan.mp4">"#,
    );
    let it = &p.items[0];
    assert_eq!(urls(it), ["/lo.mp4", "/hi.mp4", "/m"]);
    assert_eq!(it.candidates[0].height, Some(360));
    assert_eq!(it.candidates[1].height, Some(720));
    assert_eq!(it.candidates[2].proto, Proto::Hls);
    assert_eq!(it.meta.thumbnail.as_deref(), Some("/p.jpg"));
}

#[test]
fn link_alternate_manifests() {
    let p = scan(
        br#"<link rel="alternate" type="application/x-mpegURL" href="/a.m3u8">
        <link rel="alternate" type="application/dash+xml" href="/b">
        <link rel="alternate" type="application/rss+xml" href="/feed">
        <link rel="stylesheet" type="application/x-mpegURL" href="/c">"#,
    );
    let it = &p.items[0];
    assert_eq!(urls(it), ["/a.m3u8", "/b"]);
    assert_eq!(it.candidates[1].proto, Proto::Dash);
    assert!(
        it.candidates
            .iter()
            .all(|c| c.origin == Origin::LinkAlternate)
    );
}

#[test]
fn script_blobs_sources_array_and_escaped_urls() {
    let p = scan(
        br#"<script>var player = {sources:[{file:"https:\/\/c.invalid\/hi.mp4",label:"1080p"},
        {file:"https:\/\/c.invalid\/lo.mp4",label:"480p"}]};</script>
        <script type="application/json">{"hls":"https://c.invalid/x.m3u8"}</script>
        <script src="https://c.invalid/lib.js"></script>"#,
    );
    let it = &p.items[0];
    assert_eq!(
        urls(it),
        [
            "https://c.invalid/hi.mp4",
            "https://c.invalid/lo.mp4",
            "https://c.invalid/x.m3u8"
        ]
    );
    assert_eq!(it.candidates[0].height, Some(1080));
    assert!(it.candidates.iter().all(|c| c.origin == Origin::Script));
}

#[test]
fn flashvars_percent_encoded() {
    let p = scan(
        br#"<script>var flashvars = "a=1&video_url=http%3A%2F%2Fc.invalid%2Ffv%2Fclip.flv&b=2";</script>"#,
    );
    assert_eq!(urls(&p.items[0]), ["http://c.invalid/fv/clip.flv"]);
}

#[test]
fn preview_videos_rank_last() {
    let p = scan(
        br#"<video muted loop autoplay src="/preview.mp4"></video>
        <video controls src="/main.mp4"></video>
        <script>x={file:"https://c.invalid/s.mp4"}</script>"#,
    );
    let it = &p.items[0];
    assert_eq!(
        urls(it),
        ["/main.mp4", "https://c.invalid/s.mp4", "/preview.mp4"]
    );
    assert_eq!(it.candidates[2].origin, Origin::Preview);
}

#[test]
fn iframes_and_embedurl_collected_capped_at_3() {
    let mut page = String::new();
    for i in 0..10 {
        page.push_str(&format!("<iframe src=\"https://e.invalid/{i}\"></iframe>"));
    }
    page.push_str("<iframe src=\"javascript:alert(1)\"></iframe>");
    let p = scan(page.as_bytes());
    assert_eq!(p.iframes.len(), MAX_IFRAMES);
    assert_eq!(p.iframes[0], "https://e.invalid/0");
    let p = scan(b"<iframe src=\"javascript:alert(1)\"></iframe><iframe src=\"data:text/html,x\">");
    assert!(p.iframes.is_empty());
}

#[test]
fn meta_refresh_is_recorded_not_followed() {
    let p = scan(
        br#"<meta http-equiv="Refresh" content="0; url=https://r.invalid/next"><base href="https://b.invalid/dir/">"#,
    );
    assert!(p.signals.meta_refresh);
    assert_eq!(p.base_href.as_deref(), Some("https://b.invalid/dir/"));
    assert!(p.iframes.is_empty() && p.items[0].candidates.is_empty());
    assert!(!scan(b"<p>hi</p>").signals.meta_refresh);
}

#[test]
fn signals() {
    let age =
        scan(b"<body><h1>Are you 18 or older?</h1><form><button>Enter</button></form></body>");
    assert!(age.signals.age_gate && !age.signals.password_form);
    // The phrase alone, with nothing to press, is not a gate.
    assert!(!scan(b"<p>adults only</p>").signals.age_gate);
    let pw = scan(b"<form><input type=\"password\" name=p></form>");
    assert!(pw.signals.password_form);
    assert!(
        scan(b"<script>navigator.requestMediaKeySystemAccess('com.widevine.alpha')</script>")
            .signals
            .drm
    );
    assert!(scan(b"<p>Widevine protected</p>").signals.drm);
    let spa = scan(
        b"<div id=\"root\"></div><script src=a></script><script src=b></script><script src=c></script>",
    );
    assert!(spa.signals.scripted);
    let nos = scan(
        b"<noscript>enable</noscript><script src=a></script><script src=b></script><script src=c></script>",
    );
    assert!(nos.signals.scripted);
    assert!(!scan(b"<div id=\"root\">hello</div><script src=a></script><script src=b></script><script src=c></script>").signals.scripted);
    assert!(
        !scan(b"<noscript>x</noscript><script src=a></script>")
            .signals
            .scripted
    );
}

#[test]
fn title_order_and_sanitizing() {
    let p = scan(b"<title>  Doc \x07title\xE2\x80\xAE </title>");
    assert_eq!(p.items[0].meta.title.as_deref(), Some("Doc title"));
    let p = scan(
        br#"<title>T</title><meta name="twitter:title" content="TW">
        <script type="application/ld+json">{"@type":"VideoObject","name":"LD","contentUrl":"/x.mp4"}</script>"#,
    );
    assert_eq!(p.items[0].meta.title.as_deref(), Some("LD"));
    let p = scan(b"<title>T</title><meta name=\"twitter:title\" content=\"TW\">");
    assert_eq!(p.items[0].meta.title.as_deref(), Some("TW"));
    let p = scan(
        br#"<title>T</title><meta property="og:title" content="OG"><meta name="twitter:title" content="TW">"#,
    );
    assert_eq!(p.items[0].meta.title.as_deref(), Some("OG"));
    let long = format!("<title>{}</title>", "x".repeat(500));
    assert_eq!(
        scan(long.as_bytes()).items[0]
            .meta
            .title
            .clone()
            .unwrap()
            .chars()
            .count(),
        120
    );
}

#[test]
fn iso_duration_from_jsonld() {
    let p = scan(
        br#"<script type="application/ld+json">{"@type":"VideoObject","contentUrl":"/a.mp4","duration":"PT1H2M3S"}</script>"#,
    );
    assert_eq!(p.items[0].meta.duration_secs, Some(3723));
}

#[test]
fn candidates_are_deduped_and_capped() {
    let mut page = String::new();
    for i in 0..200 {
        page.push_str(&format!(
            "<video src=\"/v{i}.mp4\"></video><video src=\"/v{i}.mp4\"></video>"
        ));
    }
    let p = scan(page.as_bytes());
    assert_eq!(p.items[0].candidates.len(), MAX_CANDIDATES);
    let mut u: Vec<_> = urls(&p.items[0]);
    u.dedup();
    assert_eq!(u.len(), MAX_CANDIDATES);
}

#[test]
fn nothing_found_is_one_empty_item() {
    let p = scan(b"");
    assert_eq!(p.items.len(), 1);
    assert!(p.items[0].candidates.is_empty() && p.iframes.is_empty());
    assert_eq!(p.signals, Signals::default());
}
