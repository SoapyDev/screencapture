# screencapture

Screenshot tool built on [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui).
Captures a region, one screen, or several screens stitched into one contiguous image, and saves a
PNG with a timestamped name (`screenshot_2026-11-06_14-05-09.042.png`; a `_1`, `_2`… suffix is
added if the name is already taken, so nothing is ever overwritten).

Every capture is also copied to the clipboard as `image/png`. The daemon offers it itself
(`zwlr_data_control_v1`, no helper process); a one-off run hands it to `wl-copy` (`xclip` on
X11), since its own offer would vanish when it exits. `SCREENCAPTURE_NO_CLIPBOARD=1` turns it off.

## Requirements

### Where it runs

| Desktop | Status |
|---|---|
| Linux, wlroots-style Wayland compositor (Hyprland, Sway, river, niri, Wayfire, labwc…) | Full support, default build: native screencopy, layer-shell overlay, native clipboard. Tested on Hyprland. |
| KDE Plasma (Wayland) | Build with `--features x11`: KWin has no screencopy, so capture goes through xcap and the desktop portal (slower). Overlay and clipboard should work. Untested. |
| GNOME (Wayland) | Build with `--features x11`, as above. No layer-shell, so the overlay is a fullscreen popup; the clipboard goes through `wl-copy`. Untested. |
| X11 (any distro) | Build with `--no-default-features --features x11,capture,editing,recording`. The overlay does not handle several monitors yet. |
| macOS, Windows | Code paths exist (gpui and xcap support both), but it has never been built or run there, and there is no clipboard copy. |

The overlay starts at the pointer's position only on Hyprland (read through its IPC); elsewhere
it starts without one.

### Running

- **glibc**: a binary needs the glibc it was built against or newer; one built on current
  Arch/CachyOS needs 2.44. Other distros should build from source. musl builds work, without the
  allocator tuning that keeps the daemon's memory flat (`src/memory.rs`).
- **libxkbcommon**: linked.
- **A GPU driver with Vulkan or OpenGL (EGL)**: loaded at run time by wgpu.
- **At least one font** installed: text boxes are drawn with the system fonts.
- **Optional**: `wl-clipboard` (`wl-copy`) on Wayland, or `xclip` on X11, for clipboard copy
  when no daemon is running. With the daemon on a wlroots compositor, none is needed.
- **For recording** (Linux): [gpu-screen-recorder](https://git.dec05eba.com/gpu-screen-recorder/about/)
  (GPU capture and encoding; recommended), or `wf-recorder` as a fallback (CPU encoding).
  `ffmpeg` for GIFs. Adaptive frame rate reads NVIDIA load through `nvidia-smi`.

### Building

- **Rust nightly**: picked up automatically from `rust-toolchain.toml` by rustup.
- **libxkbcommon development files** (`libxkbcommon-dev` on Debian/Ubuntu, `libxkbcommon-devel`
  on Fedora; included in the package on Arch).
- **X11 build only**: the X11/xcb libraries xcap needs (see its documentation).

```sh
cargo build --release   # target/release/screencapture
```

## Usage

```sh
screencapture                     # overlay: drag a region (or click a screen), adjust, draw, Enter
screencapture screen              # overlay: click screens to pick several, draw, Enter
screencapture screen DP-1 DP-2    # no UI: these screens, stitched
screencapture all                 # no UI: every screen, stitched
screencapture region -g "100,200 640x480"   # no UI: region in slurp format
screencapture list                # screen names and geometry
screencapture daemon              # stay resident so the overlay opens instantly
```

The saved path is printed on stdout. `-o DIR` picks the folder (default
`~/Pictures/Screenshots`); it is remembered in `~/.config/screencapture/config.json`. The
overlay's "Save in" button (`O`) changes it too.

The drawing palette defaults to six pastels (soft red, orange, yellow, green, blue, purple)
plus white and black.
Replace it in the same file; entries that don't parse are skipped, and if none do the pastels
are used:

```json
{ "palette": ["#FF8A80", "#8EE4A1", "#8AB8FFC0"] }
```

The screens are **frozen** the moment the overlay opens: you select (and draw) on that still
image, so moving content, menus or tooltips are captured exactly as they were at the trigger.
Nothing is saved until `Enter` (or the Capture button).

The bar at the bottom holds what to do with the selection: **● Record** (`V`; `Shift+V` records
every screen), **Capture** (`Enter`), the recording settings (frame rate, resolution, format,
quality, sound), which a click steps through and which are remembered, and the videos' folder
(`Shift+O`). When a recording starts,
the bars slide away and the overlay closes; drawings stay on screen and are recorded, so they can
be prepared beforehand. See [Recording](#recording).

Selecting:
- Region mode (`R`): drag a rectangle, or click a screen to select all of it. Drag edges or
  corners to resize, drag inside to move, arrows nudge, `Shift`+arrows resize. A click outside
  clears it.
- Screens mode (`S`): click screens to toggle them. `A` picks every screen.
- `O` changes the screenshots' folder, `Shift+O` the videos'; `Esc` or right-click cancels.

Drawing works for every kind of capture (region, one screen, several, all). Shapes are drawn
over the frozen desktop and burnt into the saved PNG at its full resolution; with stitched
screens, each screen's part of a drawing lands where that screen lands.
- Tools: `1` rectangle, `2` ellipse, `3` line, `4` arrow, `5` freehand, `6` numbered step
  marker (a filled disc with a white number, counting up), `7` text, `8` redact. Same key or
  button again puts the tool down; switching Region / Screens / All screens puts it down too.
- Text: drag a box (or click for a default one) and type; `Enter` starts a new line, `Esc`
  finishes, double-click a box to type in it again. The wheel or stepper sets the font size.
  "A text" / "▇ fill" choose whether the palette colours the letters or the background, and
  "Fill on/off" toggles the background. Text wraps to the box; drag its corners to reshape.
- Redact: drag over what must not be seen. Every pixel the box touches is replaced by opaque
  black in the saved file — not blurred or blended, so nothing can be recovered from it.
- Click a shape to select it: drag it to move, drag its handles to resize it (or change a
  line's length and direction), `Delete` removes it. Clicking a shape selects it even with a tool
  in hand.
- Width: the `− N px +` stepper, `[` / `]`, or the mouse wheel; it applies to the selected shape,
  or to the next one.
- Colour: palette swatches (`C` cycles), `＋` / `P` opens a colour picker; the picked colour is
  remembered as an extra swatch. Colours apply to the selected shape too.
- `Ctrl+Z` undoes the last change: drawing, moving, resizing, recolouring, deleting. A burst of
  width changes counts as one change.
- `Esc` closes the picker, then deselects, then cancels.

When several screens are stitched they keep their layout, but empty bands between them (e.g. an
unpicked screen in the middle) are removed. Screens with different scales are rendered at the
highest density.

## Recording

From the overlay: select, optionally draw, then `V` (or the Record button); `Shift+V` records
every screen. From the command line or a key binding:

```sh
screencapture record                          # the focused screen, until stopped
screencapture record screen DP-1              # a named screen
screencapture record region -g "100,200 640x480"
screencapture record all                      # every screen, joined into one video
screencapture record stop                     # finish; prints the file
screencapture record discard                  # stop and throw the recording away
screencapture record toggle                   # start the focused screen, or stop: one key for both
```

Videos go to `~/Videos/Screencapture` (`screencast_<timestamp>.mp4`); `record … -o DIR`, or the
overlay's "Videos in" button (`Shift+O`), changes and remembers that folder, separately from the
screenshots' folder. With the daemon running it
records; without, the `record` command itself records until `record stop` or Ctrl+C. While a
recording runs, Print (the overlay) does nothing but say so.

Settings, remembered under `"record"` in the config file and overridable per recording:

| Option | Values (default first) |
|---|---|
| `--fps` | `native` (the screen's refresh rate), `adaptive`, `120`, `60`, `30`, `15` |
| `--resolution` | `native`, `1440p`, `1080p`, `720p`, `480p` (largest height; scaled on the GPU while recording) |
| `--format` | `av1`, `h264`, `hevc` (all MP4), `gif` |
| `--quality` | `very-high`, `ultra`, `high`, `medium` |
| `--audio` | `off`, `output` (what the computer plays), `input` (microphone), `both` |

- Fixed rates never exceed the screen's refresh rate. `adaptive` reads CPU and GPU load once at
  the start (idle: the screen's rate; busy: 60, 30 or 15) and only takes frames when the screen
  changes.
- H.264 plays everywhere but stops at 4096 px across; wider recordings switch to AV1, and past
  8192 px (AV1/HEVC's limit) they are scaled down to fit. You are told when either happens.
- GIFs are recorded at most 25 fps and 1280 px wide, then converted with one palette at stop.
- Several screens (or a region across them) are recorded one screen per recorder, all started
  together, and joined at stop into one video laid out like the desktop, without the gaps
  between screens (like stitched screenshots). Each screen is recorded one quality preset
  finer, since joining encodes once more. Joining runs on the GPU (NVENC) when it can, else in
  software; it takes time: about 180 frames a second for two 1440p screens on an RTX 5080, so
  roughly 45 s to save a minute recorded at 144 fps.
- While recording, push the pointer to the top edge of any screen, at its middle: after a brief
  rest a drawer slides down with the time recorded, what is recorded, **■ Stop** and
  **✕ Discard**. It closes once the pointer leaves it. It takes no keyboard (your apps keep it;
  key bindings use `record stop` / `record discard`), and recording never pauses: on a recorded
  screen, the drawer is in the video while it is open.
- A red frame around the recorded area shows that a recording runs. It sits just outside the
  area, so it is never in the video; where the area touches a screen edge with no screen beyond
  it, a 3 px band is left out of the recording to make room for it. The frame takes no clicks
  and no keyboard: you work normally underneath. (Wayland with layer-shell only; on X11 there is
  no frame.)

Hyprland binds, for example:

```lua
hl.bind("SUPER + Print", hl.dsp.exec_cmd("/home/you/.local/bin/screencapture record toggle"))
```

## Latency

The time from trigger to on-screen overlay is what this tool optimises for.

- Every screen is copied (wlr-screencopy) the moment the overlay is requested. The daemon does
  it on the thread that receives the request, before anything else, over a connection it keeps
  open with already-used shared-memory buffers: fresh buffers cost the compositor ~10 ms just to
  fault their pages in. Standalone, the copy overlaps gpui start-up.
- Pixels stay in BGRA, the order the compositor hands out and gpui displays: the common
  `XRGB8888` format is copied (in parallel bands) with only the alpha set, the result goes to the
  GPU as is, and saving cuts from that same memory. Only the final cut is converted to RGBA.
- Drawing code does nothing until used; the colour palette is parsed when the toolbar first
  shows, the colour picker is only built while it is open, and system fonts are loaded on a
  background thread the first time the Text tool is picked. Text is laid out and rasterised
  with cosmic-text (gpui's own Linux text stack), the same way for the preview and the file.
- gpui's first window in a process costs ~200 ms (GPU device and pipeline setup); later ones
  ~15 ms. **Run the daemon** and every `screencapture` / `screencapture screen` call hands its
  request over a Unix socket instead of starting gpui, with a fallback to standalone if there is no daemon.
- No-UI commands (`all`, `screen NAMES`, `region -g`) never start gpui.
- The daemon's socket lives in `$XDG_RUNTIME_DIR` (or a `0700` folder in `/tmp`). Both the daemon
  and its clients refuse a socket folder that isn't yours or is open to others.

Measured on Hyprland, 3× 2560×1440 (`SCREENCAPTURE_TRACE=1` prints these milestones):

| Path | Trigger → overlay on screen |
|---|---|
| standalone `screencapture` | ~250 ms |
| via `screencapture daemon` | ~37 ms |

Daemon breakdown: the screens are frozen ~10 ms after the request (≈6 ms for the compositor to
copy three screens, ≈5 ms to convert them), then ~15 ms to open the first window and ~11 ms to
upload its 2560×1440 image and show it. Other screens follow one by one. The drawing toolbar
row is added one frame later: building it in the first frame cost a whole extra frame (~16 ms).

The daemon keeps its screencopy buffers between captures: about 15 MB per 2560×1440 screen
(44 MB for three), shared with the compositor rather than duplicated. After 5 minutes without
a capture it gives them back; the next capture then takes ~15 ms longer (52 ms instead of 37 ms
to the overlay). `SCREENCAPTURE_RELEASE_AFTER_SECS` changes the delay. Its request thread sleeps
in `poll()` and its Wayland connection is idle between captures.

| No-UI capture, whole process incl. PNG encode | |
|---|---|
| `all` (7680×1440) | ~110 ms (mostly PNG encoding) |
| `region -g` (400×200) | ~13 ms |

### Hyprland setup

```ini
exec-once = screencapture daemon
bind = , Print, exec, screencapture
bind = SHIFT, Print, exec, screencapture screen
bind = CTRL, Print, exec, screencapture all
# Optional: skip the layer fade-in so the overlay is instant visually too.
layerrule = noanim, screencapture
```

## Backends and features

| Feature | Default | Enables |
|---|---|---|
| `wayland` | yes | gpui Wayland backend (layer-shell overlay) + native `wlr-screencopy` capture |
| `x11` | no | gpui X11 backend + [`xcap`](https://crates.io/crates/xcap) capture |
| `capture` | yes | Screenshots: Enter in the overlay, `region -g`, `screen NAMES`, `all` |
| `editing` | yes | Drawing over the screens before a screenshot or a recording |
| `recording` | yes | Screen recording: V in the overlay, `record …` |

Any of `capture`, `editing` and `recording` can be left out: for example
`cargo build --release --no-default-features --features wayland,capture` makes a screenshot tool
with no drawing and no recording. The command line then offers only what was built in. Code for a
disabled feature is not compiled (`cargo tree` of the default build has no X11 crates).

The code is a cargo workspace, one crate per part:

| Crate | Holds |
|---|---|
| `crates/core` | The screens and their capture, the selection, the config, the daemon, and the overlay's shell, which the features plug into |
| `crates/capture` | Composing and saving screenshots |
| `crates/editing` | The drawing tools and how drawings are painted |
| `crates/recording` | Recording: the plan, the recorders, joining screens, the frame and controls |

The program itself (`src/`) is the command line and the list of features it was built with.
With both enabled, `SCREENCAPTURE_BACKEND=xcap` forces xcap. Windows and macOS always use xcap.

Native screencopy exists because xcap 0.9's wlroots path fails on Hyprland (`wl_shm` invalid
pool size). It works on wlroots-style compositors (Hyprland, Sway, river…). GNOME and KDE Wayland
don't offer screencopy; they need the `x11` feature (xcap goes through their portals).

## Development

Nightly toolchain (`rust-toolchain.toml`) with Polonius (`-Zpolonius=next`), applied to the
workspace's crates only through `profile-rustflags`. Developed test-first.

```sh
cargo test --workspace                      # unit tests
cargo test --workspace -- --include-ignored # + live tests against the running compositor; the
                                            #   e2e tests show the overlay, record, and drive the
                                            #   pointer (one at a time: they share the desktop)
cargo clippy --workspace --all-targets      # pedantic + nursery; also with
                                            #   --no-default-features --features x11,capture,editing,recording
                                            #   and with each of capture, editing, recording left out
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. Use it,
change it, ship it.

The code was written largely by an AI assistant (Claude, through Claude Code) under human
direction and review. It is offered openly for that reason: take what is useful.

Unless you state otherwise, any contribution you submit for inclusion is dual-licensed as above,
without any additional terms or conditions.
