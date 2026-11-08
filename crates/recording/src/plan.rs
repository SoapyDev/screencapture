//! Turning "record this, like so" into exactly what the recorder is asked for. No I/O here.

use std::path::Path;

use anyhow::{Result, bail};

use super::settings::{Audio, Format, Fps, Quality, Settings};
use screencapture_core::{
    capture::{MonitorInfo, Rect},
    units::{saturating_i32, saturating_u32, unsigned},
};

/// Width of the red frame drawn around a recording, in logical px. It always sits outside the
/// recorded area, so it is never in the video.
pub const BORDER: i32 = 3;

/// What to record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The focused screen (or the first one, if that isn't known).
    Focused,
    All,
    Screens(Vec<String>),
    Region(Rect),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Av1,
    H264,
    Hevc,
}

impl Codec {
    /// Largest picture side hardware encoders take (NVENC; AMD and Intel are the same or
    /// larger for these codecs).
    #[must_use]
    pub const fn max_side(self) -> u32 {
        match self {
            Self::H264 => 4096,
            Self::Av1 | Self::Hevc => 8192,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Av1 => "AV1",
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
        }
    }
}

/// GIFs: these caps keep the conversion and the file reasonable.
pub const GIF_FPS: u32 = 25;
pub const GIF_WIDTH: u32 = 1280;

/// Everything the recorder is asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// Logical desktop pixels, border already left out.
    pub area: Rect,
    pub fps: u32,
    /// Frames only when the screen changes, at most `fps`.
    pub variable: bool,
    /// Picture size, when scaled down from the area's.
    pub size: Option<(u32, u32)>,
    pub codec: Codec,
    pub quality: Quality,
    pub audio: Audio,
    /// Convert to GIF at stop.
    pub gif: bool,
}

/// What the recorder runs: one job per screen, and how to join them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub parts: Vec<PartJob>,
    /// How the parts become one video; `None` for one part, recorded straight into the file.
    pub join: Option<Join>,
    /// Adjustments the user should hear about.
    pub notes: Vec<String>,
}

impl Plan {
    /// What is recorded, in a few words, for the controls: `MP4 · AV1 · 60 fps · 3 screens`.
    #[must_use]
    pub fn summary(&self) -> String {
        let first = &self.parts[0].job;
        let (codec, fps, gif) = self.join.map_or((first.codec, first.fps, first.gif), |join| {
            (join.codec, join.fps, join.gif)
        });
        let format = if gif {
            "GIF".to_string()
        } else {
            format!("MP4 · {}", codec.label())
        };
        let screens = match self.parts.len() {
            1 => String::new(),
            count => format!(" · {count} screens"),
        };
        format!("{format} · {fps} fps{screens}")
    }
}

/// One screen's recording, and where it lands in the joined video (video px).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartJob {
    pub job: Job,
    pub slot: Rect,
}

/// The joined video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Join {
    pub size: (u32, u32),
    pub codec: Codec,
    pub quality: Quality,
    pub fps: u32,
    /// The first part's sound goes into the joined video.
    pub audio: bool,
    /// Convert the joined video to GIF.
    pub gif: bool,
}

/// The desktop area `target` covers, on `monitors`.
///
/// # Errors
///
/// When a named screen doesn't exist, the region is on no screen, or there are no screens.
pub fn area(target: &Target, monitors: &[MonitorInfo], focused: Option<&str>) -> Result<Rect> {
    let found = match target {
        Target::Focused => focused_screen(monitors, focused).map(|m| m.rect),
        Target::All => monitors.iter().map(|m| m.rect).reduce(Rect::union),
        Target::Screens(names) => named_screens(names, monitors)?,
        Target::Region(region) => Some(region_on_screens(*region, monitors)?),
    };
    found.ok_or_else(|| anyhow::anyhow!("no screens found"))
}

/// The focused screen, else the first.
fn focused_screen<'a>(monitors: &'a [MonitorInfo], focused: Option<&str>) -> Option<&'a MonitorInfo> {
    let named = focused.and_then(|name| monitors.iter().find(|m| m.name == name));
    named.or_else(|| monitors.first())
}

/// The screens called `names`, together; an unknown name is an error.
fn named_screens(names: &[String], monitors: &[MonitorInfo]) -> Result<Option<Rect>> {
    let rect_of = |name: &String| {
        let screen = monitors.iter().find(|m| &m.name == name);
        screen
            .map(|m| m.rect)
            .ok_or_else(|| anyhow::anyhow!("no screen named {name:?}"))
    };
    let rects: Vec<Rect> = names.iter().map(rect_of).collect::<Result<_>>()?;
    Ok(rects.into_iter().reduce(Rect::union))
}

/// The part of `region` on the screens.
fn region_on_screens(region: Rect, monitors: &[MonitorInfo]) -> Result<Rect> {
    let pieces = monitors.iter().filter_map(|m| m.rect.intersect(region));
    pieces
        .reduce(Rect::union)
        .ok_or_else(|| anyhow::anyhow!("the region is on no screen"))
}

/// Smallest side worth recording, in logical px.
const MIN_SIDE: i32 = 16;

/// `area` shrunk where the border would not fit on any screen, and to even sizes (encoders
/// need them). (Recordings go through [`super::split`]; this is the one-piece case.)
///
/// # Errors
///
/// When what is left is too small to record.
#[cfg(test)]
pub fn inset(area: Rect, monitors: &[MonitorInfo]) -> Result<Rect> {
    let screens: Vec<Rect> = monitors.iter().map(|m| m.rect).collect();
    let room = room_for_border(area, &screens);
    // Encoders work on 2×2 blocks of colour.
    let even = Rect::new(room.x, room.y, room.w - room.w % 2, room.h - room.h % 2);
    ensure_recordable(even)
}

/// `area` shrunk where the strip just outside an edge, which the border needs, is on none of
/// the recorded `screens`: the frame never lands on a screen that is not recorded.
#[must_use]
pub fn room_for_border(area: Rect, screens: &[Rect]) -> Rect {
    let on_screen = |(x, y): (i32, i32)| screens.iter().any(|screen| screen.contains(x, y));
    // How far an edge must move in so that both ends of the strip outside it are on a screen.
    let room = |strip: &dyn Fn(i32) -> [(i32, i32); 2]| {
        (0..=BORDER)
            .find(|&k| strip(k).into_iter().all(on_screen))
            .unwrap_or(BORDER)
    };
    let (mut l, mut t, mut r, mut b) = (area.x, area.y, area.right(), area.bottom());
    l += room(&|k| [(l + k - BORDER, t), (l + k - BORDER, b - 1)]);
    r -= room(&|k| [(r - k + BORDER - 1, t), (r - k + BORDER - 1, b - 1)]);
    t += room(&|k| [(l, t + k - BORDER), (r - 1, t + k - BORDER)]);
    b -= room(&|k| [(l, b - k + BORDER - 1), (r - 1, b - k + BORDER - 1)]);
    Rect::new(l, t, r - l, b - t)
}

/// `rect`, if big enough to be worth recording.
///
/// # Errors
///
/// When a side is shorter than the smallest recordable.
pub fn ensure_recordable(rect: Rect) -> Result<Rect> {
    if !is_recordable(rect) {
        bail!("the area is too small to record");
    }
    Ok(rect)
}

/// Whether both sides of `rect` reach the smallest recordable.
#[must_use]
pub const fn is_recordable(rect: Rect) -> bool {
    rect.w >= MIN_SIDE && rect.h >= MIN_SIDE
}

/// When the refresh rate isn't known.
const FALLBACK_FPS: u32 = 60;

/// Frame rate, and whether it is variable. `native` is the screens' refresh rate, `load` the
/// busier of CPU and GPU (0..=1), only sampled for [`Fps::Adaptive`].
pub fn frame_rate(fps: Fps, native: Option<u32>, load: Option<f32>) -> (u32, bool) {
    let screen = native.unwrap_or(FALLBACK_FPS);
    match fps {
        Fps::Native => (screen, false),
        Fps::Adaptive => (load.map_or(FALLBACK_FPS, adaptive_tier).min(screen), true),
        fixed => (fixed.fixed().unwrap_or(FALLBACK_FPS).min(screen), false),
    }
}

/// Adaptive: lighter the busier the machine is.
#[must_use]
pub fn adaptive_tier(load: f32) -> u32 {
    match load {
        l if l < 0.5 => u32::MAX,
        l if l < 0.75 => 60,
        l if l < 0.9 => 30,
        _ => 15,
    }
}

/// Nearest even size, at least 2.
fn even(length: f64) -> u32 {
    (saturating_u32((length / 2.).round()) * 2).max(2)
}

/// `(w, h)` scaled by `factor`, to even sizes.
fn scaled((w, h): (u32, u32), factor: f64) -> (u32, u32) {
    (even(f64::from(w) * factor), even(f64::from(h) * factor))
}

/// One recorder's job per part; the first records the sound.
fn part_jobs(
    parts: &[super::split::Part],
    video: &Video,
    settings: Settings,
    rate: (u32, bool),
) -> Vec<PartJob> {
    let several = parts.len() > 1;
    let role = |index| PartRole {
        first: index == 0,
        several,
    };
    let jobs = parts.iter().enumerate();
    jobs.map(|(index, part)| video.part_job(part, role(index), settings, rate))
        .collect()
}

/// The plan for recording `target`.
///
/// # Errors
///
/// When `target` covers no screen, or what it covers is too small to record.
pub fn plan(
    target: &Target,
    settings: Settings,
    monitors: &[MonitorInfo],
    focused: Option<&str>,
    load: Option<f32>,
) -> Result<Plan> {
    let area = area(target, monitors, focused)?;
    let screens = screens_of(target, area, monitors, focused);
    let (parts, joined) = super::split::split(area, &screens)?;
    let video = Video::fitted(joined, density_of(&screens), settings);
    let rate = frame_rate(settings.fps, refresh_rate(&screens), load);
    let several = parts.len() > 1;
    let jobs = part_jobs(&parts, &video, settings, rate);
    let join = several.then(|| video.join(settings, rate.0));
    Ok(Plan {
        parts: jobs,
        join,
        notes: video.notes,
    })
}

/// The screens `target` records from.
fn screens_of<'a>(
    target: &Target,
    area: Rect,
    monitors: &'a [MonitorInfo],
    focused: Option<&str>,
) -> Vec<&'a MonitorInfo> {
    match target {
        Target::Focused => focused_screen(monitors, focused).into_iter().collect(),
        Target::All => monitors.iter().collect(),
        Target::Screens(names) => monitors.iter().filter(|m| names.contains(&m.name)).collect(),
        Target::Region(_) => monitors
            .iter()
            .filter(|m| m.rect.intersect(area).is_some())
            .collect(),
    }
}

/// The densest of `screens`, in physical px per logical px: the video keeps every pixel of it.
fn density_of(screens: &[&MonitorInfo]) -> f64 {
    let densities = screens.iter().map(|screen| screen.density());
    densities.reduce(f64::max).unwrap_or(1.)
}

/// The fastest refresh rate among `screens`, in Hz.
fn refresh_rate(screens: &[&MonitorInfo]) -> Option<u32> {
    screens
        .iter()
        .filter_map(|m| m.refresh_mhz)
        .max()
        .map(|mhz| (mhz + 500) / 1000)
}

/// Which part of a recording a job is for.
#[derive(Clone, Copy)]
struct PartRole {
    first: bool,
    several: bool,
}

/// The finished video's size and codec, from the desktop size it shows.
struct Video {
    /// The joined desktop size, logical px.
    joined: (i32, i32),
    /// Physical px per logical px on the densest screen recorded.
    density: f64,
    /// Video px per logical px.
    scale: f64,
    codec: Codec,
    gif: bool,
    notes: Vec<String>,
}

impl Video {
    /// The video for `joined` logical px, recorded at `density`: the encoders' limits and the
    /// settings' caps apply to its pixels, not to the logical size.
    fn fitted(joined: (i32, i32), density: f64, settings: Settings) -> Self {
        let pixels = |logical: i32| unsigned(at_factor(logical, density));
        let native = (pixels(joined.0), pixels(joined.1));
        let wanted = capped_size(native, settings);
        let (codec, size, notes) = within_codec_limits(wanted, first_codec(settings.format));
        let scale = if size == native {
            density
        } else {
            f64::from(size.0) / f64::from(unsigned(joined.0))
        };
        let gif = settings.format == Format::Gif;
        Self {
            joined,
            density,
            scale,
            codec,
            gif,
            notes,
        }
    }

    /// A desktop length in video px.
    fn length(&self, logical: i32) -> i32 {
        at_factor(logical, self.scale)
    }

    /// Whether `area`, recorded as is, already has the size of `slot`.
    fn fills(&self, area: Rect, slot: Rect) -> bool {
        let pixels = |logical: i32| at_factor(logical, self.density);
        (pixels(area.w), pixels(area.h)) == (slot.w, slot.h)
    }

    /// Where a part lands in the joined video.
    fn slot(&self, part: &super::split::Part) -> Rect {
        let (x, y) = (self.length(part.slot.0), self.length(part.slot.1));
        let right = self.length(part.slot.0 + part.area.w);
        let bottom = self.length(part.slot.1 + part.area.h);
        Rect::new(x, y, right - x, bottom - y)
    }

    fn part_job(
        &self,
        part: &super::split::Part,
        role: PartRole,
        settings: Settings,
        rate: (u32, bool),
    ) -> PartJob {
        let slot = self.slot(part);
        let native = self.fills(part.area, slot);
        let job = Job {
            area: part.area,
            fps: if self.gif { rate.0.min(GIF_FPS) } else { rate.0 },
            variable: rate.1,
            size: (!native).then_some((unsigned(slot.w), unsigned(slot.h))),
            codec: self.codec,
            // Joining encodes again: record finer so the result keeps the chosen quality.
            quality: if role.several {
                settings.quality.finer()
            } else {
                settings.quality
            },
            audio: if role.first { settings.audio } else { Audio::Off },
            gif: self.gif && !role.several,
        };
        PartJob { job, slot }
    }

    fn join(&self, settings: Settings, fps: u32) -> Join {
        Join {
            size: (
                unsigned(self.length(self.joined.0)),
                unsigned(self.length(self.joined.1)),
            ),
            codec: self.codec,
            quality: settings.quality,
            fps: if self.gif { fps.min(GIF_FPS) } else { fps },
            audio: settings.audio != Audio::Off,
            gif: self.gif,
        }
    }
}

/// `logical` at `factor` px per logical px, as an even length, rounded the same way for every
/// edge so that neighbouring parts meet exactly; untouched at 1. (Not [`even`]: positions can
/// be 0.)
fn at_factor(logical: i32, factor: f64) -> i32 {
    if (factor - 1.).abs() < f64::EPSILON {
        return logical;
    }
    saturating_i32((f64::from(logical) * factor / 2.).round() * 2.)
}

/// `native` after the resolution cap, and for GIFs their width cap.
fn capped_size(native: (u32, u32), settings: Settings) -> (u32, u32) {
    let mut size = native;
    if let Some(max) = settings.resolution.max_height()
        && size.1 > max
    {
        size = scaled(size, f64::from(max) / f64::from(size.1));
    }
    if settings.format == Format::Gif && size.0 > GIF_WIDTH {
        size = scaled(size, f64::from(GIF_WIDTH) / f64::from(size.0));
    }
    size
}

const fn first_codec(format: Format) -> Codec {
    match format {
        Format::Av1 => Codec::Av1,
        // Quick to decode again for the GIF conversion.
        Format::H264 | Format::Gif => Codec::H264,
        Format::Hevc => Codec::Hevc,
    }
}

/// The codec and size that encoders take for `size`, and what had to change.
fn within_codec_limits(size: (u32, u32), codec: Codec) -> (Codec, (u32, u32), Vec<String>) {
    let mut notes = Vec::new();
    let codec = codec_for(size, codec, &mut notes);
    let size = fitted_to(size, codec, &mut notes);
    (codec, size, notes)
}

/// The longer side of `(w, h)`.
fn longest((w, h): (u32, u32)) -> u32 {
    w.max(h)
}

/// `codec`, unless it is H.264 and `size` is too big for it: then AV1, noted.
fn codec_for(size: (u32, u32), codec: Codec, notes: &mut Vec<String>) -> Codec {
    let limit = Codec::H264.max_side();
    if codec != Codec::H264 || longest(size) <= limit {
        return codec;
    }
    notes.push(format!(
        "H.264 can't encode more than {limit} px across: recording in AV1"
    ));
    Codec::Av1
}

/// `size`, scaled down (noted) if `codec` can't encode it.
fn fitted_to(size: (u32, u32), codec: Codec, notes: &mut Vec<String>) -> (u32, u32) {
    let limit = codec.max_side();
    if longest(size) <= limit {
        return size;
    }
    let (w, h) = scaled(size, f64::from(limit) / f64::from(longest(size)));
    // Rounding to even may have gone a pixel over.
    let (w, h) = (w.min(limit), h.min(limit));
    notes.push(format!(
        "{} can't encode more than {limit} px across: scaled to {w}x{h}",
        codec.label()
    ));
    (w, h)
}

fn strings<const N: usize>(words: [&str; N]) -> impl Iterator<Item = String> {
    words.into_iter().map(String::from)
}

const fn audio_sources(audio: Audio) -> Option<&'static str> {
    match audio {
        Audio::Off => None,
        Audio::Output => Some("default_output"),
        Audio::Input => Some("default_input"),
        Audio::Both => Some("default_output|default_input"),
    }
}

/// `gpu-screen-recorder` arguments.
#[must_use]
pub fn gsr_args(job: &Job, out: &Path) -> Vec<String> {
    let Rect { x, y, w, h } = job.area;
    let frames = if job.variable { "vfr" } else { "cfr" };
    let mut args = vec!["-w".to_string(), format!("{w}x{h}+{x}+{y}")];
    args.extend(["-f".to_string(), job.fps.to_string()]);
    let (codec, quality) = (gsr_codec(job.codec), gsr_quality(job.quality));
    args.extend(strings([
        "-fm", frames, "-k", codec, "-q", quality, "-c", "mp4", "-cursor", "yes",
    ]));
    // Written once the first frame is in: the signal that recording really started.
    args.extend(strings(["-write-first-frame-ts", "yes"]));
    if let Some((w, h)) = job.size {
        args.extend(["-s".into(), format!("{w}x{h}")]);
    }
    args.extend(gsr_audio_args(job));
    args.extend(["-o".into(), out.display().to_string()]);
    args
}

const fn gsr_codec(codec: Codec) -> &'static str {
    match codec {
        Codec::Av1 => "av1",
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
    }
}

const fn gsr_quality(quality: Quality) -> &'static str {
    match quality {
        Quality::Medium => "medium",
        Quality::High => "high",
        Quality::VeryHigh => "very_high",
        Quality::Ultra => "ultra",
    }
}

/// The sound sources, and its codec: H.264 is the compatibility choice, so AAC goes with it;
/// otherwise gsr's Opus.
fn gsr_audio_args(job: &Job) -> Vec<String> {
    let Some(sources) = audio_sources(job.audio) else {
        return Vec::new();
    };
    let aac = (job.codec == Codec::H264).then_some(["-ac", "aac"]);
    strings(["-a", sources])
        .chain(aac.into_iter().flatten().map(String::from))
        .collect()
}

/// `wf-recorder` arguments (fallback when gpu-screen-recorder is missing). It encodes on the
/// CPU with these codecs, and records the default input when sound is on.
#[must_use]
pub fn wf_args(job: &Job, out: &Path) -> Vec<String> {
    let Rect { x, y, w, h } = job.area;
    let codec = match job.codec {
        Codec::Av1 => "libsvtav1",
        Codec::H264 => "libx264",
        Codec::Hevc => "libx265",
    };
    let mut args: Vec<String> = vec!["-y".into(), "-g".into(), format!("{x},{y} {w}x{h}")];
    // Without -r, wf-recorder only takes frames when the screen changes.
    if !job.variable {
        args.extend(["-r".into(), job.fps.to_string()]);
    }
    args.extend(strings(["-c", codec]));
    if let Some((w, h)) = job.size {
        args.extend(["-F".into(), format!("scale={w}:{h}")]);
    }
    if job.audio != Audio::Off {
        args.push("-a".into());
    }
    args.extend(["-f".into(), out.display().to_string()]);
    args
}

/// `ffmpeg` arguments turning the recorded `video` into `gif`: one palette for the whole clip,
/// recomputed only where the picture changes.
#[must_use]
pub fn gif_args(fps: u32, video: &Path, gif: &Path) -> Vec<String> {
    let filter = format!(
        "fps={fps},split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"
    );
    let mut args: Vec<String> = strings(["-v", "error", "-y", "-i"]).collect();
    args.push(video.display().to_string());
    args.extend([
        "-vf".into(),
        filter,
        "-loop".into(),
        "0".into(),
        gif.display().to_string(),
    ]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use screencapture_core::capture::PixelSize;

    fn monitor(name: &str, x: i32, w: i32, h: i32, hz: u32) -> MonitorInfo {
        MonitorInfo {
            name: name.into(),
            rect: Rect::new(x, 0, w, h),
            output_position: (x, 0),
            refresh_mhz: Some(hz * 1000),
            pixels: PixelSize::new(unsigned(w), unsigned(h)),
        }
    }

    /// Three 2560×1440 screens side by side, like the development machine.
    fn three() -> Vec<MonitorInfo> {
        vec![
            monitor("DP-1", 0, 2560, 1440, 144),
            monitor("HDMI-A-1", 2560, 2560, 1440, 144),
            monitor("DP-2", 5120, 2560, 1440, 60),
        ]
    }

    #[test]
    fn targets_cover_their_screens() {
        let m = three();
        assert_eq!(area(&Target::Focused, &m, Some("DP-2")).unwrap(), m[2].rect);
        assert_eq!(
            area(&Target::Focused, &m, None).unwrap(),
            m[0].rect,
            "first when unknown"
        );
        assert_eq!(area(&Target::Focused, &m, Some("gone")).unwrap(), m[0].rect);
        assert_eq!(area(&Target::All, &m, None).unwrap(), Rect::new(0, 0, 7680, 1440));
        assert_eq!(
            area(&Target::Screens(vec!["HDMI-A-1".into()]), &m, None).unwrap(),
            m[1].rect
        );
        assert!(area(&Target::Screens(vec!["DP-9".into()]), &m, None).is_err());
    }

    #[test]
    fn a_region_is_kept_to_the_screens() {
        let m = three();
        let r = Rect::new(100, 200, 640, 480);
        assert_eq!(area(&Target::Region(r), &m, None).unwrap(), r);
        assert_eq!(
            area(&Target::Region(Rect::new(7000, 1000, 1000, 1000)), &m, None).unwrap(),
            Rect::new(7000, 1000, 680, 440)
        );
        assert!(area(&Target::Region(Rect::new(0, 2000, 10, 10)), &m, None).is_err());
    }

    #[test]
    fn the_border_fits_outside_a_region_in_the_middle() {
        let r = Rect::new(100, 200, 640, 480);
        assert_eq!(inset(r, &three()).unwrap(), r);
    }

    #[test]
    fn a_full_screen_gives_up_a_border_width_on_every_edge() {
        let m = [monitor("DP-1", 0, 2560, 1440, 60)];
        assert_eq!(
            inset(m[0].rect, &m).unwrap(),
            Rect::new(BORDER, BORDER, 2560 - 2 * BORDER, 1440 - 2 * BORDER)
        );
    }

    #[test]
    fn a_recorded_neighbouring_screen_can_hold_the_border() {
        let m = three();
        // HDMI-A-1 with DP-1 and DP-2 recorded too: they hold the border at its sides.
        assert_eq!(
            inset(m[1].rect, &m).unwrap(),
            Rect::new(2560, BORDER, 2560, 1440 - 2 * BORDER)
        );
        // A region touching the desktop's top edge only.
        assert_eq!(
            inset(Rect::new(100, 0, 640, 480), &m).unwrap(),
            Rect::new(100, BORDER, 640, 480 - BORDER - 1),
            "shrunk at the top, then to an even height"
        );
    }

    #[test]
    fn sizes_are_made_even() {
        let r = inset(Rect::new(101, 201, 641, 479), &three()).unwrap();
        assert_eq!((r.w % 2, r.h % 2), (0, 0));
        assert_eq!((r.x, r.y), (101, 201));
    }

    #[test]
    fn too_small_to_record_is_an_error() {
        let m = [monitor("DP-1", 0, 2560, 1440, 60)];
        assert!(inset(Rect::new(0, 0, 4, 400), &m).is_err());
    }

    #[test]
    fn native_is_the_refresh_rate_and_fixed_rates_never_exceed_it() {
        assert_eq!(frame_rate(Fps::Native, Some(144), None), (144, false));
        assert_eq!(frame_rate(Fps::Native, None, None), (60, false), "unknown: 60");
        assert_eq!(frame_rate(Fps::F120, Some(144), None), (120, false));
        assert_eq!(frame_rate(Fps::F120, Some(60), None), (60, false));
        assert_eq!(frame_rate(Fps::F15, Some(144), None), (15, false));
    }

    #[test]
    fn adaptive_backs_off_as_the_machine_gets_busy() {
        assert_eq!(adaptive_tier(0.1), u32::MAX, "idle: as fast as the screen");
        assert_eq!(adaptive_tier(0.6), 60);
        assert_eq!(adaptive_tier(0.8), 30);
        assert_eq!(adaptive_tier(0.95), 15);
        assert_eq!(frame_rate(Fps::Adaptive, Some(144), Some(0.1)), (144, true));
        assert_eq!(frame_rate(Fps::Adaptive, Some(144), Some(0.8)), (30, true));
        assert_eq!(frame_rate(Fps::Adaptive, Some(50), Some(0.6)), (50, true));
        assert_eq!(
            frame_rate(Fps::Adaptive, Some(144), None),
            (60, true),
            "no reading: middle"
        );
    }

    fn planned(target: &Target, settings: Settings, monitors: &[MonitorInfo]) -> Plan {
        plan(target, settings, monitors, Some("DP-2"), None).expect("the target is recordable")
    }

    /// The job of a plan with a single part.
    fn single(plan: &Plan) -> &Job {
        assert_eq!(plan.parts.len(), 1, "one screen, one part");
        &plan.parts[0].job
    }

    fn with_format(format: Format) -> Settings {
        Settings {
            format,
            ..Settings::default()
        }
    }

    fn at_720p() -> Settings {
        Settings {
            resolution: super::super::settings::Resolution::P720,
            ..Settings::default()
        }
    }

    fn wide(width: i32) -> [MonitorInfo; 1] {
        [MonitorInfo {
            rect: Rect::new(0, 0, width, 1440),
            pixels: PixelSize::new(unsigned(width), 1440),
            ..monitor("WIDE", 0, 0, 0, 60)
        }]
    }

    #[test]
    fn a_plan_uses_the_screens_refresh_rate() {
        assert_eq!(
            single(&planned(&Target::Focused, Settings::default(), &three())).fps,
            60
        );
    }

    #[test]
    fn the_default_format_is_av1() {
        assert_eq!(
            single(&planned(&Target::Focused, Settings::default(), &three())).codec,
            Codec::Av1
        );
    }

    #[test]
    fn a_screen_is_recorded_with_room_for_the_frame() {
        let m = three();
        let job = single(&planned(&Target::Focused, Settings::default(), &m)).clone();
        assert_eq!(
            job.area,
            inset(m[2].rect, &m[2..]).expect("a screen is recordable")
        );
    }

    #[test]
    fn a_native_recording_is_not_scaled() {
        assert_eq!(
            single(&planned(&Target::Focused, Settings::default(), &three())).size,
            None
        );
    }

    #[test]
    fn a_plain_recording_has_nothing_to_say() {
        assert_eq!(
            planned(&Target::Focused, Settings::default(), &three()).notes,
            Vec::<String>::new()
        );
    }

    #[test]
    fn resolution_caps_the_height_and_keeps_the_aspect() {
        let region = Target::Region(Rect::new(100, 100, 1280, 960));
        assert_eq!(
            single(&planned(&region, at_720p(), &three())).size,
            Some((960, 720))
        );
    }

    #[test]
    fn a_resolution_above_the_area_leaves_it_alone() {
        let region = Target::Region(Rect::new(100, 100, 640, 480));
        assert_eq!(single(&planned(&region, at_720p(), &three())).size, None);
    }

    #[test]
    fn h264_too_wide_switches_to_av1() {
        let job = single(&planned(&Target::All, with_format(Format::H264), &wide(5120))).clone();
        assert_eq!(job.codec, Codec::Av1);
    }

    #[test]
    fn switching_codecs_is_told() {
        assert_eq!(
            planned(&Target::All, with_format(Format::H264), &wide(5120))
                .notes
                .len(),
            1
        );
    }

    #[test]
    fn h264_within_its_limit_stays_h264() {
        let region = Target::Region(Rect::new(10, 10, 4000, 1000));
        assert_eq!(
            single(&planned(&region, with_format(Format::H264), &wide(5120))).codec,
            Codec::H264
        );
    }

    #[test]
    fn beyond_what_any_codec_takes_is_scaled_to_fit() {
        let (w, _) = single(&planned(&Target::All, Settings::default(), &wide(10240)))
            .size
            .expect("scaled");
        assert!(w <= 8192, "{w}");
    }

    #[test]
    fn scaling_to_fit_is_told() {
        assert_eq!(
            planned(&Target::All, Settings::default(), &wide(10240))
                .notes
                .len(),
            1
        );
    }

    /// `count` 1920×1080 logical screens side by side, shown at scale `density`.
    fn row(count: i32, density: u32) -> Vec<MonitorInfo> {
        let screen = |index: i32| MonitorInfo {
            pixels: PixelSize::new(1920 * density, 1080 * density),
            ..monitor(&format!("S-{index}"), index * 1920, 1920, 1080, 60)
        };
        (0..count).map(screen).collect()
    }

    #[test]
    fn h264_too_wide_in_pixels_switches_to_av1() {
        // 3834 logical px across, 7668 pixels.
        let plan = planned(&Target::All, with_format(Format::H264), &row(2, 2));
        assert_eq!(plan.join.expect("joined").codec, Codec::Av1);
    }

    #[test]
    fn a_native_hidpi_recording_is_not_scaled() {
        assert_eq!(
            single(&planned(&Target::All, Settings::default(), &row(1, 2))).size,
            None
        );
    }

    #[test]
    fn hidpi_screens_join_at_their_pixel_size() {
        let joined = planned(&Target::All, Settings::default(), &row(2, 2)).join;
        let plain = planned(&Target::All, Settings::default(), &row(2, 1)).join;
        let (joined, plain) = (joined.expect("joined"), plain.expect("joined"));
        assert_eq!(joined.size, (plain.size.0 * 2, plain.size.1 * 2));
    }

    #[test]
    fn hidpi_parts_at_their_pixel_size_are_not_scaled() {
        let plan = planned(&Target::All, Settings::default(), &row(2, 2));
        assert_eq!(plan.parts[0].job.size, None);
    }

    #[test]
    fn hidpi_screens_too_wide_in_pixels_are_scaled_to_fit() {
        // 5754 logical px across, 11508 pixels.
        let join = planned(&Target::All, Settings::default(), &row(3, 2)).join;
        assert!(join.expect("joined").size.0 <= 8192);
    }

    #[test]
    fn scaling_hidpi_screens_to_fit_is_told() {
        let notes = planned(&Target::All, Settings::default(), &row(3, 2)).notes;
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn gif_records_slow_already() {
        assert_eq!(
            single(&planned(&Target::Focused, with_format(Format::Gif), &three())).fps,
            GIF_FPS
        );
    }

    #[test]
    fn gif_records_small_already() {
        let (w, _) = single(&planned(&Target::Focused, with_format(Format::Gif), &three()))
            .size
            .expect("scaled");
        assert_eq!(w, GIF_WIDTH);
    }

    #[test]
    fn gif_records_in_h264_quick_to_decode_again() {
        assert_eq!(
            single(&planned(&Target::Focused, with_format(Format::Gif), &three())).codec,
            Codec::H264
        );
    }

    #[test]
    fn a_summary_names_the_format_and_rate() {
        assert_eq!(
            planned(&Target::Focused, Settings::default(), &three()).summary(),
            "MP4 · AV1 · 60 fps"
        );
    }

    #[test]
    fn a_gif_summary_says_gif() {
        assert_eq!(
            planned(&Target::Focused, with_format(Format::Gif), &three()).summary(),
            "GIF · 25 fps"
        );
    }

    #[test]
    fn a_summary_counts_several_screens() {
        assert!(
            planned(&Target::All, Settings::default(), &three())
                .summary()
                .ends_with(" · 3 screens")
        );
    }

    #[test]
    fn one_screen_needs_no_joining() {
        assert_eq!(
            planned(&Target::Focused, Settings::default(), &three()).join,
            None
        );
    }

    #[test]
    fn several_screens_are_recorded_one_per_screen() {
        assert_eq!(
            planned(&Target::All, Settings::default(), &three()).parts.len(),
            3
        );
    }

    #[test]
    fn several_screens_are_joined_into_one_video() {
        let join = planned(&Target::All, Settings::default(), &three())
            .join
            .expect("joined");
        assert_eq!(join.size, (7672, 1434));
    }

    #[test]
    fn parts_land_side_by_side() {
        let plan = planned(&Target::All, Settings::default(), &three());
        assert_eq!(plan.parts[1].slot.x, plan.parts[0].slot.w);
    }

    #[test]
    fn only_the_first_part_records_sound() {
        let settings = Settings {
            audio: Audio::Both,
            ..Settings::default()
        };
        let plan = planned(&Target::All, settings, &three());
        assert_eq!(plan.parts[1].job.audio, Audio::Off);
    }

    #[test]
    fn parts_record_finer_than_the_result_since_joining_encodes_again() {
        let plan = planned(&Target::All, Settings::default(), &three());
        assert_eq!(plan.parts[0].job.quality, Quality::Ultra);
    }

    #[test]
    fn the_resolution_caps_the_joined_video() {
        let join = planned(&Target::All, at_720p(), &three()).join.expect("joined");
        assert_eq!(join.size.1, 720);
    }

    #[test]
    fn scaled_parts_fill_their_slots_exactly() {
        let plan = planned(&Target::All, at_720p(), &three());
        let slot = plan.parts[0].slot;
        assert_eq!(plan.parts[0].job.size, Some((unsigned(slot.w), unsigned(slot.h))));
    }

    #[test]
    fn h264_too_wide_once_joined_joins_in_av1() {
        let join = planned(&Target::All, with_format(Format::H264), &three())
            .join
            .expect("joined");
        assert_eq!(join.codec, Codec::Av1);
    }

    #[test]
    fn a_joined_gif_is_made_from_the_joined_video() {
        let plan = planned(&Target::All, with_format(Format::Gif), &three());
        assert!(!plan.parts[0].job.gif && plan.join.is_some_and(|join| join.gif));
    }

    fn job() -> Job {
        Job {
            area: Rect::new(5123, 3, 2554, 1434),
            fps: 60,
            variable: false,
            size: None,
            codec: Codec::Av1,
            quality: Quality::VeryHigh,
            audio: Audio::Off,
            gif: false,
        }
    }

    fn has(args: &[String], pair: &[&str]) -> bool {
        args.windows(pair.len()).any(|w| w == pair)
    }

    #[test]
    fn gpu_screen_recorder_gets_the_region_and_settings() {
        let args = gsr_args(&job(), Path::new("/v/a.mp4"));
        assert!(has(&args, &["-w", "2554x1434+5123+3"]), "{args:?}");
        assert!(has(&args, &["-f", "60"]));
        assert!(has(&args, &["-fm", "cfr"]));
        assert!(has(&args, &["-k", "av1"]));
        assert!(has(&args, &["-q", "very_high"]));
        assert!(has(&args, &["-c", "mp4"]));
        assert!(has(&args, &["-o", "/v/a.mp4"]));
        assert!(has(&args, &["-write-first-frame-ts", "yes"]), "start signal");
        assert!(!args.iter().any(|a| a == "-a" || a == "-s"));
    }

    #[test]
    fn gpu_screen_recorder_scaling_sound_and_variable_rate() {
        let j = Job {
            variable: true,
            size: Some((1280, 718)),
            codec: Codec::H264,
            audio: Audio::Both,
            quality: Quality::Medium,
            ..job()
        };
        let args = gsr_args(&j, Path::new("/v/a.mp4"));
        assert!(has(&args, &["-fm", "vfr"]));
        assert!(has(&args, &["-s", "1280x718"]));
        assert!(has(&args, &["-k", "h264"]));
        assert!(has(&args, &["-q", "medium"]));
        assert!(has(&args, &["-a", "default_output|default_input"]));
        assert!(has(&args, &["-ac", "aac"]), "H.264 is for compatibility: AAC");
        for (audio, source) in [(Audio::Output, "default_output"), (Audio::Input, "default_input")] {
            let args = gsr_args(&Job { audio, ..job() }, Path::new("/v/a.mp4"));
            assert!(has(&args, &["-a", source]), "{args:?}");
            assert!(!args.iter().any(|a| a == "aac"), "Opus otherwise");
        }
    }

    #[test]
    fn wf_recorder_fallback() {
        let j = Job {
            size: Some((1280, 718)),
            audio: Audio::Input,
            ..job()
        };
        let args = wf_args(&j, Path::new("/v/a.mp4"));
        assert!(has(&args, &["-g", "5123,3 2554x1434"]), "{args:?}");
        assert!(has(&args, &["-r", "60"]));
        assert!(has(&args, &["-c", "libsvtav1"]));
        assert!(has(&args, &["-F", "scale=1280:718"]));
        assert!(has(&args, &["-f", "/v/a.mp4"]));
        assert!(args.iter().any(|a| a == "-a"));
        assert!(
            !wf_args(&job(), Path::new("/v/a.mp4"))
                .iter()
                .any(|a| a == "-a" || a == "-F")
        );
    }

    fn gif_args_for_a_clip() -> Vec<String> {
        gif_args(25, Path::new("/v/a.part.mp4"), Path::new("/v/a.gif"))
    }

    fn video_filter(args: &[String]) -> &str {
        let at = args.iter().position(|a| a == "-vf").expect("a video filter");
        &args[at + 1]
    }

    #[test]
    fn gif_conversion_reads_the_video() {
        assert!(has(&gif_args_for_a_clip(), &["-i", "/v/a.part.mp4"]));
    }

    #[test]
    fn gif_conversion_keeps_the_frame_rate() {
        assert!(video_filter(&gif_args_for_a_clip()).contains("fps=25"));
    }

    #[test]
    fn gif_conversion_uses_one_palette() {
        let args = gif_args_for_a_clip();
        assert!(video_filter(&args).contains("palettegen") && video_filter(&args).contains("paletteuse"));
    }

    #[test]
    fn gif_conversion_writes_the_gif_last() {
        assert_eq!(gif_args_for_a_clip().last().map(String::as_str), Some("/v/a.gif"));
    }
}
