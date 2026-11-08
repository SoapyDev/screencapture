//! Put each capture on the clipboard as `image/png`.
//!
//! On Wayland a clipboard entry lives as long as the program offering it. The daemon outlives
//! every capture, so it offers the image itself. A one-off run exits right after saving, so it
//! hands the image to `wl-copy` (or `xclip` on X11), which stays behind to serve it.

use std::{
    io::Write,
    process::{Command, Stdio},
};

#[cfg(all(target_os = "linux", feature = "wayland"))]
pub use native::Server;

/// The daemon's own clipboard, once [`serve`] started it.
#[cfg(all(target_os = "linux", feature = "wayland"))]
static SERVER: std::sync::OnceLock<Server> = std::sync::OnceLock::new();

/// For a process that stays up (the daemon): offer copied images from here, without a helper.
#[cfg_attr(
    not(all(target_os = "linux", feature = "wayland")),
    expect(clippy::missing_const_for_fn, reason = "empty without the native clipboard")
)]
pub fn serve() {
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    match Server::start() {
        Ok(server) => {
            let _ = SERVER.set(server);
        }
        Err(e) => eprintln!("screencapture: clipboard: {e:#}; will use wl-copy instead"),
    }
}

/// Make `png` the clipboard's content. Failures are reported, never fatal: the file is saved.
#[cfg_attr(
    not(all(target_os = "linux", feature = "wayland")),
    expect(clippy::needless_pass_by_value, reason = "the native clipboard keeps it")
)]
pub fn copy_png(png: Vec<u8>) {
    // Test runs must not clobber the user's clipboard.
    if std::env::var_os("SCREENCAPTURE_NO_CLIPBOARD").is_some() {
        return;
    }
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    if let Some(server) = SERVER.get() {
        server.offer(png);
        return;
    }
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();
    let Some((program, args)) = command_for(wayland, x11) else {
        return;
    };
    if let Err(e) = pipe_to(program, args, &png) {
        eprintln!("screencapture: could not copy to the clipboard ({program}: {e})");
    }
}

/// The helper that keeps a copied image on offer after this process exits.
const fn command_for(wayland: bool, x11: bool) -> Option<(&'static str, &'static [&'static str])> {
    if wayland {
        Some(("wl-copy", &["--type", "image/png"]))
    } else if x11 {
        Some((
            "xclip",
            &["-selection", "clipboard", "-target", "image/png", "-in"],
        ))
    } else {
        None
    }
}

/// Run `program` with `bytes` on its stdin. Both helpers fork into the background once they
/// have read everything, so waiting only covers the read.
fn pipe_to(program: &str, args: &[&str], bytes: &[u8]) -> std::io::Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        // The background half keeps these open: inherited, they would hold our caller's pipes.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Dropped right after writing: EOF tells the helper the image is complete.
    let written = child.stdin.take().expect("stdin is piped").write_all(bytes);
    let status = child.wait()?;
    written?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("exited with {status}")))
    }
}

/// `zwlr_data_control_v1`: own the clipboard without a window or input focus.
#[cfg(all(target_os = "linux", feature = "wayland"))]
mod native {
    use std::{fs::File, io::Write, sync::Arc};

    use anyhow::{Context, Result};
    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{wl_registry, wl_seat},
    };
    use wayland_protocols_wlr::data_control::v1::client::{
        zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
        zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
        zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
    };

    const PNG: &str = "image/png";

    /// Offers the latest copied image; a dispatch thread answers paste requests.
    pub struct Server {
        conn: Connection,
        qh: QueueHandle<State>,
        manager: ZwlrDataControlManagerV1,
        device: ZwlrDataControlDeviceV1,
    }

    struct State;

    impl Server {
        /// Connect to the compositor's clipboard and serve it from a thread of its own.
        ///
        /// # Errors
        ///
        /// When Wayland can't be reached, lacks data control or a seat, or the thread can't start.
        pub fn start() -> Result<Self> {
            let conn = Connection::connect_to_env().context("connecting to Wayland")?;
            let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
            let qh = queue.handle();
            let manager: ZwlrDataControlManagerV1 = globals
                .bind(&qh, 1..=2, ())
                .context("compositor lacks zwlr_data_control_manager_v1")?;
            let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).context("no wl_seat")?;
            let device = manager.get_data_device(&seat, &qh, ());
            std::thread::Builder::new()
                .name("clipboard".into())
                .spawn(move || while queue.blocking_dispatch(&mut State).is_ok() {})?;
            Ok(Self {
                conn,
                qh,
                manager,
                device,
            })
        }

        /// Replace the clipboard with `png`. The previous image is freed once the compositor
        /// cancels its offer.
        pub fn offer(&self, png: Vec<u8>) {
            let source = self.manager.create_data_source(&self.qh, Arc::<[u8]>::from(png));
            source.offer(PNG.into());
            self.device.set_selection(Some(&source));
            if let Err(e) = self.conn.flush() {
                eprintln!("screencapture: clipboard: {e}");
            }
        }
    }

    impl Dispatch<ZwlrDataControlSourceV1, Arc<[u8]>> for State {
        fn event(
            _: &mut Self,
            source: &ZwlrDataControlSourceV1,
            event: zwlr_data_control_source_v1::Event,
            png: &Arc<[u8]>,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                zwlr_data_control_source_v1::Event::Send { mime_type, fd } if mime_type == PNG => {
                    // A slow reader must not hold up the next paste or offer.
                    let png = png.clone();
                    std::thread::spawn(move || {
                        let _ = File::from(fd).write_all(&png);
                    });
                }
                zwlr_data_control_source_v1::Event::Cancelled => source.destroy(),
                // Other types: dropping the fd closes it, an empty answer.
                _ => {}
            }
        }
    }

    impl Dispatch<ZwlrDataControlDeviceV1, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZwlrDataControlDeviceV1,
            event: zwlr_data_control_device_v1::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            // Other programs' clipboards are announced to us too; never read, just released.
            match event {
                zwlr_data_control_device_v1::Event::Selection { id: Some(offer) }
                | zwlr_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
                _ => {}
            }
        }

        wayland_client::event_created_child!(State, ZwlrDataControlDeviceV1, [
            zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
        ]);
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

    wayland_client::delegate_noop!(State: ZwlrDataControlManagerV1);
    wayland_client::delegate_noop!(State: ignore ZwlrDataControlOfferV1);
    wayland_client::delegate_noop!(State: ignore wl_seat::WlSeat);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_copier_follows_the_session() {
        assert_eq!(
            command_for(true, true),
            Some(("wl-copy", &["--type", "image/png"][..]))
        );
        assert_eq!(
            command_for(false, true),
            Some((
                "xclip",
                &["-selection", "clipboard", "-target", "image/png", "-in"][..]
            ))
        );
        assert_eq!(command_for(false, false), None);
    }

    #[test]
    fn the_external_copier_gets_the_png_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("got");
        let script = format!("cat > '{}'", out.display());
        pipe_to("sh", &["-c", &script], b"\x89PNG fake").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"\x89PNG fake");
        assert!(pipe_to("screencapture-no-such-copier", &[], b"x").is_err());
    }

    #[cfg(all(target_os = "linux", feature = "wayland"))]
    #[test]
    #[ignore = "replaces your clipboard; needs a wlroots session and wl-paste"]
    fn the_daemon_serves_the_png_until_replaced() {
        let png: Vec<u8> = (0..300_000)
            .map(|i| u8::try_from(i % 251).expect("a remainder of 251 fits a byte"))
            .collect();
        let server = Server::start().unwrap();
        server.offer(png.clone());
        // Give the compositor the new selection before asking for it.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let types = std::process::Command::new("wl-paste")
            .arg("--list-types")
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&types.stdout)
                .lines()
                .any(|t| t == "image/png")
        );
        for _ in 0..2 {
            let pasted = std::process::Command::new("wl-paste")
                .args(["--type", "image/png"])
                .output()
                .unwrap();
            assert_eq!(pasted.stdout, png);
        }
    }
}
