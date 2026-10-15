#!/usr/bin/env bash
# Rebuilds docs/media/demo.gif and docs/media/social-preview.png.
#
# Every frame is rendered by the app itself (`jizzcraft --screenshot`) from the
# synthetic fixtures (`cargo xtask fixtures`): one run per frame, driven by the
# same named commands as the menus and shortcuts (`--cmd`). No real media is
# involved. Captions, the progress rail and the end card are drawn around the
# frames with ImageMagick; nothing inside the app window is retouched.
#
# Needs: a release build, ImageMagick 7 (`magick`), ffmpeg, and a Wayland or
# X11 display. The window must get its requested 1280x800, so on a tiling
# compositor run a nested one and point the script at it:
#   kwin_wayland --virtual --no-lockscreen --width 1600 --height 1000 --socket wayland-demo &
#   DEMO_WAYLAND=wayland-demo docs/media/make-demo.sh
#
# DEMO_KEEP=DIR keeps the raw and captioned frames there for review;
# DEMO_RAW=DIR/raw then redraws captions from those frames without the app.
#
# Usage: docs/media/make-demo.sh   (from the repository root)
set -euo pipefail

R=$(cd "$(dirname "$0")/../.." && pwd)
cd "$R"
[ -d fixtures/images ] || { echo "run \`cargo xtask fixtures\` first" >&2; exit 1; }
for t in magick ffmpeg; do command -v $t >/dev/null || { echo "missing: $t" >&2; exit 1; }; done
cargo build --release -p jizzcraft
BIN=$R/target/release/jizzcraft

T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
RAW=$T/raw CAP=$T/cap SEQ=$T/seq LIB=$T/Media
mkdir -p "$RAW" "$CAP" "$SEQ" "$T/db" "$LIB/videos"

# A throw-away library: the fixture images and the short artwork clips (the
# bench clips are plain test patterns and would only fill the grid).
cp -r fixtures/images "$LIB/"
cp fixtures/videos/clip-*.webm "$LIB/videos/"

# --home keeps thumbnails out of your cache; screenshot runs never read or
# write your config (they use the defaults, inspector open).
run() {
  local env=()
  [ -n "${DEMO_WAYLAND:-}" ] && env=(env WAYLAND_DISPLAY="$DEMO_WAYLAND" DISPLAY=)
  "${env[@]}" "$BIN" --home "$T/home" --lang en --assume-adult "$@" >/dev/null 2>&1
}

shoot_all() {
  # Import once (that frame carries the import toast and is not used), then
  # every shot starts from a copy of the imported library.
  run --db "$T/base.db" --import "$LIB" --screenshot "$T/import.png"
  shot() { # name, app args...
    local name=$1 e
    shift
    for e in "" -wal -shm; do
      [ -f "$T/base.db$e" ] && cp "$T/base.db$e" "$T/db/$name.db$e"
    done
    run --db "$T/db/$name.db" "$@" --screenshot "$RAW/$name.png"
    [ -s "$RAW/$name.png" ] || { echo "shot $name failed" >&2; exit 1; }
  }
  sel() { printf -- '--cmd\nview.select.move={"dx":%s,"dy":%s}\n' "$1" "$2"; }
  mapfile -t M0 < <(sel 1 0) # the first move selects item 0
  mapfile -t MR < <(sel 1 0)
  mapfile -t MD < <(sel 0 1)

  # Library sorted by name: aurora 0-3, bokeh 4-7, clips 8-10, dunes 11-14, ...
  shot l0
  shot l1 "${M0[@]}"
  shot l2 "${M0[@]}" "${MR[@]}"
  shot l3 "${M0[@]}" "${MR[@]}" "${MR[@]}"
  shot l4 "${M0[@]}" "${MR[@]}" "${MR[@]}" "${MD[@]}"
  shot f0 --cmd 'view.apply={"spec":{"name":"sunset"},"label":"Name: sunset"}'
  shot f1 --cmd 'view.filter={"tab":"videos"}'
  RATE=("${M0[@]}" --cmd 'view.select.move={"dx":3,"dy":2}') # dunes-01
  shot r0 "${RATE[@]}"
  shot r1 "${RATE[@]}" --cmd 'media.rate={"rating":4}'
  shot r2 "${RATE[@]}" --cmd 'media.rate={"rating":5}' --cmd media.favorite.toggle
  # Images tab: ... sunset-02 is 23, sunset-03 (portrait) is 24.
  shot v0 --tab images --open 23
  shot v1 --tab images --open 24
  PLAY=(--tab videos --open 2
    --cmd 'markers.add={"pos_ms":2500,"label":"glow"}'
    --cmd 'markers.add={"pos_ms":7600,"label":"curtain"}'
    --cmd 'player.loop={"a_ms":2000,"b_ms":8500}')
  # Without the side panels: the player bar needs the width.
  PLAY+=(--cmd sidebar.toggle --cmd inspector.toggle)
  for p in 2.5 3.5 4.5 5.5 6.5; do shot "p$p" "${PLAY[@]}" --cmd "player.seek={\"pos\":$p}"; done
  shot p9 "${PLAY[@]}" --cmd 'player.seek={"pos":7}' --cmd 'player.preview={"pos":7.6}'
  shot t0 --cmd triage.start
  shot t1 --cmd triage.start --cmd triage.keep
  shot t2 --cmd triage.start --cmd triage.keep --cmd triage.fav
  shot t3 --cmd triage.start --cmd triage.keep --cmd triage.fav --cmd triage.hide
  shot d0 "${M0[@]}" --cmd app.discreet.toggle
  # The social preview's shot: no sidebar, so the grid gets the width.
  shot hero "${M0[@]}" "${MR[@]}" --cmd 'media.rate={"rating":5}' --cmd media.favorite.toggle \
    --cmd sidebar.toggle

}
if [ -n "${DEMO_RAW:-}" ]; then
  cp "$DEMO_RAW"/*.png "$RAW/" # captions only, from a DEMO_KEEP run
else
  shoot_all
fi

# ---- captions --------------------------------------------------------------
# House look: app background, Archivo title, Inter caption, the real shortcut
# as JetBrains Mono key caps on the right, and a progress rail with one
# segment per chapter.
F=$R/assets/fonts
W=960 H=600 BAND=64
BG='#121316' TEXT='#f2f1ee' MUTED='#b9b5ae' APP='#ff8a6b'
CHAPTERS=7
n=0 k=0
frames() { # file, seconds -> symlinks at 10 fps
  local i count
  count=$(awk -v d="$2" 'BEGIN { printf "%d", d * 10 + 0.5 }')
  for ((i = 0; i < count; i++)); do ln -s "$1" "$(printf "%s/%04d.png" "$SEQ" $k)"; k=$((k + 1)); done
}
rail() { # chapter (1-based) -> draw args for the segments
  local i x0 x1 col seg=$(((W - 40 - (CHAPTERS - 1) * 6) / CHAPTERS))
  for ((i = 1; i <= CHAPTERS; i++)); do
    x0=$((20 + (i - 1) * (seg + 6)))
    x1=$((x0 + seg))
    if ((i < $1)); then col='#5a5652'; elif ((i == $1)); then col=$APP; else col='#2a2a2e'; fi
    printf -- '-fill\n%s\n-draw\nrectangle %d,%d %d,%d\n' "$col" $x0 $((BAND - 4)) $x1 $((BAND - 2))
  done
}
keycaps() { # "k1 k2 ..." -> one image of key caps on stdout (MIFF); "_" is a space
  local kk args=()
  for kk in $1; do
    args+=(\( -background '#202126' -fill "$TEXT" -font "$F/JetBrainsMono-Regular.ttf" -pointsize 15
      "label:${kk//_/ }" -bordercolor '#202126' -border 9x5 -bordercolor '#4a4743' -border 1
      -bordercolor "$BG" -border 4x0 \))
  done
  magick "${args[@]}" -background "$BG" +append +repage miff:-
}
cap() { # raw, chapter, title, text, keys ("" for none), seconds
  n=$((n + 1))
  local out r=() kc=()
  out=$(printf "%s/%03d.png" "$CAP" $n)
  mapfile -t r < <(rail "$2")
  if [ -n "$5" ]; then
    keycaps "$5" >"$T/keys.miff"
    kc=("$T/keys.miff" -gravity east -geometry +16-2 -composite)
  fi
  magick \( -size ${W}x$BAND xc:"$BG" +size \
    \( -background "$BG" -fill white -font "$F/Archivo-Medium.ttf" -pointsize 24 "label:$3" \) \
    -gravity west -geometry +20-3 -composite \
    \( -background "$BG" -fill "$MUTED" -font "$F/Inter-Regular.ttf" -pointsize 17 "label:${4//%/%%}" \) \
    -gravity west -geometry +150-2 -composite \
    "${kc[@]}" -gravity northwest "${r[@]}" \) \
    \( "$RAW/$1.png" -filter Lanczos -resize ${W}x$H \) -append +repage -alpha off "PNG24:$out"
  frames "$out" "$6"
}

cap l0 1 "Library" "Point it at your folders. Files are never moved or modified." "" 2.0
cap l1 1 "Library" "Keyboard first. The inspector follows the selection." "← ↑ → ↓" 0.5
cap l2 1 "Library" "Keyboard first. The inspector follows the selection." "← ↑ → ↓" 0.4
cap l3 1 "Library" "Keyboard first. The inspector follows the selection." "← ↑ → ↓" 0.4
cap l4 1 "Library" "Keyboard first. The inspector follows the selection." "← ↑ → ↓" 0.9
cap f0 2 "Filter" "By name, type, rating, size, duration or folder." "" 1.6
cap f1 2 "Filter" "Or by tab: library, images, videos, favorites." "1 2 3 4" 1.4
cap r0 3 "Rate" "Stars and favorites live in the app's own database." "Alt+1…5 S" 0.6
cap r1 3 "Rate" "Stars and favorites live in the app's own database." "Alt+1…5 S" 0.6
cap r2 3 "Rate" "Stars and favorites live in the app's own database." "Alt+1…5 S" 1.1
cap v0 4 "Viewer" "Fit, 100 %, zoom around the cursor, step with the arrows." "← →" 1.1
cap v1 4 "Viewer" "Fit, 100 %, zoom around the cursor, step with the arrows." "← →" 1.1
for p in 2.5 3.5 4.5 5.5 6.5; do
  cap "p$p" 5 "Player" "GStreamer playback, markers and an A-B loop." "K L" 0.35
done
cap p9 5 "Player" "Hover the timeline for a preview frame." "K L" 1.1
cap t0 6 "Triage" "Review new imports one by one. Nothing is ever deleted." "→_keep ↑_fav ↓_hide" 1.0
cap t1 6 "Triage" "Review new imports one by one. Nothing is ever deleted." "→_keep ↑_fav ↓_hide" 0.6
cap t2 6 "Triage" "Review new imports one by one. Nothing is ever deleted." "→_keep ↑_fav ↓_hide" 0.6
cap t3 6 "Triage" "Hidden is a flag in the database, and ← undoes it." "← undo" 1.0
cap d0 7 "Discreet" "Names hidden, thumbnails blurred." "" 1.3
cap d0 7 "Boss key" "Pause, mute, discreet mode, minimize. One chord." "Ctrl+Shift+H" 1.4

END=$CAP/end.png
magick -size ${W}x$((H + BAND)) xc:"$BG" \
  \( assets/icons/jizzcraft-512.png -resize 112x112 \) -gravity center -geometry +0-150 -composite \
  -font "$F/Archivo-Medium.ttf" -pointsize 64 -fill white -annotate +0-40 "JizzCraft" \
  -font "$F/Inter-Regular.ttf" -pointsize 22 -fill "$TEXT" -annotate +0+22 "A private, offline library and player for your own media." \
  -font "$F/Inter-Regular.ttf" -pointsize 17 -fill "$MUTED" -annotate +0+56 "No account. No telemetry. No network code in the default build." \
  -font "$F/JetBrainsMono-Bold.ttf" -pointsize 22 -fill "$APP" -annotate +0+120 "github.com/kerstz/JizzCraft" \
  -font "$F/Inter-Regular.ttf" -pointsize 14 -fill '#8a8680' -annotate +0+224 "Rendered by the app from synthetic test media. Rust, egui, GStreamer, SQLite. 18+." \
  -alpha off "PNG24:$END"
frames "$END" 2.8
# Fade the card out to the background, so the loop restarts on a clean cut.
for a in 40 75; do
  magick "$END" -fill "$BG" -colorize $a -alpha off "PNG24:$CAP/fade$a.png"
  frames "$CAP/fade$a.png" 0.1
done

mkdir -p docs/media
ffmpeg -v error -y -framerate 10 -i "$SEQ/%04d.png" \
  -vf "split[s0][s1];[s0]palettegen=max_colors=256:stats_mode=diff[p];[s1][p]paletteuse=dither=sierra2_4a:diff_mode=rectangle" \
  -loop 0 docs/media/demo.gif

# ---- social preview (1280x640) ----------------------------------------------
# Name, one sentence and the repository on the left; the whole window on the
# right, large enough to read as "a grid of pictures" at 400 px wide.
SW=736 SH=460
magick -size 1280x640 xc:"$BG" \
  -fill "$APP" -draw "rectangle 0,0 1279,7" \
  \( "$RAW/hero.png" -filter Lanczos -resize ${SW}x$SH -bordercolor '#4a4743' -border 1 \) \
  -gravity northwest -geometry +$((1280 - SW - 2 - 48))+$(((640 - SH) / 2 + 4)) -composite \
  \( assets/icons/jizzcraft-512.png -resize 80x80 \) -geometry +56+70 -composite \
  -font "$F/Archivo-Medium.ttf" -pointsize 80 -fill white -annotate +50+166 'JizzCraft' \
  -font "$F/Inter-Medium.ttf" -pointsize 32 -fill "$TEXT" -annotate +56+284 'A private, offline library' \
  -annotate +56+326 'and player for your own' -annotate +56+368 'media collection.' \
  -font "$F/Inter-Regular.ttf" -pointsize 22 -fill "$MUTED" -annotate +56+440 'No account. No telemetry.' \
  -annotate +56+472 'No network code in the default build.' \
  -font "$F/JetBrainsMono-Bold.ttf" -pointsize 20 -fill "$APP" -annotate +56+542 'github.com/kerstz/JizzCraft' \
  -alpha off -strip -define png:compression-level=9 PNG24:docs/media/social-preview.png

if [ -n "${DEMO_KEEP:-}" ]; then
  mkdir -p "$DEMO_KEEP"
  cp -r "$RAW" "$CAP" "$DEMO_KEEP/"
fi
# Lossless squeeze when available (also worth running on docs/screenshots).
command -v optipng >/dev/null && optipng -quiet -o2 -strip all docs/media/social-preview.png
ls -l docs/media/demo.gif docs/media/social-preview.png
