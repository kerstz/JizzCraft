#!/usr/bin/env bash
# Rebuilds docs/media/demo.gif and docs/media/social-preview.png.
#
# Every frame is rendered by the app itself (`jizzcraft --screenshot`) from the
# synthetic fixtures (`cargo xtask fixtures`), each with a throw-away library.
# No real media is involved. Needs: a release build, ImageMagick 7 (`magick`),
# ffmpeg, and a Wayland or X11 display.
#
# On a tiling compositor the window gets the tile's size instead of 1280x800;
# run a nested one and point the script at it, for example:
#   kwin_wayland --virtual --width 1600 --height 1000 --socket wayland-demo &
#   DEMO_WAYLAND=wayland-demo docs/media/make-demo.sh
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
RAW=$T/raw CAP=$T/cap SEQ=$T/seq
mkdir -p "$RAW" "$CAP" "$SEQ" "$T/db"

shot() { # name, app args...
  local name=$1; shift
  local env=()
  [ -n "${DEMO_WAYLAND:-}" ] && env=(env WAYLAND_DISPLAY="$DEMO_WAYLAND" DISPLAY=)
  "${env[@]}" "$BIN" --db "$T/db/$name.db" --lang en "$@" \
    --screenshot "$RAW/$name.png" --assume-adult >/dev/null
}

ALL=(--import fixtures)
IMG=(--import fixtures/images)
SEL=(--cmd 'view.select.move={"dx":1,"dy":0}') # the first move selects item 0
LOOP=(--cmd 'markers.add={"pos_ms":20000,"label":"scene"}' --cmd 'player.loop={"a_ms":8000,"b_ms":16000}')
RATE=(--cmd 'view.select.move={"dx":2,"dy":1}')

shot a0 "${ALL[@]}"
shot a1 "${ALL[@]}" "${SEL[@]}"
shot a2 "${ALL[@]}" "${SEL[@]}" --cmd 'view.select.move={"dx":1,"dy":0}'
shot a3 "${ALL[@]}" "${SEL[@]}" --cmd 'view.select.move={"dx":2,"dy":0}'
shot a4 "${ALL[@]}" "${SEL[@]}" --cmd 'view.select.move={"dx":7,"dy":0}'
shot b0 "${ALL[@]}" --cmd 'view.apply={"spec":{"name":"circles"},"label":"circles"}'
shot b1 "${ALL[@]}" --cmd 'view.apply={"spec":{"name":"pattern"},"label":"pattern"}'
shot b2 "${ALL[@]}" --cmd 'view.filter={"tab":"videos"}'
shot c0 "${IMG[@]}" "${RATE[@]}"
shot c1 "${IMG[@]}" "${RATE[@]}" --cmd 'media.rate={"rating":4}'
shot c2 "${IMG[@]}" "${RATE[@]}" --cmd 'media.rate={"rating":5}' --cmd media.favorite.toggle
shot d0 "${IMG[@]}" --tab images --open 17
shot d1 "${IMG[@]}" --tab images --open 18
for p in 3 6 9 12 15; do
  shot "e$p" --import fixtures/videos --tab videos --open 1 --cmd "player.seek={\"pos\":$p}" "${LOOP[@]}"
done
shot f0 "${IMG[@]}" --cmd triage.start
shot f1 "${IMG[@]}" --cmd triage.start --cmd triage.keep
shot f2 "${IMG[@]}" --cmd triage.start --cmd triage.keep --cmd triage.fav
shot f3 "${IMG[@]}" --cmd triage.start --cmd triage.keep --cmd triage.fav --cmd triage.hide
shot g0 "${IMG[@]}" --cmd app.discreet.toggle

# Captions in the house look: JetBrains Mono, APP peach tag, warm white text.
F=$R/assets/fonts
W=900 BAND=44 n=0 k=0
cap() { # raw, tag, text, seconds
  n=$((n + 1))
  local out
  out=$(printf "%s/%03d.png" "$CAP" $n)
  magick "$RAW/$1.png" -resize ${W}x -background '#121316' -gravity north -splice 0x$BAND \
    -font "$F/JetBrainsMono-Bold.ttf" -pointsize 15 -fill '#ff8a6b' -gravity northwest -annotate +18+14 "$2" \
    -font "$F/JetBrainsMono-Regular.ttf" -fill '#f2f1ee' -annotate +$((32 + 11 * ${#2}))+14 "$3" \
    -fill '#3a3836' -draw "rectangle 0,$((BAND - 1)) $W,$((BAND - 1))" -alpha off "PNG24:$out"
  frames "$out" "$4"
}
frames() { # file, seconds -> symlinks at 10 fps
  local i count
  count=$(awk -v d="$2" 'BEGIN { printf "%d", d * 10 + 0.5 }')
  for ((i = 0; i < count; i++)); do ln -s "$1" "$(printf "%s/%04d.png" "$SEQ" $k)"; k=$((k + 1)); done
}

cap a0 "LIBRARY" "your folders, indexed in place. Never moved, never modified." 1.4
for i in 1 2 3; do cap a$i "LIBRARY" "arrow keys walk a virtualized grid" 0.5; done
cap a4 "LIBRARY" "the inspector follows the selection" 1.4
cap b0 "FILTER" "by name..." 1.3
cap b1 "FILTER" "by name..." 0.9
cap b2 "FILTER" "...or by tab: library, images, videos, favorites" 1.4
cap c0 "RATE" "Alt+1..5 for stars, S for favorite" 0.8
cap c1 "RATE" "Alt+1..5 for stars, S for favorite" 0.8
cap c2 "RATE" "Alt+1..5 for stars, S for favorite" 1.4
cap d0 "VIEWER" "fit, 100 %%, zoom around the cursor, left/right to step" 1.0
cap d1 "VIEWER" "fit, 100 %%, zoom around the cursor, left/right to step" 1.0
for p in 3 6 9 12 15; do cap e$p "PLAYER" "GStreamer playback, markers, A-B loop, scrubbing" 0.5; done
cap f0 "TRIAGE" "V: review new imports. Right keep, down hide, up favorite" 1.2
cap f1 "TRIAGE" "V: review new imports. Right keep, down hide, up favorite" 0.8
cap f2 "TRIAGE" "V: review new imports. Right keep, down hide, up favorite" 0.8
cap f3 "TRIAGE" "nothing is ever deleted; left undoes" 1.3
cap g0 "BOSS KEY" "Ctrl+Shift+H: pause, mute, discreet mode, minimize" 2.4

END=$CAP/end.png
magick -size ${W}x607 xc:'#121316' \
  \( assets/icons/jizzcraft-512.png -resize 120x120 \) -gravity center -geometry +0-110 -composite \
  -font "$F/Archivo-Medium.ttf" -pointsize 54 -fill white -annotate +0+10 "JizzCraft" \
  -font "$F/JetBrainsMono-Regular.ttf" -pointsize 17 -fill '#f2f1ee' -annotate +0+70 "Your media. Your disk. No network in the default build." \
  -font "$F/JetBrainsMono-Bold.ttf" -pointsize 15 -fill '#ff8a6b' -annotate +0+110 "RUST  ·  EGUI  ·  GSTREAMER  ·  SQLITE  ·  LINUX" \
  -font "$F/JetBrainsMono-Regular.ttf" -pointsize 13 -fill '#999999' -annotate +0+150 "Every frame of this GIF was rendered by the app from synthetic test media." \
  -alpha off "PNG24:$END"
frames "$END" 2.6

mkdir -p docs/media
ffmpeg -v error -y -framerate 10 -i "$SEQ/%04d.png" \
  -vf "split[s0][s1];[s0]palettegen=max_colors=256:stats_mode=full[p];[s1][p]paletteuse=dither=sierra2_4a:diff_mode=rectangle" \
  -loop 0 docs/media/demo.gif

magick -size 1280x640 xc:'#121316' \
  \( "$RAW/a4.png" -resize 820x \( +clone -fill none -stroke '#5a5652' -strokewidth 2 -draw 'rectangle 0,0 819,511' \) -composite \) \
  -gravity northwest -geometry +560+96 -composite \
  \( assets/icons/jizzcraft-512.png -resize 96x96 \) -geometry +64+96 -composite -stroke none -strokewidth 0 \
  -font "$F/Archivo-Medium.ttf" -pointsize 76 -fill white -annotate +60+220 'JizzCraft' \
  -font "$F/Inter-Medium.ttf" -pointsize 28 -fill '#f2f1ee' -annotate +64+330 'A private, offline media library' \
  -annotate +64+366 'and player for your own collection.' \
  -font "$F/JetBrainsMono-Bold.ttf" -pointsize 18 -fill '#ff8a6b' -annotate +64+440 'NO ACCOUNT · NO TELEMETRY' \
  -annotate +64+470 'NO NETWORK IN THE DEFAULT BUILD' \
  -font "$F/JetBrainsMono-Regular.ttf" -pointsize 17 -fill '#999999' -annotate +64+560 'Rust · egui · GStreamer · SQLite · Linux' \
  -annotate +64+590 'github.com/kerstz/JizzCraft · 18+' \
  -alpha off -strip PNG24:docs/media/social-preview.png

ls -l docs/media/demo.gif docs/media/social-preview.png
