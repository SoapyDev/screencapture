//! Keep the resident daemon's memory flat across captures.
//!
//! Every capture allocates one buffer per screen (~15 MB at 2560×1440) and frees it when the
//! overlay closes. glibc serves such sizes with `mmap` at first, but after the first one is
//! freed it raises its mmap threshold (up to 32 MB) and serves the next ones from heap arenas,
//! which it keeps: the daemon grew by a full set of screens per capture.

/// Pin glibc's mmap threshold so screen-sized buffers are always mapped, and unmapped when
/// freed. Measured: no cost to capture latency (fresh pages come as transparent huge pages).
pub fn tune() -> anyhow::Result<()> {
    // Setting either disables glibc's dynamic adjustment of both.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        set_malloc_option(libc::M_MMAP_THRESHOLD, 1 << 20)?;
        set_malloc_option(libc::M_TRIM_THRESHOLD, 4 << 20)?;
    }
    Ok(())
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn set_malloc_option(option: libc::c_int, value: libc::c_int) -> anyhow::Result<()> {
    // SAFETY: `mallopt` has no preconditions: it takes the allocator's lock itself and only
    // changes tuning parameters (glibc 2.44 even reports success for values it ignores). Our
    // values are the two constants above; `glibc_accepts_the_tuning` and the threshold test
    // check their effect.
    let accepted = unsafe { libc::mallopt(option, value) } == 1;
    anyhow::ensure!(accepted, "glibc refused malloc option {option} = {value}");
    Ok(())
}

/// Hand memory freed by an overlay (heap fragments from drawing, text, layout) back to the OS.
pub fn release() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` has no preconditions: it takes the allocator's locks itself and
    // only returns memory that is free. Exercised by `trimming_is_harmless`.
    unsafe {
        libc::malloc_trim(0);
    }
}

// The test reads glibc chunk headers: only meaningful (and only sound) there.
#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    use super::*;

    /// glibc marks chunks it got straight from `mmap` with bit 1 of the size word just before
    /// the pointer (`IS_MMAPPED`); such chunks are unmapped, back to the OS, on free.
    fn is_mmapped(buffer: &[u8]) -> bool {
        // An empty Vec owns no malloc chunk: there would be no header to read.
        assert!(!buffer.is_empty(), "needs a malloc'd buffer");
        // SAFETY: a non-empty Vec<u8> buffer is the start of a malloc chunk (the global
        // allocator is glibc's, and u8 needs no extra alignment), and glibc puts the chunk's
        // size word right before it. Checked against known heap and mapped blocks below.
        let size_word = unsafe {
            buffer
                .as_ptr()
                .sub(size_of::<usize>())
                .cast::<usize>()
                .read_unaligned()
        };
        size_word & 2 != 0
    }

    #[test]
    fn screen_sized_buffers_return_to_the_os_after_bigger_ones_were_freed() {
        tune().unwrap();
        // What used to raise glibc's threshold past a screen's size.
        drop(std::hint::black_box(vec![1_u8; 32 << 20]));
        let screen = std::hint::black_box(vec![1_u8; 2560 * 1440 * 4]);
        assert!(is_mmapped(&screen));
    }

    #[test]
    fn glibc_accepts_the_tuning() {
        assert!(tune().is_ok());
    }

    // The header check the threshold test rests on, against blocks of known origin.
    #[test]
    fn small_blocks_read_as_heap_chunks() {
        let small = std::hint::black_box(vec![1_u8; 1024]);
        assert!(!is_mmapped(&small));
    }

    #[test]
    fn huge_blocks_read_as_mapped_chunks() {
        let huge = std::hint::black_box(vec![1_u8; 64 << 20]);
        assert!(is_mmapped(&huge));
    }

    #[test]
    fn trimming_is_harmless() {
        let small: Vec<Box<[u8; 4096]>> = (0..256).map(|_| Box::new([7; 4096])).collect();
        drop(small);
        release();
        assert_eq!(std::hint::black_box(vec![7_u8; 4096])[4095], 7);
    }
}
