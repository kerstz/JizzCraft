#!/usr/bin/env python3
"""Builds docs/launch/launch-kit.html from docs/launch/posts.md.

One self-contained page: the texts come from posts.md (edit that file, not the
page), the GIF, the social preview and two screenshots are embedded as data
URIs, so it works from file:// and offline. Standard library only; when
fontTools is installed the embedded fonts are subset to Latin to save space.

Usage: python3 docs/launch/make-launch-kit.py   (from the repository root)
"""

import base64
import html
import io
import json
import re
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
POSTS = ROOT / "docs/launch/posts.md"
OUT = ROOT / "docs/launch/launch-kit.html"
REPO = "https://github.com/kerstz/JizzCraft"

MEDIA = {
    "demo": ("docs/media/demo.gif", "image/gif", "jizzcraft-demo.gif"),
    "social": ("docs/media/social-preview.png", "image/png", "jizzcraft-social-preview.png"),
    "library": ("docs/screenshots/library.png", "image/png", "jizzcraft-library.png"),
    "player": ("docs/screenshots/player.png", "image/png", "jizzcraft-player.png"),
}
FONTS = {
    "Archivo": "assets/fonts/Archivo-Medium.ttf",
    "Inter": "assets/fonts/Inter-Regular.ttf",
    "JetBrains Mono": "assets/fonts/JetBrainsMono-Regular.ttf",
}


def data_uri(path, mime):
    return f"data:{mime};base64," + base64.b64encode((ROOT / path).read_bytes()).decode()


def font_uri(path):
    raw = (ROOT / path).read_bytes()
    try:
        from fontTools import subset
        from fontTools.ttLib import TTFont

        font = TTFont(io.BytesIO(raw))
        opts = subset.Options()
        opts.flavor = None
        opts.layout_features = ["kern", "liga", "calt", "tnum"]
        sub = subset.Subsetter(opts)
        sub.populate(unicodes=list(range(0x20, 0x17F)) + [0x2013, 0x2014, 0x2018, 0x2019, 0x201C,
                                                           0x201D, 0x2022, 0x2026, 0x2190, 0x2191,
                                                           0x2192, 0x2193, 0x2713, 0xD7])
        sub.subset(font)
        buf = io.BytesIO()
        font.save(buf)
        raw = buf.getvalue()
    except ImportError:
        pass
    return "data:font/ttf;base64," + base64.b64encode(raw).decode()


def image_size(path):
    b = (ROOT / path).read_bytes()
    if b[:4] == b"GIF8":
        return struct.unpack("<HH", b[6:10])
    return struct.unpack(">II", b[16:24])


def unwrap(lines):
    """Join soft-wrapped Markdown lines: one line per paragraph or list item."""
    out = []
    for line in lines:
        s = line.rstrip()
        if not s:
            if out and out[-1] != "":
                out.append("")
            continue
        if re.match(r"\s*([-*]|\d+\.)\s", s) and not s.startswith("  "):
            out.append(s.strip())
        elif out and out[-1] != "":
            out[-1] += " " + s.strip()
        else:
            out.append(s.strip())
    while out and out[-1] == "":
        out.pop()
    return "\n".join(out)


def plain(text):
    """Hacker News has no Markdown: drop the marks, keep list items apart."""
    text = re.sub(r"\*\*(.+?)\*\*", r"\1", text)
    text = re.sub(r"`([^`]+)`", r"\1", text)
    return re.sub(r"\n(- )", r"\n\n\1", text)


def bullets(block):
    items, cur = [], None
    for line in block.splitlines():
        if line.startswith("- "):
            cur = [line[2:].strip()]
            items.append(cur)
        elif cur is not None and line.startswith("  "):
            cur.append(line.strip())
    return [" ".join(i) for i in items]


def inline(md):
    """Escape, then the few inline marks posts.md uses."""
    t = html.escape(md)
    t = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", t)
    t = re.sub(r"`([^`]+)`", r"<code>\1</code>", t)
    t = re.sub(r"(?<![\w*])\*([^*]+)\*(?![\w*])", r"<em>\1</em>", t)
    t = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r"\1", t)
    return t


def parse(md):
    head, *sections = re.split(r"\n---\n", md)
    before = bullets(head.split("Before posting anywhere:", 1)[1].strip().split("\n\n", 1)[0])
    posts, order, after, order_note = [], [], [], ""
    for sec in sections:
        sec = sec.strip()
        m = re.match(r"## (.+)", sec)
        if not m:
            continue
        name = m.group(1).strip()
        body = sec[m.end():].strip()
        if name == "After posting":
            after = bullets(body)
            continue
        post = {"name": name, "notes": [], "titles": [], "url": "", "body": "", "body_label": ""}
        quote, para, mode = [], [], ""

        def flush():
            if para:
                post["notes"].append(unwrap(para))
                para.clear()

        for line in body.splitlines():
            label = re.match(r"\*\*([^*]+?):?\*\*:?\s*(.*)", line)
            if line.startswith(">"):
                flush()
                quote.append(re.sub(r"^> ?", "", line))
            elif label and label.group(1).startswith("Title"):
                flush()
                mode = "title"
                post["titles"] += re.findall(r"`([^`]+)`", line)
            elif mode == "title" and line.startswith("- `"):
                post["titles"] += re.findall(r"`([^`]+)`", line)
            elif label and label.group(1) == "URL":
                flush()
                post["url"] = label.group(2).strip()
            elif label and re.match(r"(Body|First comment)", label.group(1)):
                flush()
                post["body_label"] = label.group(1).strip()
            elif not line.strip():
                flush()
                mode = "" if mode != "title" else mode
            else:
                if mode == "title" and not line.startswith("- "):
                    mode = ""
                para.append(line)
        flush()
        # An alternative title mentioned in a note ("the alternative is `…`").
        for n in post["notes"]:
            if "alternative" in n:
                post["titles"] += [t for t in re.findall(r"`([^`]+)`", n) if t not in post["titles"]]
        post["body"] = unwrap(quote)
        posts.append(post)
    om = re.search(r"## Order\n\n(.+?)\n\n(\|.+?)\n\n(.+?)$", head, re.S)
    if om:
        order_note = unwrap(om.group(1).splitlines())
        for row in om.group(2).splitlines()[2:]:
            cells = [c.strip() for c in row.strip("|").split("|")]
            order.append({"day": int(cells[0]), "where": cells[1], "attach": cells[2]})
    return before, posts, order, order_note, after


def sub_links(name):
    m = re.match(r"r/(\w+)", name)
    if m:
        r = f"https://www.reddit.com/r/{m.group(1)}"
        return [("Rules", f"{r}/about/rules"), ("Submit", f"{r}/submit")]
    if "Hacker News" in name:
        return [("Show HN rules", "https://news.ycombinator.com/showhn.html"),
                ("Submit", "https://news.ycombinator.com/submit")]
    return []


def attach_key(text):
    if "demo.gif" in text:
        return "demo"
    return None


def build():
    md = POSTS.read_text(encoding="utf-8")
    before, posts, order, order_note, after = parse(md)
    if not posts or not order:
        sys.exit("posts.md: no posts or no Order table found")
    by_name = {p["name"]: p for p in posts}
    for o in order:
        if o["where"] not in by_name:
            sys.exit(f"posts.md: Order names `{o['where']}`, which has no section")
    # Each file is embedded once (JSON, base64) and turned into a blob: URL
    # by the script, so the GIF shown three times weighs once.
    blobs = {k: {"mime": mime, "b64": base64.b64encode((ROOT / p).read_bytes()).decode()}
             for k, (p, mime, _) in MEDIA.items()}
    sizes = {k: (ROOT / p).stat().st_size for k, (p, _, _) in MEDIA.items()}
    dims = {k: image_size(p) for k, (p, _, _) in MEDIA.items()}
    fonts = "\n".join(
        f"@font-face{{font-family:'{n} Kit';src:url({font_uri(p)}) format('truetype');font-display:swap}}"
        for n, p in FONTS.items()
    )

    def mb(n):
        return f"{n / 1_000_000:.1f} MB" if n >= 1_000_000 else f"{round(n / 1000)} KB"

    copies = {}

    def copy_btn(key, text, label="Copy"):
        copies[key] = text
        return (f'<button class="copy" type="button" data-copy="{key}" '
                f'aria-label="{html.escape(label)}">{html.escape(label)}</button>')

    days = {}
    for o in order:
        days.setdefault(o["day"], []).append(o)

    runway = []
    for d, items in sorted(days.items()):
        names = "".join(
            f'<a href="#{slug(o["where"])}" class="rw-item" data-post="{slug(o["where"])}">'
            f'<span class="tick" aria-hidden="true"></span>{html.escape(short(o["where"]))}</a>'
            for o in items)
        runway.append(f'<li class="rw-day"><span class="rw-num">Day {d}</span>{names}</li>')

    cards = []
    for o in order:
        p = by_name[o["where"]]
        s = slug(p["name"])
        hn = "Hacker News" in p["name"]
        notes = "".join(f"<p>{inline(n)}</p>" for n in p["notes"])
        titles = ""
        if p["titles"]:
            rows = "".join(
                f'<li class="line"><span class="line-text">{html.escape(t)}</span>'
                f'<span class="line-meta">{len(t)} chars</span>{copy_btn(f"{s}-t{i}", t)}</li>'
                for i, t in enumerate(p["titles"]))
            label = "Title, pick one" if len(p["titles"]) > 1 else "Title"
            titles = f'<h4>{label}</h4><ul class="lines">{rows}</ul>'
        url = ""
        if p["url"]:
            url = (f'<h4>URL</h4><ul class="lines"><li class="line"><span class="line-text">'
                   f'{html.escape(p["url"])}</span>{copy_btn(f"{s}-url", p["url"])}</li></ul>')
        body = ""
        if p["body"]:
            text = plain(p["body"]) if hn else p["body"]
            fmt = "plain text, HN has no Markdown" if hn else "Markdown"
            body = (f'<div class="body-head"><h4>{html.escape(p["body_label"] or "Body")}</h4>'
                    f'<span class="line-meta">{fmt}</span>{copy_btn(f"{s}-body", text)}</div>'
                    f'<pre class="body">{html.escape(text)}</pre>')
        key = attach_key(o["attach"])
        if key:
            w, h = dims[key]
            attach = (f'<div class="attach"><img data-media="{key}" alt="" width="96" height="{round(96 * h / w)}">'
                      f'<div><strong>Attach the demo GIF</strong><span>{inline(o["attach"])}</span></div>'
                      f'<a class="btn" data-media-dl="{key}" download="{MEDIA[key][2]}">Download GIF</a></div>')
        else:
            attach = f'<div class="attach attach-none"><div><strong>No attachment</strong><span>{inline(o["attach"])}</span></div></div>'
        links = "".join(f'<a href="{u}" target="_blank" rel="noopener">{t}</a>' for t, u in sub_links(p["name"]))
        cards.append(f'''
<article class="post" id="{s}">
  <header class="post-head">
    <div><span class="post-day">Day {o["day"]}</span><h3>{html.escape(p["name"])}</h3></div>
    <div class="post-links">{links}</div>
    <label class="posted"><input type="checkbox" data-store="posted:{s}"> Posted</label>
  </header>
  <div class="post-notes">{notes}</div>
  {titles}{url}{body}{attach}
</article>''')

    def checklist(items, prefix):
        return "".join(
            f'<li><label><input type="checkbox" data-store="{prefix}:{i}"><span>{inline(t)}</span></label></li>'
            for i, t in enumerate(items))

    media_cards = []
    for key, title, use in [
        ("demo", "Demo GIF", "Day 1 and day 3 posts; the README shows the same file."),
        ("social", "Social preview", "Upload once in Settings, General, Social preview. Link previews use it."),
        ("library", "Screenshot: library", "Spare image for comments and replies."),
        ("player", "Screenshot: player", "Spare image for comments and replies."),
    ]:
        w, h = dims[key]
        media_cards.append(f'''
<figure class="media{' media-wide' if key in ('demo', 'social') else ''}">
  <img data-media="{key}" alt="{html.escape(title)}" width="{w}" height="{h}">
  <figcaption><strong>{title}</strong><span>{w} × {h}, {mb(sizes[key])}. {use}</span>
  <a class="btn" data-media-dl="{key}" download="{MEDIA[key][2]}">Download</a></figcaption>
</figure>''')

    page = TEMPLATE
    for k, v in {
        "FONTS": fonts,
        "ICON": data_uri("assets/icons/hicolor/128x128/apps/io.github.kerstz.JizzCraft.png", "image/png"),
        "RUNWAY": "".join(runway),
        "ORDER_NOTE": inline(order_note),
        "MEDIA": "".join(media_cards),
        "BEFORE": checklist(before, "before"),
        "POSTS": "".join(cards),
        "AFTER": checklist(after, "after"),
        "BLOBS": json.dumps(blobs),
        "COPIES": json.dumps(copies, ensure_ascii=False).replace("</", "<\\/"),
        "REPO": REPO,
        "COUNT": str(len(order)),
        "DAYS": str(len(days)),
    }.items():
        page = page.replace("{{" + k + "}}", v)
    OUT.write_text(page, encoding="utf-8")
    print(f"{OUT.relative_to(ROOT)}: {OUT.stat().st_size / 1_000_000:.2f} MB")


def slug(name):
    return re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")


def short(name):
    return "Show HN" if "Show HN" in name else name


TEMPLATE = r"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>JizzCraft launch kit</title>
<link rel="icon" href="{{ICON}}">
<style>
{{FONTS}}
:root {
  --bg: #f4f4f2; --surface: #ffffff; --sunken: #ebeae6; --ink: #17181b; --muted: #5d5a55;
  --faint: #8b877f; --line: rgba(23, 24, 27, .12); --line-strong: rgba(23, 24, 27, .32);
  --app: #c4502f; --app-soft: rgba(255, 138, 107, .18); --on-app: #ffffff; --ok: #24785a;
  --display: 'Archivo Kit', 'Archivo', system-ui, sans-serif;
  --text: 'Inter Kit', 'Inter', system-ui, sans-serif;
  --mono: 'JetBrains Mono Kit', 'JetBrains Mono', ui-monospace, monospace;
  color-scheme: light;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --bg: #121316; --surface: #18191d; --sunken: #0c0c0e; --ink: #f2f1ee; --muted: #b9b5ae;
    --faint: #8a8680; --line: rgba(242, 238, 230, .1); --line-strong: rgba(242, 238, 230, .3);
    --app: #ff8a6b; --app-soft: rgba(255, 138, 107, .14); --on-app: #121316; --ok: #6fd3a5;
    color-scheme: dark;
  }
}
:root[data-theme="dark"] {
  --bg: #121316; --surface: #18191d; --sunken: #0c0c0e; --ink: #f2f1ee; --muted: #b9b5ae;
  --faint: #8a8680; --line: rgba(242, 238, 230, .1); --line-strong: rgba(242, 238, 230, .3);
  --app: #ff8a6b; --app-soft: rgba(255, 138, 107, .14); --on-app: #121316; --ok: #6fd3a5;
  color-scheme: dark;
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; scroll-behavior: smooth; }
@media (prefers-reduced-motion: reduce) { html { scroll-behavior: auto; } }
body { margin: 0; background: var(--bg); color: var(--ink); font: 16px/1.55 var(--text); }
a { color: inherit; }
img { max-width: 100%; height: auto; display: block; }
code { font: .88em var(--mono); background: var(--sunken); padding: .08em .3em; }
:focus-visible { outline: 2px solid var(--app); outline-offset: 2px; }
.wrap { max-width: 1080px; margin: 0 auto; padding: 0 16px; }

.top { border-top: 6px solid var(--app); padding: 40px 0 28px; }
.brand { display: flex; align-items: center; gap: 16px; }
.brand img { width: 56px; height: 56px; }
.brand h1 { font: 500 clamp(30px, 5vw, 44px)/1.05 var(--display); margin: 0; letter-spacing: -.01em; }
.brand p { margin: 4px 0 0; color: var(--muted); }
.theme { margin-left: auto; align-self: flex-start; }

.runway { list-style: none; margin: 28px 0 0; padding: 0; display: grid;
  grid-template-columns: repeat({{DAYS}}, minmax(0, 1fr)); border: 1px solid var(--line-strong); }
.rw-day { padding: 16px 16px 18px; border-right: 1px solid var(--line-strong); display: flex; flex-direction: column; gap: 6px; }
.rw-day:last-child { border-right: 0; }
.rw-num { font: 500 clamp(26px, 4vw, 40px)/1 var(--display); margin-bottom: 6px; }
.rw-item { display: flex; align-items: center; gap: 8px; text-decoration: none; font-weight: 500; width: fit-content; }
.rw-item:hover { text-decoration: underline; text-underline-offset: 3px; }
.tick { width: 14px; height: 14px; border: 1.5px solid var(--line-strong); flex: none; display: grid; place-items: center; }
.rw-item.done .tick { background: var(--app); border-color: var(--app); }
.rw-item.done .tick::after { content: ""; width: 7px; height: 4px; border: 2px solid var(--on-app); border-top: 0; border-right: 0; transform: rotate(-45deg) translate(1px, -1px); }
.rw-item.done { color: var(--muted); }
.order-note { color: var(--muted); max-width: 72ch; margin: 14px 0 0; font-size: 15px; }
.progress { font: 13px var(--mono); color: var(--faint); margin: 10px 0 0; }

section { padding: 36px 0 8px; }
h2 { font: 500 26px/1.2 var(--display); margin: 0 0 6px; }
.lead { color: var(--muted); margin: 0 0 20px; max-width: 72ch; }
h3 { font: 500 24px/1.2 var(--display); margin: 2px 0 0; }
h4 { font: 500 13px/1.3 var(--text); color: var(--muted); margin: 20px 0 8px; }

.btn, .copy, .theme {
  font: 500 13px/1 var(--text); color: var(--ink); background: transparent; border: 1px solid var(--line-strong);
  padding: 9px 12px; cursor: pointer; text-decoration: none; display: inline-flex; align-items: center; gap: 6px; white-space: nowrap; }
.btn:hover, .copy:hover, .theme:hover { border-color: var(--ink); }
.copy { min-width: 74px; justify-content: center; }
.copy.ok { background: var(--app); border-color: var(--app); color: var(--on-app); }
.copy.fail { border-color: var(--app); color: var(--app); }

.media-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; }
.media { margin: 0; background: var(--surface); border: 1px solid var(--line); display: flex; flex-direction: column; }
.media img { background: var(--sunken); width: 100%; }
.media figcaption { padding: 14px 16px 16px; display: grid; gap: 4px 12px; grid-template-columns: 1fr auto; align-items: start; }
.media figcaption span { color: var(--muted); font-size: 14px; grid-column: 1; }
.media figcaption .btn { grid-column: 2; grid-row: 1 / span 2; }

.checks { list-style: none; padding: 0; margin: 0; border-top: 1px solid var(--line); max-width: 820px; }
.checks li { border-bottom: 1px solid var(--line); }
.checks label { display: flex; gap: 12px; padding: 12px 2px; cursor: pointer; align-items: flex-start; }
.checks input, .posted input { width: 18px; height: 18px; margin: 2px 0 0; accent-color: var(--app); flex: none; }
.checks input:checked + span { color: var(--faint); text-decoration: line-through; text-decoration-color: var(--line-strong); }

.post { background: var(--surface); border: 1px solid var(--line); padding: 22px 22px 24px; margin: 0 0 18px; scroll-margin-top: 16px; }
.post.is-posted { border-color: var(--app); box-shadow: inset 4px 0 0 var(--app); }
.post-head { display: flex; flex-wrap: wrap; gap: 10px 16px; align-items: flex-end; }
.post-day { font: 13px var(--mono); color: var(--app); }
.post-links { display: flex; gap: 14px; margin-left: auto; font-size: 14px; }
.post-links a { color: var(--muted); text-underline-offset: 3px; }
.post-links a:hover { color: var(--ink); }
.posted { display: inline-flex; gap: 8px; align-items: center; font-weight: 500; font-size: 14px; border: 1px solid var(--line-strong); padding: 7px 10px; cursor: pointer; }
.post-notes { color: var(--muted); max-width: 72ch; font-size: 15px; }
.post-notes p { margin: 12px 0 0; }
.lines { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
.line { display: flex; gap: 12px; align-items: center; background: var(--sunken); padding: 10px 10px 10px 14px; }
.line-text { flex: 1; font-weight: 500; overflow-wrap: anywhere; }
.line-meta { font: 12px var(--mono); color: var(--faint); white-space: nowrap; }
.body-head { display: flex; align-items: baseline; gap: 12px; margin-top: 20px; }
.body-head h4 { margin: 0; }
.body-head .copy { margin-left: auto; }
.body { white-space: pre-wrap; overflow-wrap: anywhere; font: 15px/1.6 var(--text); background: var(--sunken);
  margin: 8px 0 0; padding: 16px 18px; max-height: 420px; overflow: auto; max-width: 100%; }
.attach { display: flex; gap: 14px; align-items: center; margin-top: 16px; padding-top: 16px; border-top: 1px solid var(--line); }
.attach img { width: 96px; flex: none; border: 1px solid var(--line); }
.attach div { display: grid; gap: 2px; flex: 1; }
.attach span { color: var(--muted); font-size: 14px; }
.attach-none strong { color: var(--muted); font-weight: 500; }

footer { color: var(--faint); font-size: 14px; padding: 28px 0 48px; margin-top: 24px; border-top: 1px solid var(--line); }

@media (max-width: 760px) {
  .runway { grid-template-columns: 1fr 1fr; }
  .rw-day:nth-child(2n) { border-right: 0; }
  .rw-day:nth-child(-n+2) { border-bottom: 1px solid var(--line-strong); }
  .media-grid { grid-template-columns: 1fr; }
  .post { padding: 16px; }
  .post-links { margin-left: 0; width: 100%; order: 3; }
  .line { flex-wrap: wrap; }
  .line-text { flex-basis: 100%; }
  .line .copy { margin-left: auto; }
  .attach { flex-wrap: wrap; }
  .body-head { flex-wrap: wrap; }
  .brand { flex-wrap: wrap; align-items: flex-start; }
  .brand img { width: 44px; height: 44px; }
  .brand > div { flex: 1 1 200px; }
  .theme { order: 3; margin-left: 0; }
}
@media print { .copy, .btn, .theme { display: none; } .body { max-height: none; } }
</style>
</head>
<body>
<header class="top">
  <div class="wrap">
    <div class="brand">
      <img src="{{ICON}}" alt="" width="56" height="56">
      <div>
        <h1>JizzCraft launch kit</h1>
        <p>{{COUNT}} posts over {{DAYS}} days for 0.2.0. Copy the text, attach what it says, then stay in the thread.</p>
      </div>
      <button class="theme" type="button" id="theme">Theme: auto</button>
    </div>
    <ol class="runway" aria-label="Posting order">{{RUNWAY}}</ol>
    <p class="order-note">{{ORDER_NOTE}}</p>
    <p class="progress" id="progress" aria-live="polite"></p>
  </div>
</header>
<main class="wrap">
  <section id="media">
    <h2>Media</h2>
    <p class="lead">Synthetic and safe for work: every app frame is rendered by JizzCraft from generated test media. Downloads save the exact files from <code>docs/media</code> and <code>docs/screenshots</code>.</p>
    <div class="media-grid">{{MEDIA}}</div>
  </section>
  <section id="before">
    <h2>Before each post</h2>
    <p class="lead">From the top of posts.md. Ticks are kept in this browser only.</p>
    <ul class="checks">{{BEFORE}}</ul>
  </section>
  <section id="posts">
    <h2>Posts</h2>
    <p class="lead">In posting order. Reddit bodies copy as Markdown, one line per paragraph; the Hacker News comment copies as plain text.</p>
    {{POSTS}}
  </section>
  <section id="after">
    <h2>After posting</h2>
    <ul class="checks">{{AFTER}}</ul>
  </section>
  <footer>Generated from <code>docs/launch/posts.md</code> by <code>docs/launch/make-launch-kit.py</code>. Edit the Markdown, then rebuild. Repository: <a href="{{REPO}}">{{REPO}}</a></footer>
</main>
<script type="application/json" id="media-data">{{BLOBS}}</script>
<script>
(() => {
  const COPIES = {{COPIES}};
  try {
    const blobs = JSON.parse(document.getElementById('media-data').textContent);
    for (const [key, m] of Object.entries(blobs)) {
      const bin = atob(m.b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      const url = URL.createObjectURL(new Blob([bytes], { type: m.mime }));
      document.querySelectorAll('[data-media="' + key + '"]').forEach((el) => { el.src = url; });
      document.querySelectorAll('[data-media-dl="' + key + '"]').forEach((el) => { el.href = url; });
    }
  } catch (e) { console.error('media', e); }
  const store = {
    get(k) { try { return localStorage.getItem('jc-kit:' + k); } catch (e) { return null; } },
    set(k, v) { try { localStorage.setItem('jc-kit:' + k, v); } catch (e) { /* private mode */ } },
  };

  function legacyCopy(text) {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.cssText = 'position:fixed;top:0;left:0;opacity:0;';
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
    ta.remove();
    return ok;
  }
  async function copy(text) {
    if (navigator.clipboard && window.isSecureContext) {
      try { await navigator.clipboard.writeText(text); return true; } catch (e) { /* fall through */ }
    }
    return legacyCopy(text);
  }
  document.addEventListener('click', async (ev) => {
    const b = ev.target.closest('button.copy');
    if (!b) return;
    const ok = await copy(COPIES[b.dataset.copy] || '');
    const label = b.getAttribute('aria-label');
    b.classList.toggle('ok', ok);
    b.classList.toggle('fail', !ok);
    b.textContent = ok ? 'Copied' : 'Select it';
    if (!ok) {
      const pre = b.closest('.post') && b.closest('.body-head') ? b.closest('.post').querySelector('.body') : b.parentElement.querySelector('.line-text');
      if (pre) { const r = document.createRange(); r.selectNodeContents(pre); const s = getSelection(); s.removeAllRanges(); s.addRange(r); }
    }
    clearTimeout(b._t);
    b._t = setTimeout(() => { b.textContent = label; b.classList.remove('ok', 'fail'); }, 1600);
  });

  const boxes = [...document.querySelectorAll('input[data-store]')];
  function refresh() {
    let posted = 0, total = 0;
    document.querySelectorAll('.post').forEach((p) => {
      const on = p.querySelector('.posted input').checked;
      total += 1; posted += on ? 1 : 0;
      p.classList.toggle('is-posted', on);
      document.querySelectorAll('.rw-item[data-post="' + p.id + '"]').forEach((a) => a.classList.toggle('done', on));
    });
    document.getElementById('progress').textContent = posted + ' of ' + total + ' posted';
  }
  boxes.forEach((b) => {
    b.checked = store.get(b.dataset.store) === '1';
    b.addEventListener('change', () => { store.set(b.dataset.store, b.checked ? '1' : '0'); refresh(); });
  });
  refresh();

  const root = document.documentElement;
  const btn = document.getElementById('theme');
  const modes = ['auto', 'light', 'dark'];
  function apply(m) {
    if (m === 'auto') root.removeAttribute('data-theme'); else root.setAttribute('data-theme', m);
    btn.textContent = 'Theme: ' + m;
  }
  let mode = store.get('theme') || 'auto';
  apply(mode);
  btn.addEventListener('click', () => { mode = modes[(modes.indexOf(mode) + 1) % 3]; store.set('theme', mode); apply(mode); });
})();
</script>
</body>
</html>
"""

if __name__ == "__main__":
    build()
