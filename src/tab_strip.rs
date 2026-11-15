//! Layout arithmetic of the scrolling tab strip: which target tabs fit when they don't all fit into the tab bar.

/// Number of tabs from `first` on that fit side by side (with `gap` between them) into `strip` points. At least one
/// if any remain, so the strip never stays empty however narrow the window is.
pub fn fitting(widths: &[f32], gap: f32, strip: f32, first: usize) -> usize {
    let mut used = 0.0;
    let mut count = 0_usize;
    for width in widths.iter().skip(first) {
        let next = if count == 0 { *width } else { used + gap + width };
        if count > 0 && next > strip {
            break;
        }
        used = next;
        count = count.saturating_add(1);
    }
    count
}

/// The first tab to show, starting from `first`: moved so that tab `reveal` (if given) is in view, and back as far
/// as the tabs still fit, so no room is left empty at the end of the strip.
pub fn first_visible(widths: &[f32], gap: f32, strip: f32, first: usize, reveal: Option<usize>) -> usize {
    let len = widths.len();
    let mut first = first.min(len.saturating_sub(1));
    if let Some(reveal) = reveal.filter(|reveal| *reveal < len) {
        first = first.min(reveal);
        while first < reveal && first.saturating_add(fitting(widths, gap, strip, first)) <= reveal {
            first = first.saturating_add(1);
        }
    }
    while let Some(earlier) = first.checked_sub(1)
        && earlier.saturating_add(fitting(widths, gap, strip, earlier)) >= len
    {
        first = earlier;
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five tabs of 100 points.
    const WIDTHS: [f32; 5] = [100.0; 5];

    #[test]
    fn counts_the_tabs_that_fit() {
        assert_eq!(fitting(&WIDTHS, 4.0, 310.0, 0), 3);
        assert_eq!(fitting(&WIDTHS, 4.0, 300.0, 0), 2, "gaps take room too");
        assert_eq!(fitting(&WIDTHS, 4.0, 1_000.0, 3), 2);
        assert_eq!(fitting(&WIDTHS, 4.0, 10.0, 1), 1, "one tab even if it doesn't fit");
    }

    #[test]
    fn scrolls_to_reveal_a_tab() {
        // Three fit: revealing the last one starts the strip at the third.
        assert_eq!(first_visible(&WIDTHS, 4.0, 310.0, 0, Some(4)), 2);
        assert_eq!(first_visible(&WIDTHS, 4.0, 310.0, 2, Some(0)), 0);
        assert_eq!(first_visible(&WIDTHS, 4.0, 310.0, 1, Some(2)), 1, "already in view");
    }

    #[test]
    fn leaves_no_empty_room_at_the_end() {
        assert_eq!(first_visible(&WIDTHS, 4.0, 310.0, 4, None), 2);
        assert_eq!(first_visible(&WIDTHS, 4.0, 1_000.0, 3, None), 0, "all fit");
        assert_eq!(first_visible(&[], 4.0, 310.0, 3, None), 0);
    }
}
