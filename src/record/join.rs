//! Joining the per-screen recordings into one video with ffmpeg: each part scaled into its slot,
//! all lined up on the latest first frame, encoded once.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use super::{
    plan::{Codec, Join},
    settings::Quality,
};
use crate::capture::Rect;

/// One part's video and its place in the joined one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub video: PathBuf,
    pub slot: Rect,
    /// Skipped at its start, so every part starts at the same moment.
    pub skip: Duration,
}

/// Where the joined video is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    /// NVENC: fast, NVIDIA only.
    Gpu,
    /// Software: anywhere, slower.
    Cpu,
}

/// How much of each part to skip, from their first frames' times (µs on one clock): the part
/// that started last sets the start. Unknown times skip nothing.
pub fn skips(first_frames: &[Option<u64>]) -> Vec<Duration> {
    let latest = first_frames.iter().flatten().max().copied().unwrap_or(0);
    first_frames
        .iter()
        .map(|first| first.map_or(Duration::ZERO, |at| Duration::from_micros(latest - at)))
        .collect()
}

/// ffmpeg arguments joining `inputs` as `join` says into `out`.
pub fn join_args(inputs: &[Input], join: &Join, out: &Path, encoder: Encoder) -> Vec<String> {
    let mut args = words(&["-v", "error", "-y", "-hwaccel", "auto"]);
    for input in inputs {
        let skip = format!("{:.3}", input.skip.as_secs_f64());
        args.extend(["-ss".into(), skip, "-i".into(), input.video.display().to_string()]);
    }
    args.extend([
        "-filter_complex".into(),
        filter_graph(inputs, join.fps),
        "-map".into(),
        "[joined]".into(),
    ]);
    if join.audio {
        args.extend(words(&["-map", "0:a", "-c:a", "copy"]));
    }
    args.extend(encoder_args(join.codec, join.quality, encoder));
    args.extend(words(&["-movflags", "+faststart"]));
    args.push(out.display().to_string());
    args
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| (*word).to_string()).collect()
}

/// Each part scaled into its slot, the slots stacked into one picture at `fps`.
fn filter_graph(inputs: &[Input], fps: u32) -> String {
    let scaled: Vec<String> = inputs
        .iter()
        .enumerate()
        .map(|(i, input)| format!("[{i}:v]scale={}:{},setsar=1[p{i}];", input.slot.w, input.slot.h))
        .collect();
    let labels: Vec<String> = (0..inputs.len()).map(|i| format!("[p{i}]")).collect();
    let layout: Vec<String> = inputs
        .iter()
        .map(|input| format!("{}_{}", input.slot.x, input.slot.y))
        .collect();
    format!(
        "{}{}xstack=inputs={}:layout={}:fill=black:shortest=1,fps={fps},format=yuv420p[joined]",
        scaled.concat(),
        labels.concat(),
        inputs.len(),
        layout.join("|")
    )
}

/// The video encoder and its quality setting.
fn encoder_args(codec: Codec, quality: Quality, encoder: Encoder) -> Vec<String> {
    let level = quality_level(codec, quality, encoder).to_string();
    let (name, mut args) = match (encoder, codec) {
        (Encoder::Gpu, Codec::Av1) => (
            "av1_nvenc",
            words(&["-preset", "p5", "-rc", "vbr", "-b:v", "0", "-cq"]),
        ),
        (Encoder::Gpu, Codec::H264) => (
            "h264_nvenc",
            words(&["-preset", "p5", "-rc", "vbr", "-b:v", "0", "-cq"]),
        ),
        (Encoder::Gpu, Codec::Hevc) => (
            "hevc_nvenc",
            words(&["-preset", "p5", "-rc", "vbr", "-b:v", "0", "-cq"]),
        ),
        (Encoder::Cpu, Codec::Av1) => ("libsvtav1", words(&["-preset", "8", "-crf"])),
        (Encoder::Cpu, Codec::H264) => ("libx264", words(&["-preset", "veryfast", "-crf"])),
        (Encoder::Cpu, Codec::Hevc) => ("libx265", words(&["-preset", "fast", "-crf"])),
    };
    args.push(level);
    let mut all = words(&["-c:v", name]);
    all.append(&mut args);
    all
}

/// Constant-quality level for `quality` (lower is better; scales differ per encoder).
const fn quality_level(codec: Codec, quality: Quality, encoder: Encoder) -> u32 {
    let finest = match (encoder, codec) {
        (Encoder::Gpu, _) => 20,
        (Encoder::Cpu, Codec::Av1) => 22,
        (Encoder::Cpu, Codec::H264) => 17,
        (Encoder::Cpu, Codec::Hevc) => 19,
    };
    let steps = match quality {
        Quality::Ultra => 0,
        Quality::VeryHigh => 1,
        Quality::High => 2,
        Quality::Medium => 3,
    };
    finest + 4 * steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join() -> Join {
        Join {
            size: (5112, 1434),
            codec: Codec::Av1,
            quality: Quality::VeryHigh,
            fps: 60,
            audio: false,
            gif: false,
        }
    }

    fn inputs() -> Vec<Input> {
        vec![
            Input {
                video: "/v/a.part0.mp4".into(),
                slot: Rect::new(0, 0, 2556, 1434),
                skip: Duration::from_millis(250),
            },
            Input {
                video: "/v/a.part1.mp4".into(),
                slot: Rect::new(2556, 0, 2556, 1434),
                skip: Duration::ZERO,
            },
        ]
    }

    fn args(join: &Join, encoder: Encoder) -> Vec<String> {
        join_args(&inputs(), join, Path::new("/v/a.mp4"), encoder)
    }

    fn has(args: &[String], words: &[&str]) -> bool {
        args.windows(words.len()).any(|w| w == words)
    }

    fn filter(args: &[String]) -> &str {
        let at = args
            .iter()
            .position(|a| a == "-filter_complex")
            .expect("a filter graph");
        &args[at + 1]
    }

    #[test]
    fn the_part_that_started_last_is_not_skipped() {
        assert_eq!(skips(&[Some(1_000_000), Some(1_250_000)])[1], Duration::ZERO);
    }

    #[test]
    fn earlier_parts_skip_the_difference() {
        assert_eq!(
            skips(&[Some(1_000_000), Some(1_250_000)])[0],
            Duration::from_millis(250)
        );
    }

    #[test]
    fn a_part_with_an_unknown_start_skips_nothing() {
        assert_eq!(skips(&[None, Some(1_250_000)])[0], Duration::ZERO);
    }

    #[test]
    fn each_input_skips_its_lead() {
        assert!(has(
            &args(&join(), Encoder::Gpu),
            &["-ss", "0.250", "-i", "/v/a.part0.mp4"]
        ));
    }

    #[test]
    fn every_part_goes_to_its_slot() {
        assert!(filter(&args(&join(), Encoder::Gpu)).contains("layout=0_0|2556_0"));
    }

    #[test]
    fn parts_are_scaled_to_their_slots() {
        assert!(filter(&args(&join(), Encoder::Gpu)).contains("scale=2556:1434"));
    }

    #[test]
    fn the_joined_video_has_the_planned_frame_rate() {
        assert!(filter(&args(&join(), Encoder::Gpu)).contains("fps=60"));
    }

    #[test]
    fn the_first_parts_sound_is_kept() {
        let with_sound = Join {
            audio: true,
            ..join()
        };
        assert!(has(&args(&with_sound, Encoder::Gpu), &["-map", "0:a"]));
    }

    #[test]
    fn without_sound_no_audio_is_mapped() {
        assert!(!args(&join(), Encoder::Gpu).iter().any(|a| a == "0:a"));
    }

    #[test]
    fn the_gpu_encodes_with_nvenc() {
        assert!(has(&args(&join(), Encoder::Gpu), &["-c:v", "av1_nvenc"]));
    }

    #[test]
    fn without_the_gpu_software_encodes() {
        assert!(has(&args(&join(), Encoder::Cpu), &["-c:v", "libsvtav1"]));
    }

    #[test]
    fn the_output_comes_last() {
        assert_eq!(
            args(&join(), Encoder::Gpu).last().map(String::as_str),
            Some("/v/a.mp4")
        );
    }
}
