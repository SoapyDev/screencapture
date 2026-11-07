//! Native capture for wlroots-style compositors (Hyprland, Sway, river, ...) via
//! `zwlr_screencopy_manager_v1`, with logical geometry from `zxdg_output_manager_v1`.
//!
//! All wanted outputs are requested at once and copied into shared memory in a single event
//! loop, then converted to RGBA in parallel.

use std::os::fd::{AsFd, OwnedFd};

use anyhow::{Context, Result, bail};
use memmap2::{Mmap, MmapOptions};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    globals::{GlobalList, GlobalListContents, registry_queue_init},
    protocol::{wl_buffer::WlBuffer, wl_output, wl_registry, wl_shm, wl_shm_pool::WlShmPool},
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1, zxdg_output_v1,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

use super::{
    MonitorInfo, Rect, Shot,
    pixels::{self, PixelFormat, Transform},
};

#[derive(Default)]
struct Output {
    name: Option<String>,
    position: (i32, i32),
    mode_size: (i32, i32),
    scale: i32,
    transform: Transform,
    logical_position: Option<(i32, i32)>,
    logical_size: Option<(i32, i32)>,
}

impl Output {
    fn info(&self, index: usize) -> MonitorInfo {
        let (x, y) = self.logical_position.unwrap_or(self.position);
        let (w, h) = self.logical_size.unwrap_or_else(|| {
            let (w, h) = self.mode_size;
            let (w, h) = match self.transform {
                Transform::Rotate90 | Transform::Rotate270 | Transform::Flipped90 | Transform::Flipped270 => {
                    (h, w)
                }
                _ => (w, h),
            };
            let scale = self.scale.max(1);
            (w / scale, h / scale)
        });
        MonitorInfo {
            name: self.name.clone().unwrap_or_else(|| format!("output-{index}")),
            rect: Rect::new(x, y, w, h),
            output_position: self.position,
        }
    }
}

#[derive(PartialEq, Eq)]
struct BufferSpec {
    format: PixelFormat,
    wl_format: wl_shm::Format,
    width: u32,
    height: u32,
    stride: u32,
}

#[derive(Default)]
enum FrameState {
    #[default]
    Negotiating,
    Copying,
    Ready,
    Failed,
}

#[derive(Default)]
struct Frame {
    output: usize,
    offers: Vec<BufferSpec>,
    y_invert: bool,
    state: FrameState,
    memory: Option<Memory>,
    error: Option<anyhow::Error>,
    /// Until the copy is over: destroyed if it never ends, so late events are dropped.
    proxy: Option<ZwlrScreencopyFrameV1>,
}

/// A shared-memory buffer the compositor copies a screen into.
type Memory = (BufferSpec, Mmap, WlBuffer);

/// A converted frame, and its buffer to keep (with its output) for the next capture.
type Converted = (Result<Shot>, Option<(usize, Memory)>);

struct State {
    outputs: Vec<Output>,
    frames: Vec<Frame>,
    shm: Option<wl_shm::WlShm>,
    screencopy_version: u32,
    /// Buffers kept from the last capture, by output.
    pool: Vec<Option<Memory>>,
    reused: usize,
}

fn connect() -> Result<(Connection, GlobalList, EventQueue<State>, State)> {
    let conn = Connection::connect_to_env().context("connecting to the Wayland display")?;
    let (globals, queue) = registry_queue_init::<State>(&conn).context("reading Wayland globals")?;
    Ok((
        conn,
        globals,
        queue,
        State {
            outputs: Vec::new(),
            frames: Vec::new(),
            shm: None,
            screencopy_version: 0,
            pool: Vec::new(),
            reused: 0,
        },
    ))
}

/// Bind every output and fetch names and geometry.
fn discover(
    globals: &GlobalList,
    queue: &mut EventQueue<State>,
    state: &mut State,
) -> Result<Vec<wl_output::WlOutput>> {
    let qh = queue.handle();
    let output_globals: Vec<(u32, u32)> = globals.contents().with_list(|list| {
        list.iter()
            .filter(|g| g.interface == "wl_output")
            .map(|g| (g.name, g.version))
            .collect()
    });
    let outputs: Vec<_> = output_globals
        .iter()
        .enumerate()
        .map(|(i, &(name, version))| {
            state.outputs.push(Output {
                scale: 1,
                ..Output::default()
            });
            globals
                .registry()
                .bind::<wl_output::WlOutput, _, _>(name, version.min(4), &qh, i)
        })
        .collect();
    if let Ok(xdg) = globals.bind::<ZxdgOutputManagerV1, _, _>(&qh, 1..=3, ()) {
        for (i, output) in outputs.iter().enumerate() {
            xdg.get_xdg_output(output, &qh, i);
        }
    }
    queue.roundtrip(state).context("Wayland roundtrip")?;
    if outputs.is_empty() {
        bail!("compositor advertises no outputs");
    }
    Ok(outputs)
}

pub fn monitors() -> Result<Vec<MonitorInfo>> {
    let (_conn, globals, mut queue, mut state) = connect()?;
    discover(&globals, &mut queue, &mut state)?;
    Ok(state.outputs.iter().enumerate().map(|(i, o)| o.info(i)).collect())
}

/// A screencopy connection kept open between captures, with its shared-memory buffers. Fresh
/// buffers cost the compositor ~3 ms per 2560×1440 screen just to fault in the pages; reused
/// ones don't.
pub struct Capturer {
    _conn: Connection,
    globals: GlobalList,
    queue: EventQueue<State>,
    state: State,
    manager: ZwlrScreencopyManagerV1,
    outputs: Vec<wl_output::WlOutput>,
    /// Registry names of the outputs bound above, to notice hotplugs.
    output_names: Vec<u32>,
    /// Buffers reused so far (for tests).
    pub reused: usize,
}

/// Monitors were plugged in or out: start a new [`Capturer`].
#[derive(Debug)]
pub struct OutputsChanged;

impl std::fmt::Display for OutputsChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("outputs changed")
    }
}

impl std::error::Error for OutputsChanged {}

fn output_names(globals: &GlobalList) -> Vec<u32> {
    globals.contents().with_list(|list| {
        list.iter()
            .filter(|g| g.interface == "wl_output")
            .map(|g| g.name)
            .collect()
    })
}

impl Capturer {
    pub fn new() -> Result<Self> {
        let (conn, globals, mut queue, mut state) = connect()?;
        let qh = queue.handle();
        let manager = globals
            .bind::<ZwlrScreencopyManagerV1, _, _>(&qh, 1..=3, ())
            .context("compositor lacks zwlr_screencopy_manager_v1")?;
        state.screencopy_version = manager.version();
        state.shm = Some(
            globals
                .bind::<wl_shm::WlShm, _, _>(&qh, 1..=1, ())
                .context("compositor lacks wl_shm")?,
        );
        let outputs = discover(&globals, &mut queue, &mut state)?;
        state.pool = outputs.iter().map(|_| None).collect();
        let output_names = output_names(&globals);
        crate::trace("outputs discovered");
        Ok(Self {
            _conn: conn,
            globals,
            queue,
            state,
            manager,
            outputs,
            output_names,
            reused: 0,
        })
    }

    /// Free the kept buffers (the next capture makes fresh ones, a few ms slower), keeping
    /// the connection.
    pub fn drop_buffers(&mut self) {
        for (_, _, buffer) in self.state.pool.iter_mut().filter_map(Option::take) {
            buffer.destroy();
        }
        let _ = self.queue.flush();
    }

    /// Bytes held in kept buffers.
    #[cfg(test)]
    pub fn pooled_bytes(&self) -> usize {
        self.state
            .pool
            .iter()
            .flatten()
            .map(|(_, map, _)| map.len())
            .sum()
    }

    /// Copy the `wanted` monitors. Fails if outputs were added or removed since [`Self::new`]:
    /// make a new capturer then.
    pub fn capture(&mut self, wanted: &dyn Fn(&MonitorInfo) -> bool) -> Result<Vec<Shot>> {
        let qh = self.queue.handle();
        // Take in whatever changed meanwhile (modes, positions, hotplugs) without a round trip.
        if let Some(guard) = self.queue.prepare_read() {
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .context("Wayland dispatch")?;
        if output_names(&self.globals) != self.output_names {
            return Err(OutputsChanged.into());
        }

        let infos: Vec<MonitorInfo> = self
            .state
            .outputs
            .iter()
            .enumerate()
            .map(|(i, o)| o.info(i))
            .collect();
        self.state.frames.clear();
        for (i, info) in infos.iter().enumerate() {
            if wanted(info) {
                let index = self.state.frames.len();
                let proxy = self.manager.capture_output(0, &self.outputs[i], &qh, index);
                self.state.frames.push(Frame {
                    output: i,
                    proxy: Some(proxy),
                    ..Frame::default()
                });
            }
        }
        wait_for_frames(&mut self.queue, &mut self.state)?;
        crate::trace("frames copied");
        let frames = std::mem::take(&mut self.state.frames);

        let transforms: Vec<Transform> = self.state.outputs.iter().map(|o| o.transform).collect();
        let converted: Vec<Converted> = std::thread::scope(|scope| {
            // Collected so every conversion starts before the first join.
            #[allow(clippy::needless_collect)]
            let handles: Vec<_> = frames
                .into_iter()
                .map(|frame| {
                    let info = infos[frame.output].clone();
                    let transform = transforms[frame.output];
                    scope.spawn(move || {
                        if let Some(e) = frame.error {
                            return (Err(e.context(format!("capturing {}", info.name))), None);
                        }
                        let Some(memory) = frame.memory else {
                            return (
                                Err(anyhow::anyhow!(
                                    "capturing {}: frame finished without a buffer",
                                    info.name
                                )),
                                None,
                            );
                        };
                        let (spec, map, _) = &memory;
                        let image = pixels::to_bgra(
                            spec.format,
                            map,
                            spec.width,
                            spec.height,
                            spec.stride,
                            frame.y_invert,
                        );
                        let shot = Shot {
                            name: info.name,
                            rect: info.rect,
                            output_position: info.output_position,
                            image: pixels::apply_transform(image, transform),
                        };
                        (Ok(shot), Some((frame.output, memory)))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("conversion thread panicked"))
                .collect()
        });
        let mut shots = Vec::with_capacity(converted.len());
        for (shot, memory) in converted {
            if let Some((output, memory)) = memory {
                self.state.pool[output] = Some(memory);
            }
            shots.push(shot);
        }
        self.reused = self.state.reused;
        usable(shots)
    }
}

/// The screens that were copied. One that is off (DPMS) or refused must not cost the capture
/// of the others; only when none was copied is that an error.
fn usable(results: Vec<Result<Shot>>) -> Result<Vec<Shot>> {
    let (shots, errors): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);
    let errors: Vec<String> = errors
        .into_iter()
        .filter_map(Result::err)
        .map(|e| format!("{e:#}"))
        .collect();
    if shots.is_empty() {
        bail!("no screen could be captured: {}", errors.join("; "));
    }
    for e in &errors {
        eprintln!("screencapture: skipped a screen: {e}");
    }
    Ok(shots.into_iter().filter_map(Result::ok).collect())
}

/// One-off capture on a fresh connection.
pub fn capture(wanted: &dyn Fn(&MonitorInfo) -> bool) -> Result<Vec<Shot>> {
    Capturer::new()?.capture(wanted)
}

/// Longest wait for the compositor to copy the screens. It normally takes a frame or two; it
/// may never answer for an output that is off, or for a buffer format we could not use.
const COPY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Once one screen is copied, how long the others may still take. Copies of every screen are
/// asked for at once and land within a refresh or two (~6 ms here); one still missing well
/// after that is off, and waiting the full timeout for it would delay every capture.
const STRAGGLER_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

fn give_up_at(started: std::time::Instant, first_done: Option<std::time::Instant>) -> std::time::Instant {
    let limit = started + COPY_TIMEOUT;
    first_done.map_or(limit, |t| limit.min(t + STRAGGLER_GRACE))
}

/// Dispatch until every frame is done or failed, or until [`give_up_at`]; frames still pending
/// then are failed (see [`usable`]).
fn wait_for_frames(queue: &mut EventQueue<State>, state: &mut State) -> Result<()> {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let started = std::time::Instant::now();
    let mut first_done = None;
    loop {
        queue.dispatch_pending(state).context("Wayland dispatch")?;
        let pending = |f: &Frame| matches!(f.state, FrameState::Negotiating | FrameState::Copying);
        if !state.frames.iter().any(pending) {
            return Ok(());
        }
        if first_done.is_none() && state.frames.iter().any(|f| matches!(f.state, FrameState::Ready)) {
            first_done = Some(std::time::Instant::now());
        }
        let left = give_up_at(started, first_done).saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            let waited = started.elapsed().as_secs_f32();
            for f in state.frames.iter_mut().filter(|f| pending(f)) {
                // Named by the caller's "capturing <name>" context.
                f.error = Some(anyhow::anyhow!("not copied within {waited:.2} s (output off?)"));
                f.state = FrameState::Failed;
                if let Some(proxy) = f.proxy.take() {
                    proxy.destroy();
                }
                // The compositor may still write into it: never reuse it.
                if let Some((_, _, buffer)) = f.memory.take() {
                    buffer.destroy();
                }
            }
            return Ok(());
        }
        queue.flush().context("Wayland flush")?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let fd = guard.connection_fd();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let wait = Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos().into(),
        };
        match rustix::event::poll(&mut fds, Some(&wait)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => {}
            Err(e) => return Err(e).context("waiting for the compositor"),
        }
        match guard.read() {
            Err(wayland_client::backend::WaylandError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock => {}
            result => {
                result.context("reading from the compositor")?;
            }
        }
    }
}

/// Allocate shared memory for the best offered format and ask the compositor to copy into it.
fn start_copy(state: &mut State, index: usize, frame: &ZwlrScreencopyFrameV1, qh: &QueueHandle<State>) {
    let result = (|| -> Result<Memory> {
        let f = &mut state.frames[index];
        let best = (0..f.offers.len())
            .min_by_key(|&i| f.offers[i].format.preference())
            .context("compositor offered no supported shm format")?;
        let spec = f.offers.swap_remove(best);
        // The last capture's buffer, if it still fits: its pages are already faulted in.
        match state.pool.get_mut(f.output).and_then(Option::take) {
            Some(memory) if memory.0 == spec => {
                state.reused += 1;
                return Ok(memory);
            }
            Some((_, _, unfit)) => unfit.destroy(),
            None => {}
        }
        let size = spec.stride as usize * spec.height as usize;
        let fd: OwnedFd = rustix::fs::memfd_create(c"screencapture", rustix::fs::MemfdFlags::CLOEXEC)?;
        rustix::fs::ftruncate(&fd, size as u64)?;
        // SAFETY: the memfd is private to us and the compositor, which only writes into it
        // before sending `ready`; we read it after that.
        let map = unsafe { MmapOptions::new().len(size).map(&fd)? };
        let shm = state.shm.as_ref().context("wl_shm not bound")?;
        let pool: WlShmPool = shm.create_pool(fd.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            spec.width as i32,
            spec.height as i32,
            spec.stride as i32,
            spec.wl_format,
            qh,
            (),
        );
        pool.destroy();
        Ok((spec, map, buffer))
    })();
    let f = &mut state.frames[index];
    match result {
        Ok((spec, map, buffer)) => {
            frame.copy(&buffer);
            f.memory = Some((spec, map, buffer));
            f.state = FrameState::Copying;
        }
        Err(e) => {
            f.error = Some(e);
            f.state = FrameState::Failed;
            frame.destroy();
        }
    }
}

const fn pixel_format(format: wl_shm::Format) -> Option<PixelFormat> {
    use wl_shm::Format as F;
    Some(match format {
        F::Xrgb8888 => PixelFormat::Xrgb8888,
        F::Argb8888 => PixelFormat::Argb8888,
        F::Xbgr8888 => PixelFormat::Xbgr8888,
        F::Abgr8888 => PixelFormat::Abgr8888,
        F::Xrgb2101010 => PixelFormat::Xrgb2101010,
        F::Argb2101010 => PixelFormat::Argb2101010,
        F::Xbgr2101010 => PixelFormat::Xbgr2101010,
        F::Abgr2101010 => PixelFormat::Abgr2101010,
        _ => return None,
    })
}

const fn transform(t: wl_output::Transform) -> Transform {
    use wl_output::Transform as T;
    match t {
        T::_90 => Transform::Rotate90,
        T::_180 => Transform::Rotate180,
        T::_270 => Transform::Rotate270,
        T::Flipped => Transform::Flipped,
        T::Flipped90 => Transform::Flipped90,
        T::Flipped180 => Transform::Flipped180,
        T::Flipped270 => Transform::Flipped270,
        _ => Transform::Normal,
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, usize> for State {
    fn event(
        state: &mut Self,
        frame: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        &index: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event as E;
        match event {
            E::Buffer {
                format: WEnum::Value(wl_format),
                width,
                height,
                stride,
            } => {
                if let Some(format) = pixel_format(wl_format) {
                    state.frames[index].offers.push(BufferSpec {
                        format,
                        wl_format,
                        width,
                        height,
                        stride,
                    });
                }
                // Before v3 there is no buffer_done: the single buffer event is all we get.
                if state.screencopy_version < 3 {
                    start_copy(state, index, frame, qh);
                }
            }
            E::BufferDone => start_copy(state, index, frame, qh),
            E::Flags {
                flags: WEnum::Value(flags),
            } => {
                state.frames[index].y_invert = flags.contains(zwlr_screencopy_frame_v1::Flags::YInvert);
            }
            E::Ready { .. } => {
                state.frames[index].state = FrameState::Ready;
                frame.destroy();
            }
            E::Failed => {
                let f = &mut state.frames[index];
                f.error = Some(anyhow::anyhow!("compositor refused the copy"));
                f.state = FrameState::Failed;
                frame.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        &index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let output = &mut state.outputs[index];
        match event {
            wl_output::Event::Geometry {
                x, y, transform: t, ..
            } => {
                output.position = (x, y);
                if let WEnum::Value(t) = t {
                    output.transform = transform(t);
                }
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => {
                output.mode_size = (width, height);
            }
            wl_output::Event::Scale { factor } => output.scale = factor,
            wl_output::Event::Name { name } => output.name = Some(name),
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        &index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let output = &mut state.outputs[index];
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => output.logical_position = Some((x, y)),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                output.logical_size = Some((width, height));
            }
            zxdg_output_v1::Event::Name { name } if output.name.is_none() => output.name = Some(name),
            _ => {}
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(State: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(State: WlShmPool);
wayland_client::delegate_noop!(State: ignore WlBuffer);
wayland_client::delegate_noop!(State: ZxdgOutputManagerV1);
wayland_client::delegate_noop!(State: ZwlrScreencopyManagerV1);

/// These talk to the running compositor: `cargo test -- --ignored` inside a wlroots session.
#[cfg(test)]
mod tests {
    use super::*;

    fn shot(name: &str) -> Shot {
        Shot {
            name: name.into(),
            rect: crate::capture::Rect::new(0, 0, 1, 1),
            output_position: (0, 0),
            image: image::RgbaImage::new(1, 1),
        }
    }

    #[test]
    fn a_screen_that_is_off_does_not_spoil_the_others() {
        let results = vec![
            Ok(shot("DP-1")),
            Err(anyhow::anyhow!("capturing DP-2: no copy")),
            Ok(shot("DP-3")),
        ];
        let names: Vec<String> = usable(results).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["DP-1", "DP-3"]);
    }

    #[test]
    fn nothing_copied_is_an_error_naming_every_screen() {
        let results = vec![
            Err(anyhow::anyhow!("capturing DP-1: off")),
            Err(anyhow::anyhow!("capturing DP-2: off")),
        ];
        let Err(e) = usable(results) else {
            panic!("nothing was copied")
        };
        let message = format!("{e:#}");
        assert!(message.contains("DP-1") && message.contains("DP-2"), "{message}");
    }

    #[test]
    fn stragglers_get_a_short_grace_once_any_screen_is_copied() {
        let start = std::time::Instant::now();
        assert_eq!(give_up_at(start, None), start + COPY_TIMEOUT);
        let first = start + std::time::Duration::from_millis(6);
        assert_eq!(give_up_at(start, Some(first)), first + STRAGGLER_GRACE);
        // Never later than the overall limit.
        let late = start + COPY_TIMEOUT;
        assert_eq!(give_up_at(start, Some(late)), start + COPY_TIMEOUT);
    }

    #[test]
    #[ignore = "needs a wlroots-based Wayland session"]
    fn lists_named_monitors_with_logical_geometry() {
        let monitors = monitors().unwrap();
        assert_ne!(monitors.len(), 0);
        for m in &monitors {
            assert_ne!(m.name, "");
            assert!(!m.rect.is_empty(), "{m:?}");
        }
    }

    #[test]
    #[ignore = "needs a wlroots-based Wayland session"]
    fn captures_only_wanted_monitors_at_full_size() {
        let first = monitors().unwrap().remove(0);
        let shots = capture(&|m| m.name == first.name).unwrap();
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].rect, first.rect);
        assert!(shots[0].image.width() >= first.rect.w as u32);
        // A real screen is not one flat transparent colour.
        assert!(shots[0].image.pixels().any(|p| p[3] == 255));
    }

    #[test]
    #[ignore = "needs a wlroots-based Wayland session"]
    fn captures_all_monitors() {
        let count = monitors().unwrap().len();
        assert_eq!(capture(&|_| true).unwrap().len(), count);
    }

    #[test]
    #[ignore = "needs a wlroots-based Wayland session"]
    fn a_capturer_reuses_its_buffers() {
        let count = monitors().unwrap().len();
        let mut capturer = Capturer::new().unwrap();
        assert_eq!(capturer.capture(&|_| true).unwrap().len(), count);
        assert_eq!(capturer.reused, 0);
        let shots = capturer.capture(&|_| true).unwrap();
        assert_eq!(shots.len(), count);
        assert_eq!(capturer.reused, count, "second capture reuses every buffer");
        assert!(shots[0].image.pixels().any(|p| p[3] == 255));
    }

    #[test]
    #[ignore = "needs a wlroots-based Wayland session"]
    fn dropped_buffers_are_freed_and_not_reused() {
        let mut capturer = Capturer::new().unwrap();
        capturer.capture(&|_| true).unwrap();
        assert!(capturer.pooled_bytes() > 0);
        capturer.drop_buffers();
        assert_eq!(capturer.pooled_bytes(), 0);
        assert!(
            !capturer.capture(&|_| true).unwrap().is_empty(),
            "still captures afterwards"
        );
        assert_eq!(capturer.reused, 0, "with fresh buffers");
    }
}
