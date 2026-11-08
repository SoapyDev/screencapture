//! How busy the machine is, for the Adaptive frame rate: read once, when a recording starts.

use std::{
    io::Read,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use screencapture_core::units::ToF32;

/// Cumulative CPU time from `/proc/stat`, in ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

/// The aggregate `cpu` line of `/proc/stat`.
#[must_use]
pub fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let ticks: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map_while(|t| t.parse().ok())
        .collect();
    // user nice system idle iowait irq softirq steal; guest time is already inside user.
    let total: u64 = ticks.iter().take(8).sum();
    let idle = ticks.get(3)? + ticks.get(4).unwrap_or(&0);
    Some(CpuTimes {
        busy: total - idle,
        total,
    })
}

/// Share of CPU time spent busy between two readings (0..=1).
#[must_use]
pub fn cpu_load(before: CpuTimes, after: CpuTimes) -> Option<f32> {
    let total = after.total.checked_sub(before.total).filter(|&t| t > 0)?;
    let busy = after.busy.saturating_sub(before.busy);
    Some((busy.to_f32() / total.to_f32()).clamp(0., 1.))
}

/// `nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits`: one percentage per
/// GPU; the busiest counts.
pub fn parse_nvidia_smi(text: &str) -> Option<f32> {
    text.lines()
        .filter_map(|l| l.trim().parse::<f32>().ok())
        .reduce(f32::max)
        .map(|percent| (percent / 100.).clamp(0., 1.))
}

fn read_cpu() -> Option<CpuTimes> {
    parse_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)
}

/// AMD and Intel (i915/xe) drivers expose it directly.
fn sysfs_gpu() -> Option<f32> {
    std::fs::read_dir("/sys/class/drm")
        .ok()?
        .filter_map(|card| std::fs::read_to_string(card.ok()?.path().join("device/gpu_busy_percent")).ok())
        .filter_map(|text| parse_nvidia_smi(&text))
        .reduce(f32::max)
}

/// How long `nvidia-smi` may take: it answers in tens of ms, but a wedged driver can hang it,
/// and a recording must not wait on that.
const NVIDIA_SMI_LIMIT: Duration = Duration::from_millis(500);

fn nvidia_gpu() -> Option<f32> {
    let mut command = Command::new("nvidia-smi");
    command.args(["--query-gpu=utilization.gpu", "--format=csv,noheader,nounits"]);
    parse_nvidia_smi(&output_within(command, NVIDIA_SMI_LIMIT)?)
}

/// How often a command that hasn't finished yet is checked on.
const POLL: Duration = Duration::from_millis(5);

/// What `command` printed, if it succeeds within `limit`; one still running then is killed.
/// Only for commands that print little: the output is read once the command has exited, so
/// one filling the pipe would block until `limit`.
fn output_within(mut command: Command, limit: Duration) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if !exited_within(&mut child, limit)?.success() {
        return None;
    }
    let mut output = String::new();
    child.stdout.take()?.read_to_string(&mut output).ok()?;
    Some(output)
}

/// How `child` exited, if it does within `limit`; otherwise it is killed.
fn exited_within(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// The busier of CPU and GPU over `window` (0..=1), or `None` if neither could be read.
#[must_use]
pub fn sample(window: Duration) -> Option<f32> {
    // nvidia-smi takes tens of ms to answer: ask while the CPU is being measured.
    let gpu = std::thread::spawn(|| nvidia_gpu().or_else(sysfs_gpu));
    let before = read_cpu();
    std::thread::sleep(window);
    let cpu = before.zip(read_cpu()).and_then(|(a, b)| cpu_load(a, b));
    let gpu = gpu.join().ok().flatten();
    match (cpu, gpu) {
        (Some(c), Some(g)) => Some(c.max(g)),
        (one, other) => one.or(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_counts_idle_and_iowait_as_not_busy() {
        let text = "cpu  100 5 50 800 40 3 2 0 0 0\ncpu0 1 2 3 4 5 6 7 8 0 0\nintr 1 2\n";
        assert_eq!(
            parse_proc_stat(text),
            Some(CpuTimes {
                busy: 100 + 5 + 50 + 3 + 2,
                total: 1000
            })
        );
        assert_eq!(parse_proc_stat("intr 1 2"), None);
    }

    #[test]
    fn load_between_two_readings() {
        let a = CpuTimes {
            busy: 100,
            total: 1000,
        };
        let b = CpuTimes {
            busy: 175,
            total: 1100,
        };
        assert_eq!(cpu_load(a, b), Some(0.75));
        assert_eq!(cpu_load(a, a), None, "no time passed");
    }

    #[test]
    fn busiest_gpu_counts() {
        assert_eq!(parse_nvidia_smi("12\n87\n"), Some(0.87));
        assert_eq!(parse_nvidia_smi("[N/A]\n"), None);
        assert_eq!(parse_nvidia_smi(""), None);
    }

    fn shell(script: &str) -> std::process::Command {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn a_quick_command_gives_its_output() {
        assert_eq!(
            output_within(shell("echo 42"), Duration::from_secs(5)).as_deref(),
            Some("42\n")
        );
    }

    #[test]
    fn a_failing_command_gives_nothing() {
        assert_eq!(
            output_within(shell("echo 42; exit 1"), Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn a_hung_command_is_given_up_on_in_time() {
        let started = std::time::Instant::now();
        output_within(shell("sleep 5"), Duration::from_millis(100));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_hung_command_gives_nothing() {
        assert_eq!(output_within(shell("sleep 5"), Duration::from_millis(100)), None);
    }

    #[test]
    #[ignore = "reads this machine's load"]
    fn this_machine_has_a_load() {
        let load = sample(Duration::from_millis(200)).unwrap();
        assert!((0.0..=1.0).contains(&load), "{load}");
    }
}
