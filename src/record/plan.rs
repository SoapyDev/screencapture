//! Turning "record this, like so" into exactly what the recorder is asked for. No I/O here.

use std::path::Path;

use anyhow::{Result, bail};

use super::settings::{Audio, Format, Fps, Quality, Settings};
use crate::capture::{MonitorInfo, Rect};

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
pub fn area(target: &Target, monitors: &[MonitorInfo], focused: Option<&str>) -> Result<Rect> {
    let union = |rects: &mut dyn Iterator<Item = Rect>| rects.reduce(Rect::union);
    let found = match target {
        Target::Focused => focused
            .and_then(|name| monitors.iter().find(|m| m.name == name))
            .or_else(|| monitors.first())
            .map(|m| m.rect),
        Target::All => union(&mut monitors.iter().map(|m| m.rect)),
        Target::Screens(names) => {
            let mut rects = Vec::with_capacity(names.len());
            for name in names {
                match monitors.iter().find(|m| &m.name == name) {
                    Some(m) => rects.push(m.rect),
                    None => bail!("no screen named {name:?}"),
                }
            }
            union(&mut rects.into_iter())
        }
        Target::Region(region) => {
            let found = union(&mut monitors.iter().filter_map(|m| m.rect.intersect(*region)));
            if found.is_none() {
                bail!("the region is on no screen");
            }
            found
        }
    };
    found.ok_or_else(|| anyhow::anyhow!("no screens found"))
}

/// Smallest side worth recording, in logical px.
const MIN_SIDE: i32 = 16;

/// `area` shrunk where the border would not fit on any screen, and to even sizes (encoders
/// need them). (Recordings go through [`super::split`]; this is the one-piece case.)
#[cfg(test)]
pub fn inset(area: Rect, monitors: &[MonitorInfo]) -> Result<Rect> {
    let room = room_for_border(area, monitors);
    // Encoders work on 2×2 blocks of colour.
    let even = Rect::new(room.x, room.y, room.w - room.w % 2, room.h - room.h % 2);
    ensure_recordable(even)
}

/// `area` shrunk where the strip just outside an edge, which the border needs, is on no screen.
pub fn room_for_border(area: Rect, monitors: &[MonitorInfo]) -> Rect {
    let on_screen = |(x, y): (i32, i32)| monitors.iter().any(|m| m.rect.contains(x, y));
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
pub fn ensure_recordable(rect: Rect) -> Result<Rect> {
    if rect.w < MIN_SIDE || rect.h < MIN_SIDE {
        bail!("the area is too small to record");
    }
    Ok(rect)
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
pub fn adaptive_tier(load: f32) -> u32 {
    match load {
        l if l < 0.5 => u32::MAX,
        l if l < 0.75 => 60,
        l if l < 0.9 => 30,
        _ => 15,
    }
}

/// Nearest even size, at least 2.
fn even(v: f64) -> u32 {
    ((v / 2.).round() as u32 * 2).max(2)
}

/// `(w, h)` scaled by `factor`, to even sizes.
fn scaled((w, h): (u32, u32), factor: f64) -> (u32, u32) {
    (even(f64::from(w) * factor), even(f64::from(h) * factor))
}

/// The plan for recording `target`.
pub fn plan(
    target: &Target,
    settings: Settings,
    monitors: &[MonitorInfo],
    focused: Option<&str>,
    load: Option<f32>,
) -> Result<Plan> {
    let area = area(target, monitors, focused)?;
    let screens = screens_of(target, area, monitors, focused);
    let (parts, joined) = super::split::split(area, &screens, monitors)?;
    let video = Video::fitted(joined, settings);
    let rate = frame_rate(settings.fps, refresh_rate(&screens), load);
    let several = parts.len() > 1;
    let jobs = parts
        .iter()
        .enumerate()
        .map(|(index, part)| {
            video.part_job(
                part,
                PartRole {
                    first: index == 0,
                    several,
                },
                settings,
                rate,
            )
        })
        .collect();
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

fn focused_screen<'a>(monitors: &'a [MonitorInfo], focused: Option<&str>) -> Option<&'a MonitorInfo> {
    focused
        .and_then(|name| monitors.iter().find(|m| m.name == name))
        .or_else(|| monitors.first())
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
    /// Video px per logical px.
    scale: f64,
    codec: Codec,
    gif: bool,
    notes: Vec<String>,
}

impl Video {
    fn fitted(joined: (i32, i32), settings: Settings) -> Self {
        let native = (joined.0 as u32, joined.1 as u32);
        let gif = settings.format == Format::Gif;
        let wanted = capped_size(native, settings);
        let (codec, size, notes) = within_codec_limits(wanted, first_codec(settings.format));
        let scale = f64::from(size.0) / f64::from(native.0);
        Self {
            joined,
            scale,
            codec,
            gif,
            notes,
        }
    }

    /// A desktop length in video px (even, rounded the same way for every edge, so that
    /// neighbouring parts meet exactly).
    fn length(&self, logical: i32) -> i32 {
        if self.scale >= 1. {
            return logical;
        }
        // Not `even`: positions can be 0.
        ((f64::from(logical) * self.scale / 2.).round() * 2.) as i32
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
        let native = (part.area.w, part.area.h) == (slot.w, slot.h);
        let job = Job {
            area: part.area,
            fps: if self.gif { rate.0.min(GIF_FPS) } else { rate.0 },
            variable: rate.1,
            size: (!native).then_some((slot.w as u32, slot.h as u32)),
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
                self.length(self.joined.0) as u32,
                self.length(self.joined.1) as u32,
            ),
            codec: self.codec,
            quality: settings.quality,
            fps: if self.gif { fps.min(GIF_FPS) } else { fps },
            audio: settings.audio != Audio::Off,
            gif: self.gif,
        }
    }
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
    let longest = |(w, h): (u32, u32)| w.max(h);
    let mut notes = Vec::new();
    let mut codec = codec;
    if codec == Codec::H264 && longest(size) > Codec::H264.max_side() {
        codec = Codec::Av1;
        notes.push(format!(
            "H.264 can't encode more than {} px across: recording in AV1",
            Codec::H264.max_side()
        ));
    }
    if longest(size) <= codec.max_side() {
        return (codec, size, notes);
    }
    let fitted = scaled(size, f64::from(codec.max_side()) / f64::from(longest(size)));
    // Rounding to even may have gone a pixel over.
    let fitted = (fitted.0.min(codec.max_side()), fitted.1.min(codec.max_side()));
    notes.push(format!(
        "{} can't encode more than {} px across: scaled to {}x{}",
        codec.label(),
        codec.max_side(),
        fitted.0,
        fitted.1
    ));
    (codec, fitted, notes)
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
pub fn gsr_args(job: &Job, out: &Path) -> Vec<String> {
    let Rect { x, y, w, h } = job.area;
    let codec = match job.codec {
        Codec::Av1 => "av1",
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
    };
    let quality = match job.quality {
        Quality::Medium => "medium",
        Quality::High => "high",
        Quality::VeryHigh => "very_high",
        Quality::Ultra => "ultra",
    };
    let mut args: Vec<String> = vec![
        "-w".into(),
        format!("{w}x{h}+{x}+{y}"),
        "-f".into(),
        job.fps.to_string(),
        "-fm".into(),
        if job.variable { "vfr" } else { "cfr" }.into(),
    ];
    args.extend(strings([
        "-k", codec, "-q", quality, "-c", "mp4", "-cursor", "yes",
    ]));
    // Written once the first frame is in: the signal that recording really started.
    args.extend(strings(["-write-first-frame-ts", "yes"]));
    if let Some((w, h)) = job.size {
        args.extend(["-s".into(), format!("{w}x{h}")]);
    }
    if let Some(sources) = audio_sources(job.audio) {
        args.extend(strings(["-a", sources]));
        // H.264 is the compatibility choice: AAC goes with it. Otherwise gsr's Opus.
        if job.codec == Codec::H264 {
            args.extend(strings(["-ac", "aac"]));
        }
    }
    args.extend(["-o".into(), out.display().to_string()]);
    args
}

/// `wf-recorder` arguments (fallback when gpu-screen-recorder is missing). It encodes on the
/// CPU with these codecs, and records the default input when sound is on.
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

    fn monitor(name: &str, x: i32, w: i32, h: i32, hz: u32) -> MonitorInfo {
        MonitorInfo {
            name: name.into(),
            rect: Rect::new(x, 0, w, h),
            output_position: (x, 0),
            refresh_mhz: Some(hz * 1000),
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
    fn a_neighbouring_screen_can_hold_the_border() {
        let m = three();
        // HDMI-A-1 alone: its left and right edges have DP-1 and DP-2 next to them.
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
        assert_eq!(job.area, inset(m[2].rect, &m).expect("a screen is recordable"));
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
        assert_eq!(plan.parts[0].job.size, Some((slot.w as u32, slot.h as u32)));
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
