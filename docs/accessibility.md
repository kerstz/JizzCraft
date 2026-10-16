# Accessibility

Honest status: the toolkit hooks are in place, but no screen reader session
has been run against JizzCraft by a person who relies on one. Treat support
as unverified.

## Supported

- **Screen reader plumbing.** egui's `accesskit` feature is enabled, so the
  widget tree is exposed through AT-SPI on Linux (UI Automation on Windows,
  NSAccessibility on macOS). On Linux this adds the pure-Rust `atspi`/`zbus`
  crates, which talk to the local session bus only; the offline build is still
  proven network-free by `offline_deps`.
- **Names for painted widgets.** Buttons, chips, nav tabs and the rating
  stars are drawn by hand; each reports a label and role to the accessibility
  tree (`WidgetInfo`).
- **Interface zoom 75 % to 200 %.** Ctrl+= / Ctrl+- / Ctrl+0 (egui built-ins)
  or Settings > General > Appearance. Persisted in `config.json` (`ui_zoom`).
  Not listed in the key table: it is egui's own shortcut, not a command.
- **Keyboard focus ring.** 2 px accent outline on focused custom widgets.
- **Contrast.** `crates/ui/tests/a11y.rs` asserts WCAG AA 4.5:1 for text
  tokens (TEXT, MUTED, FAINT, accent and danger inks) on BG, RAISED, SUNKEN,
  and 3:1 for strokes. FAINT was raised from 38 % to 50 % warm white to pass.
- **Reduce motion.** Settings > Appearance > Reduce motion sets egui's
  animation time to 0 (`reduce_motion` in config). It does not read the OS
  setting.
- **Funscript heatmap is decorative.** The 3 px band above the timeline (shown
  only when a `.funscript` sits next to the video) carries no information that
  is not also in text: the inspector shows "Script: N actions". It can be
  switched off in Settings, and it is hidden in discreet mode.

## Not supported / unknown

- No verified Orca, NVDA or VoiceOver session.
- Grid thumbnails and the player canvas are painted; their names and focus
  order are not audited. Tab order was not systematically reviewed.
- Most keyboard actions are single-key shortcuts, which can conflict with
  assistive technology.
- The OS reduced-motion preference and OS text scaling are not followed.
- Video content has no captions beyond what the media itself carries.
- The Appearance strings (interface zoom, reduce motion) are translated in French, Spanish, German, Italian and Portuguese; the audio language setting and any string added after them may still fall back to English.
