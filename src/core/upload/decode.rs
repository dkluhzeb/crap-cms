//! Decoding and processing images under process-wide bounds.
//!
//! Every image decoder is built here, with an explicit allocation limit, and
//! every decode + resize + encode runs holding one of a fixed number of
//! processing slots. A decoded image costs up to hundreds of megabytes and
//! seconds of CPU, so without the slots a burst of uploads (or queued
//! conversions) could exhaust the host's memory and starve every other
//! request; with them, the excess waits its turn.

use std::{
    error::Error,
    fmt,
    io::Cursor,
    num::NonZeroUsize,
    sync::{Condvar, Mutex, OnceLock, PoisonError},
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result};
use image::{DynamicImage, ImageReader, Limits};

/// The most pixels an image may declare — the decompression-bomb cap.
pub(super) const MAX_IMAGE_PIXELS: u64 = 100_000_000;

/// The most bytes one decoder may allocate: an 8-bit RGBA image at the pixel
/// cap plus working headroom. A 16-bit or floating-point image of that size
/// needs two to four times as much and is refused before its pixels are read.
const MAX_DECODE_ALLOC: u64 = MAX_IMAGE_PIXELS * 4 + 64 * 1024 * 1024;

/// How long image work waits for a free processing slot before giving up.
const SLOT_WAIT: Duration = Duration::from_mins(1);

/// A reader over `data` with its format guessed and the decode limits applied
/// — the one way an image decoder is constructed.
///
/// # Errors
///
/// Returns an error if the format cannot be detected.
pub(super) fn image_reader(data: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>> {
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .context("Failed to detect image format")?;

    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);

    Ok(reader)
}

/// Decode `data` under the decode limits.
///
/// # Errors
///
/// Returns an error if the image cannot be decoded or needs more memory than
/// the limits allow.
pub(super) fn decode_image(data: &[u8]) -> Result<DynamicImage> {
    image_reader(data)?
        .decode()
        .context("Failed to decode image")
}

/// Every image-processing slot stayed taken for the whole wait. Transient —
/// the same request succeeds once the burst has drained.
#[derive(Debug)]
pub struct ImageProcessingBusy;

impl fmt::Display for ImageProcessingBusy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("image processing is at capacity; try again shortly")
    }
}

impl Error for ImageProcessingBusy {}

/// A counting semaphore for blocking threads — image work runs on the
/// blocking pool, never on an async task.
struct Slots {
    free: Mutex<usize>,
    released: Condvar,
}

/// One held slot, returned on drop.
struct Slot<'a>(&'a Slots);

impl Slots {
    fn new(count: usize) -> Self {
        Self {
            free: Mutex::new(count),
            released: Condvar::new(),
        }
    }

    /// Take a slot, waiting at most `wait` for one to be released.
    fn acquire(&self, wait: Duration) -> Option<Slot<'_>> {
        let free = self.free.lock().unwrap_or_else(PoisonError::into_inner);

        let (mut free, _) = self
            .released
            .wait_timeout_while(free, wait, |free| *free == 0)
            .unwrap_or_else(PoisonError::into_inner);

        if *free == 0 {
            return None;
        }

        *free -= 1;

        Some(Slot(self))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(PoisonError::into_inner) += 1;

        self.0.released.notify_one();
    }
}

/// The configured slot count, installed once when the config is applied.
static CONCURRENCY: OnceLock<usize> = OnceLock::new();

/// The process-wide slots, sized on first use.
static SLOTS: OnceLock<Slots> = OnceLock::new();

/// Half the available CPUs, at least one — image work is CPU-bound, and the
/// other half stays free for requests.
fn default_concurrency() -> usize {
    thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1))
}

/// Install the number of images processed at once (`upload.
/// max_concurrent_image_processing`; `None` = half the CPUs, at least one).
/// The first install wins; it happens at startup, before any image work.
pub fn set_image_concurrency(limit: Option<NonZeroUsize>) {
    let _ = CONCURRENCY.set(limit.map_or_else(default_concurrency, NonZeroUsize::get));
}

/// Run `work` — a decode and whatever is made of the image — holding one of
/// the process-wide processing slots.
///
/// # Errors
///
/// Returns [`ImageProcessingBusy`] when no slot frees up within the wait, and
/// `work`'s own error otherwise.
pub(super) fn with_image_slot<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    let slots = SLOTS.get_or_init(|| Slots::new(*CONCURRENCY.get_or_init(default_concurrency)));

    let Some(_slot) = slots.acquire(SLOT_WAIT) else {
        return Err(ImageProcessingBusy.into());
    };

    work()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use image::ImageError;

    use super::*;

    const SHORT: Duration = Duration::from_millis(20);

    #[test]
    fn slots_run_out_and_come_back() {
        let slots = Slots::new(2);

        let a = slots.acquire(SHORT).expect("first");
        let _b = slots.acquire(SHORT).expect("second");
        assert!(slots.acquire(SHORT).is_none(), "every slot is taken");

        drop(a);
        assert!(
            slots.acquire(SHORT).is_some(),
            "a released slot is reusable"
        );
    }

    /// A waiter is woken by a release instead of timing out.
    #[test]
    fn a_waiter_gets_the_released_slot() {
        let slots = Arc::new(Slots::new(1));
        let held = slots.acquire(SHORT).expect("held");

        let waiter = {
            let slots = Arc::clone(&slots);
            thread::spawn(move || slots.acquire(Duration::from_secs(10)).is_some())
        };

        thread::sleep(SHORT);
        drop(held);

        assert!(waiter.join().unwrap(), "the waiter took the released slot");
    }

    #[test]
    fn the_default_concurrency_is_at_least_one() {
        assert!(default_concurrency() >= 1);
    }

    #[test]
    fn busy_is_its_own_error_type() {
        let err: anyhow::Error = ImageProcessingBusy.into();

        assert!(err.is::<ImageProcessingBusy>());
    }

    /// CRC-32 (IEEE) of `bytes`, for hand-built PNG chunks.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;

        for &byte in bytes {
            crc ^= u32::from(byte);

            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }

        !crc
    }

    /// Append a PNG chunk of `kind` carrying `data`.
    fn push_chunk(png: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
        let mut body = kind.to_vec();
        body.extend(data);

        png.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
        png.extend(&body);
        png.extend(crc32(&body).to_be_bytes());
    }

    /// A PNG that declares `width`×`height` 16-bit RGBA pixels and carries
    /// (almost) no pixel data — only the header a decoder sizes its buffer by.
    fn png_header_only(width: u32, height: u32) -> Vec<u8> {
        let mut ihdr = width.to_be_bytes().to_vec();
        ihdr.extend(height.to_be_bytes());
        ihdr.extend([16, 6, 0, 0, 0]);

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        push_chunk(&mut png, *b"IHDR", &ihdr);
        push_chunk(&mut png, *b"IDAT", &[0x78, 0x9c]);
        push_chunk(&mut png, *b"IEND", &[]);

        png
    }

    /// Regression: decoding used the crate default limits, so a 16-bit image
    /// within the pixel cap allocated past what the cap was sized for. The
    /// explicit allocation limit refuses it before a pixel is read.
    #[test]
    fn a_decode_over_the_allocation_limit_is_refused() {
        let data = png_header_only(10_000, 10_000);

        let err = decode_image(&data).unwrap_err();

        assert!(
            matches!(
                err.downcast_ref::<ImageError>(),
                Some(ImageError::Limits(_))
            ),
            "{err:#}"
        );
    }
}
