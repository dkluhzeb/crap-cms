//! The passes a resize runs, planned from the dimensions alone so the work
//! and memory a size costs are bounded before a single pixel is touched.
//!
//! A filtered resample (`resize_exact`) first resamples vertically into an
//! `f32` buffer as wide as its *input* and as tall as its *output*, then
//! horizontally. Asked to enlarge an extreme-aspect source — a 65535×10 strip
//! covering a 300×300 box, or stretched to fill one — that buffer is millions
//! of pixels wide and hundreds tall, far beyond anything the source or the
//! configured size would suggest. Every plan here keeps each pass's buffers
//! within the larger of the source and the target:
//!
//! - `cover` crops the source to the target's aspect ratio *first*, then
//!   resamples the crop to the target,
//! - `contain` / `inside` scale uniformly, which never widens one axis while
//!   narrowing the other,
//! - any resample that would narrow the image while making it taller first
//!   shrinks the width by area averaging (no intermediate buffer), so the
//!   filtered pass that follows starts at the target width.

use image::{DynamicImage, imageops::FilterType};

use crate::core::upload::{ImageFit, ImageSize};

/// The resampling filter of every filtered pass.
const FILTER: FilterType = FilterType::CatmullRom;

/// One pass of a resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Keep the `width`×`height` region at (`x`, `y`).
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    /// Area-average down to `width`×`height` — both no larger than the input;
    /// allocates only the output.
    Shrink { width: u32, height: u32 },
    /// Filtered resample to `width`×`height`; its intermediate buffer is the
    /// input width × `height`.
    Resample { width: u32, height: u32 },
}

/// `a * b / c`, rounded to nearest, at least 1 and at most `max`.
fn scaled(a: u32, b: u32, c: u32, max: u32) -> u32 {
    let (a, b, c) = (u64::from(a), u64::from(b), u64::from(c.max(1)));
    let value = (a * b + c / 2) / c;

    u32::try_from(value.clamp(1, u64::from(max))).unwrap_or(max)
}

/// The passes taking a `width`×`height` image to `to_w`×`to_h`.
fn resample_steps(width: u32, height: u32, to_w: u32, to_h: u32) -> Vec<Step> {
    if (width, height) == (to_w, to_h) {
        return Vec::new();
    }

    // Narrower but taller: the vertical pass would run over the full input
    // width at the enlarged height. Shrinking the width first bounds it.
    if width > to_w && to_h > height {
        return vec![
            Step::Shrink {
                width: to_w,
                height,
            },
            Step::Resample {
                width: to_w,
                height: to_h,
            },
        ];
    }

    vec![Step::Resample {
        width: to_w,
        height: to_h,
    }]
}

/// `cover`: the centered region of the source with the target's aspect
/// ratio, then that region resampled to the target.
fn cover_steps(width: u32, height: u32, to_w: u32, to_h: u32) -> Vec<Step> {
    let source_wider = u64::from(width) * u64::from(to_h) > u64::from(height) * u64::from(to_w);

    let (crop_w, crop_h) = if source_wider {
        (scaled(height, to_w, to_h, width), height)
    } else {
        (width, scaled(width, to_h, to_w, height))
    };

    let mut steps = Vec::new();

    if (crop_w, crop_h) != (width, height) {
        steps.push(Step::Crop {
            x: (width - crop_w) / 2,
            y: (height - crop_h) / 2,
            width: crop_w,
            height: crop_h,
        });
    }

    steps.extend(resample_steps(crop_w, crop_h, to_w, to_h));

    steps
}

/// `contain` / `inside`: the largest uniform scale of the source that fits
/// the target box.
fn fit_inside_steps(width: u32, height: u32, to_w: u32, to_h: u32) -> Vec<Step> {
    let height_bound = u64::from(width) * u64::from(to_h) <= u64::from(height) * u64::from(to_w);

    let (new_w, new_h) = if height_bound {
        (scaled(width, to_h, height, to_w), to_h)
    } else {
        (to_w, scaled(height, to_w, width, to_h))
    };

    resample_steps(width, height, new_w, new_h)
}

/// The passes resizing a `width`×`height` source to `size`. `None` when the
/// source or the size has a zero dimension.
pub(super) fn plan(width: u32, height: u32, size: &ImageSize) -> Option<Vec<Step>> {
    if width == 0 || height == 0 || size.width == 0 || size.height == 0 {
        return None;
    }

    let (to_w, to_h) = (size.width, size.height);

    Some(match size.fit {
        ImageFit::Cover => cover_steps(width, height, to_w, to_h),
        ImageFit::Contain | ImageFit::Inside => fit_inside_steps(width, height, to_w, to_h),
        ImageFit::Fill => resample_steps(width, height, to_w, to_h),
    })
}

/// One pass over `img`.
fn apply(img: &DynamicImage, step: Step) -> DynamicImage {
    match step {
        Step::Crop {
            x,
            y,
            width,
            height,
        } => img.crop_imm(x, y, width, height),
        Step::Shrink { width, height } => img.thumbnail_exact(width, height),
        Step::Resample { width, height } => img.resize_exact(width, height, FILTER),
    }
}

/// Run `steps` over `img`. The source is borrowed, never copied whole — the
/// first pass reads it in place — unless there is no pass to run.
pub(super) fn run(img: &DynamicImage, steps: &[Step]) -> DynamicImage {
    let Some((first, rest)) = steps.split_first() else {
        return img.clone();
    };

    rest.iter()
        .fold(apply(img, *first), |current, step| apply(&current, *step))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::upload::ImageSizeBuilder;

    fn size(width: u32, height: u32, fit: ImageFit) -> ImageSize {
        ImageSizeBuilder::new("s")
            .width(width)
            .height(height)
            .fit(fit)
            .build()
    }

    /// The pixel count of the largest buffer each pass of `steps` allocates,
    /// starting from `width`×`height`, and the final dimensions.
    fn peak_and_output(width: u32, height: u32, steps: &[Step]) -> (u64, (u32, u32)) {
        let (mut w, mut h) = (width, height);
        let mut peak = 0u64;

        for step in steps {
            let (next_w, next_h, buffer) = match *step {
                Step::Crop { width, height, .. } | Step::Shrink { width, height } => {
                    (width, height, u64::from(width) * u64::from(height))
                }
                Step::Resample { width, height } => {
                    let intermediate = u64::from(w) * u64::from(height);
                    (
                        width,
                        height,
                        intermediate.max(u64::from(width) * u64::from(height)),
                    )
                }
            };

            peak = peak.max(buffer);
            (w, h) = (next_w, next_h);
        }

        (peak, (w, h))
    }

    const ALL_FITS: [ImageFit; 4] = [
        ImageFit::Cover,
        ImageFit::Contain,
        ImageFit::Inside,
        ImageFit::Fill,
    ];

    /// Regression: `cover` resampled the whole source to cover the box before
    /// cropping, so a 65535×10 strip into a 300×300 size asked for a ~1.97M×300
    /// canvas. Every fit mode's buffers now stay within the larger of the
    /// source and the target, for both extreme orientations.
    #[test]
    fn every_fit_is_bounded_by_source_or_target() {
        for (src_w, src_h) in [(65535, 10), (10, 65535), (10_000_000, 10), (10, 10_000_000)] {
            for fit in ALL_FITS {
                let target = size(300, 300, fit.clone());
                let steps = plan(src_w, src_h, &target).unwrap();
                let (peak, _) = peak_and_output(src_w, src_h, &steps);

                let bound = (u64::from(src_w) * u64::from(src_h)).max(300 * 300);
                assert!(
                    peak <= bound,
                    "{src_w}x{src_h} {fit:?}: peak {peak} over {bound} ({steps:?})"
                );
            }
        }
    }

    /// The same with an enlarging size: an intermediate never outgrows the
    /// source or the target.
    #[test]
    fn enlarging_sizes_stay_bounded() {
        for (src_w, src_h) in [(65535, 10), (10, 65535)] {
            for fit in ALL_FITS {
                let target = size(4000, 3000, fit.clone());
                let steps = plan(src_w, src_h, &target).unwrap();
                let (peak, _) = peak_and_output(src_w, src_h, &steps);

                let bound = (u64::from(src_w) * u64::from(src_h)).max(4000 * 3000);
                assert!(peak <= bound, "{src_w}x{src_h} {fit:?}: peak {peak}");
            }
        }
    }

    /// `cover` and `fill` land on the size exactly; `contain` / `inside` fit
    /// within it with one side on the box.
    #[test]
    fn outputs_match_the_fit_mode() {
        for (src_w, src_h) in [(65535, 10), (10, 65535), (400, 200), (200, 400), (300, 300)] {
            for fit in [ImageFit::Cover, ImageFit::Fill] {
                let steps = plan(src_w, src_h, &size(300, 200, fit.clone())).unwrap();
                assert_eq!(
                    peak_and_output(src_w, src_h, &steps).1,
                    (300, 200),
                    "{fit:?}"
                );
            }

            for fit in [ImageFit::Contain, ImageFit::Inside] {
                let steps = plan(src_w, src_h, &size(300, 200, fit.clone())).unwrap();
                let (_, (w, h)) = peak_and_output(src_w, src_h, &steps);

                assert!(w <= 300 && h <= 200, "{src_w}x{src_h} {fit:?}: {w}x{h}");
                assert!(w == 300 || h == 200, "{src_w}x{src_h} {fit:?}: {w}x{h}");
            }
        }
    }

    /// `contain` / `inside` land on exactly the dimensions the image crate's
    /// own fit-inside resize produced before the passes were planned, so an
    /// existing size keeps its dimensions — down- and upscaling, both
    /// orientations, and ratios that round either way.
    #[test]
    fn fit_inside_matches_the_previous_dimensions() {
        let sources = [(1000, 669), (669, 1000), (1234, 567), (7, 3), (300, 301)];
        let boxes = [(300, 200), (200, 300), (1, 1), (1000, 1000)];

        for (src_w, src_h) in sources {
            let img = DynamicImage::new_rgb8(src_w, src_h);

            for (to_w, to_h) in boxes {
                let steps = plan(src_w, src_h, &size(to_w, to_h, ImageFit::Contain)).unwrap();
                let previous = img.resize(to_w, to_h, FILTER);

                assert_eq!(
                    peak_and_output(src_w, src_h, &steps).1,
                    (previous.width(), previous.height()),
                    "{src_w}x{src_h} into {to_w}x{to_h}"
                );
            }
        }
    }

    /// `cover` crops the centered region with the target's aspect ratio
    /// before resampling anything.
    #[test]
    fn cover_crops_first() {
        let steps = plan(65535, 10, &size(300, 300, ImageFit::Cover)).unwrap();

        assert_eq!(
            steps[0],
            Step::Crop {
                x: 32762,
                y: 0,
                width: 10,
                height: 10
            }
        );
    }

    /// A size equal to the source needs no pass at all.
    #[test]
    fn identity_plans_nothing() {
        assert!(
            plan(300, 200, &size(300, 200, ImageFit::Cover))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn zero_dimensions_plan_nothing() {
        assert!(plan(0, 10, &size(10, 10, ImageFit::Cover)).is_none());
        assert!(plan(10, 0, &size(10, 10, ImageFit::Fill)).is_none());
    }

    /// Running the plan on real extreme-aspect sources is cheap and lands on
    /// the planned dimensions.
    #[test]
    fn running_extreme_sources_lands_on_the_size() {
        for (src_w, src_h) in [(65535, 10), (10, 65535)] {
            let img = DynamicImage::new_rgb8(src_w, src_h);

            for fit in ALL_FITS {
                let target = size(300, 300, fit.clone());
                let steps = plan(src_w, src_h, &target).unwrap();
                let out = run(&img, &steps);

                assert_eq!(
                    (out.width(), out.height()),
                    peak_and_output(src_w, src_h, &steps).1,
                    "{src_w}x{src_h} {fit:?}"
                );
            }
        }
    }
}
