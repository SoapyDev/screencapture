//! Keep the resident daemon's memory flat across captures.
//!
//! Every capture allocates one buffer per screen (~15 MB at 2560×1440) and frees it when the
//! overlay closes. glibc serves such sizes with `mmap` at first, but after the first one is
//! freed it raises its mmap threshold (up to 32 MB) and serves the next ones from heap arenas,
//! which it keeps: the daemon grew by a full set of screens per capture.

/// Pin glibc's mmap threshold so screen-sized buffers are always mapped, and unmapped when
/// freed. Measured: no cost to capture latency (fresh pages come as transparent huge pages).
pub fn tune() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: plain malloc parameter changes, before any thread that could race them is busy.
    unsafe {
        // Setting either disables glibc's dynamic adjustment of both.
        libc::mallopt(libc::M_MMAP_THRESHOLD, 1 << 20);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 4 << 20);
    }
}

/// Hand memory freed by an overlay (heap fragments from drawing, text, layout) back to the OS.
pub fn release() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: thread-safe glibc call with no preconditions.
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
    const fn is_mmapped(buffer: &[u8]) -> bool {
        // SAFETY: every malloc chunk is preceded by its (aligned) size word.
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
        tune();
        // What used to raise glibc's threshold past a screen's size.
        drop(std::hint::black_box(vec![1_u8; 32 << 20]));
        let screen = std::hint::black_box(vec![1_u8; 2560 * 1440 * 4]);
        assert!(is_mmapped(&screen));
    }

    #[test]
    fn trimming_is_harmless() {
        let small: Vec<Box<[u8; 4096]>> = (0..256).map(|_| Box::new([7; 4096])).collect();
        drop(small);
        release();
        assert_eq!(std::hint::black_box(vec![7_u8; 4096])[4095], 7);
    }
}
