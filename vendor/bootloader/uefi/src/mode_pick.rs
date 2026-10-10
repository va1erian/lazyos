//! Choosing a GOP mode from the resolutions the firmware lists.
//!
//! Upstream `bootloader` 0.11.17 takes the *last* listed mode that meets the
//! minimum, which is the largest only when the firmware sorts its list
//! ascending. The MAGICNUC AS1 lists 2560x1440 first and 4:3 modes after it,
//! so the stock rule picked 1280x960 on a 2560x1440 panel. LazyOS takes the
//! largest area instead; on an ascending list the result is unchanged (ties go
//! to the later mode, as before).
//!
//! Pure and `core`-only so it can be tested on the host:
//! `rustc --test --edition 2021 vendor/bootloader/uefi/src/mode_pick.rs`.

/// Index of the mode to set, or `None` when no minimum is configured or no
/// mode meets it (the firmware's current mode is kept, as upstream does).
pub fn pick(
    resolutions: impl Iterator<Item = (usize, usize)>,
    min_width: Option<usize>,
    min_height: Option<usize>,
) -> Option<usize> {
    if min_width.is_none() && min_height.is_none() {
        return None;
    }
    resolutions
        .enumerate()
        .filter(|&(_, (width, height))| {
            min_width.is_none_or(|min| width >= min) && min_height.is_none_or(|min| height >= min)
        })
        .max_by_key(|&(_, (width, height))| width.saturating_mul(height))
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::pick;

    fn run(list: &[(usize, usize)], w: Option<usize>, h: Option<usize>) -> Option<usize> {
        pick(list.iter().copied(), w, h)
    }

    /// The AS1's list, in the order `videoinfo` printed it.
    const AS1: [(usize, usize); 8] = [
        (2560, 1440),
        (640, 480),
        (800, 600),
        (1024, 768),
        (1280, 1024),
        (1400, 1050),
        (1600, 1200),
        (1280, 960),
    ];

    #[test]
    fn as1_gets_its_native_mode_not_the_last_listed() {
        assert_eq!(run(&AS1, Some(1280), Some(720)), Some(0));
    }

    #[test]
    fn ascending_list_still_takes_the_last_largest() {
        let list = [(640, 480), (1280, 720), (1920, 1080), (2560, 1440)];
        assert_eq!(run(&list, Some(1280), Some(720)), Some(3));
    }

    #[test]
    fn equal_areas_go_to_the_later_mode() {
        let list = [(1920, 1080), (1080 * 2, 960)];
        assert_eq!(run(&list, Some(1), Some(1)), Some(1));
    }

    #[test]
    fn minimum_is_respected_on_both_axes() {
        let list = [(4000, 600), (1280, 720)];
        assert_eq!(run(&list, Some(1280), Some(720)), Some(1));
    }

    #[test]
    fn width_only_and_height_only_minimums() {
        assert_eq!(run(&AS1, Some(2000), None), Some(0));
        assert_eq!(run(&AS1, None, Some(1100)), Some(0));
    }

    #[test]
    fn no_minimum_or_no_match_keeps_the_firmware_mode() {
        assert_eq!(run(&AS1, None, None), None);
        assert_eq!(run(&AS1, Some(5000), Some(5000)), None);
        assert_eq!(run(&[], Some(1), Some(1)), None);
    }
}
