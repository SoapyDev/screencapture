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
    /// Adjustments the user should hear about.
    pub notes: Vec<String>,
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
/// need them).
pub fn inset(area: Rect, monitors: &[MonitorInfo]) -> Result<Rect> {
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
    // Encoders work on 2×2 blocks of colour.
    r -= (r - l) % 2;
    b -= (b - t) % 2;
    let rect = Rect::new(l, t, r - l, b - t);
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
) -> Result<Job> {
    let wanted = area(target, monitors, focused)?;
    let screens: Vec<&MonitorInfo> = monitors.iter().filter(|m| m.rect.intersect(wanted).is_some()).collect();
    if screens.len() > 1 {
        bail!("recording several screens at once isn't supported yet: pick one screen, or a region inside one");
    }
    let area = inset(wanted, monitors)?;
    let native = screens
        .iter()
        .filter_map(|m| m.refresh_mhz)
        .max()
        .map(|mhz| (mhz + 500) / 1000);
    let gif = settings.format == Format::Gif;
    let (mut fps, variable) = frame_rate(settings.fps, native, load);
    if gif {
        fps = fps.min(GIF_FPS);
    }

    let native_size = (area.w as u32, area.h as u32);
    let mut size = native_size;
    if let Some(max) = settings.resolution.max_height()
        && size.1 > max
    {
        size = scaled(size, f64::from(max) / f64::from(size.1));
    }
    if gif && size.0 > GIF_WIDTH {
        size = scaled(size, f64::from(GIF_WIDTH) / f64::from(size.0));
    }
    let mut notes = Vec::new();
    let mut codec = match settings.format {
        Format::Av1 => Codec::Av1,
        // Quick to decode again for the GIF conversion.
        Format::H264 | Format::Gif => Codec::H264,
        Format::Hevc => Codec::Hevc,
    };
    let longest = |(w, h): (u32, u32)| w.max(h);
    if codec == Codec::H264 && longest(size) > Codec::H264.max_side() {
        codec = Codec::Av1;
        notes.push(format!(
            "H.264 can't encode more than {} px across: recording in AV1",
            Codec::H264.max_side()
        ));
    }
    if longest(size) > codec.max_side() {
        size = scaled(size, f64::from(codec.max_side()) / f64::from(longest(size)));
        // Rounding to even may have gone a pixel over.
        size = (size.0.min(codec.max_side()), size.1.min(codec.max_side()));
        notes.push(format!(
            "{} can't encode more than {} px across: scaled to {}x{}",
            codec.label(),
            codec.max_side(),
            size.0,
            size.1
        ));
    }
    Ok(Job {
        area,
        fps,
        variable,
        size: (size != native_size).then_some(size),
        codec,
        quality: settings.quality,
        audio: settings.audio,
        gif,
        notes,
    })
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
    args.extend(strings(["-k", codec, "-q", quality, "-c", "mp4", "-cursor", "yes"]));
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
pub fn gif_args(job: &Job, video: &Path, gif: &Path) -> Vec<String> {
    let filter = format!(
        "fps={},split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle",
        job.fps
    );
    let mut args: Vec<String> = strings(["-v", "error", "-y", "-i"]).collect();
    args.push(video.display().to_string());
    args.extend(["-vf".into(), filter, "-loop".into(), "0".into(), gif.display().to_string()]);
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
        assert_eq!(area(&Target::Focused, &m, None).unwrap(), m[0].rect, "first when unknown");
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
        assert_eq!(frame_rate(Fps::Adaptive, Some(144), None), (60, true), "no reading: middle");
    }

    #[test]
    fn a_plan_uses_the_screens_refresh_rate() {
        let m = three();
        let job = plan(&Target::Focused, Settings::default(), &m, Some("DP-2"), None).unwrap();
        assert_eq!(job.fps, 60);
        assert_eq!(job.codec, Codec::Av1);
        assert_eq!(job.area, inset(m[2].rect, &m).unwrap());
        assert_eq!(job.size, None);
        assert_eq!(job.notes, Vec::<String>::new());
    }

    #[test]
    fn resolution_caps_the_height_and_keeps_the_aspect() {
        let settings = Settings {
            resolution: super::super::settings::Resolution::P720,
            ..Settings::default()
        };
        let r = Rect::new(100, 100, 1280, 960);
        let job = plan(&Target::Region(r), settings, &three(), None, None).unwrap();
        assert_eq!(job.size, Some((960, 720)));
        // Already smaller: untouched.
        let small = Rect::new(100, 100, 640, 480);
        let job = plan(&Target::Region(small), settings, &three(), None, None).unwrap();
        assert_eq!(job.size, None);
    }

    #[test]
    fn h264_too_wide_switches_to_av1_and_says_so() {
        let m = [MonitorInfo {
            rect: Rect::new(0, 0, 5120, 1440),
            ..monitor("WIDE", 0, 0, 0, 60)
        }];
        let settings = Settings {
            format: Format::H264,
            ..Settings::default()
        };
        let job = plan(&Target::All, settings, &m, None, None).unwrap();
        assert_eq!(job.codec, Codec::Av1);
        assert_eq!(job.notes.len(), 1, "{:?}", job.notes);
        // Narrow enough: stays H.264.
        let job = plan(&Target::Region(Rect::new(10, 10, 4000, 1000)), settings, &m, None, None).unwrap();
        assert_eq!(job.codec, Codec::H264);
    }

    #[test]
    fn beyond_what_any_codec_takes_is_scaled_to_fit() {
        let m = [MonitorInfo {
            rect: Rect::new(0, 0, 10240, 1440),
            ..monitor("HUGE", 0, 0, 0, 60)
        }];
        let job = plan(&Target::All, Settings::default(), &m, None, None).unwrap();
        let (w, h) = job.size.unwrap();
        assert!(w <= 8192 && w % 2 == 0 && h % 2 == 0, "{w}x{h}");
        assert_eq!(job.notes.len(), 1);
    }

    #[test]
    fn gif_records_small_and_slow_already() {
        let settings = Settings {
            format: Format::Gif,
            ..Settings::default()
        };
        let m = three();
        let job = plan(&Target::Focused, settings, &m, None, None).unwrap();
        assert!(job.gif);
        assert_eq!(job.fps, GIF_FPS);
        let (w, h) = job.size.unwrap();
        assert_eq!(w, GIF_WIDTH);
        assert!(h.abs_diff(1280 * 1434 / 2554) <= 1 && h % 2 == 0, "{h}");
        assert_eq!(job.codec, Codec::H264, "quick to decode for the conversion");
    }

    #[test]
    fn several_screens_are_not_supported_yet() {
        let err = plan(&Target::All, Settings::default(), &three(), None, None).unwrap_err();
        assert!(format!("{err}").contains("several screens"), "{err}");
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
            notes: Vec::new(),
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
        assert!(!wf_args(&job(), Path::new("/v/a.mp4")).iter().any(|a| a == "-a" || a == "-F"));
    }

    #[test]
    fn gif_conversion_uses_a_palette() {
        let j = Job {
            gif: true,
            fps: 25,
            ..job()
        };
        let args = gif_args(&j, Path::new("/v/a.part.mp4"), Path::new("/v/a.gif"));
        assert!(has(&args, &["-i", "/v/a.part.mp4"]));
        let filter = &args[args.iter().position(|a| a == "-vf").unwrap() + 1];
        assert!(filter.contains("fps=25") && filter.contains("palettegen") && filter.contains("paletteuse"));
        assert_eq!(args.last().unwrap(), "/v/a.gif");
    }
}
