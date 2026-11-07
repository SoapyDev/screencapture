//! Portable fallback (X11, GNOME/KDE portals, Windows, macOS) through the `xcap` crate.

use anyhow::{Context, Result};
use xcap::Monitor;

use super::{MonitorInfo, Rect, Shot};

fn info(monitor: &Monitor) -> Result<MonitorInfo> {
    let (x, y) = (monitor.x()?, monitor.y()?);
    Ok(MonitorInfo {
        name: monitor.name()?,
        rect: Rect::new(x, y, monitor.width()? as i32, monitor.height()? as i32),
        output_position: (x, y),
    })
}

pub fn monitors() -> Result<Vec<MonitorInfo>> {
    Monitor::all()
        .context("listing monitors")?
        .iter()
        .map(info)
        .collect()
}

pub fn capture(wanted: &dyn Fn(&MonitorInfo) -> bool) -> Result<Vec<Shot>> {
    let mut shots = Vec::new();
    for monitor in Monitor::all().context("listing monitors")? {
        let info = info(&monitor)?;
        if wanted(&info) {
            let mut image = monitor
                .capture_image()
                .with_context(|| format!("capturing {}", info.name))?;
            // Snapshots hold BGRA (see the `capture` module).
            super::swap_red_blue(&mut image);
            shots.push(Shot {
                name: info.name,
                rect: info.rect,
                output_position: info.output_position,
                image,
            });
        }
    }
    Ok(shots)
}
