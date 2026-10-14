//! Rectangle coverage: whether windows hide an output completely.
//!
//! Platform-neutral so it can be tested anywhere; the Windows presenter
//! feeds it window rectangles.

/// A rectangle in screen pixels, right and bottom exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Visible area up to a strip this many pixels wide along the output's
/// longer edge does not count: frame bounds can be off by a pixel or two, and
/// a frozen frame behind a few pixels is not noticeable. Area, not piece
/// size, because subtraction splits one visible region into many pieces.
const SLIVER: i64 = 2;

impl Rect {
    fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    fn intersect(&self, other: &Rect) -> Rect {
        Rect {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }

    /// `self` minus `cut`, as up to four disjoint rectangles.
    fn subtract(&self, cut: &Rect) -> Vec<Rect> {
        let overlap = self.intersect(cut);
        if overlap.is_empty() {
            return vec![*self];
        }
        let pieces = [
            // Above and below the overlap, full width.
            Rect {
                bottom: overlap.top,
                ..*self
            },
            Rect {
                top: overlap.bottom,
                ..*self
            },
            // Left and right of it, overlap height.
            Rect {
                top: overlap.top,
                bottom: overlap.bottom,
                right: overlap.left,
                ..*self
            },
            Rect {
                top: overlap.top,
                bottom: overlap.bottom,
                left: overlap.right,
                ..*self
            },
        ];
        pieces.into_iter().filter(|r| !r.is_empty()).collect()
    }

    fn area(&self) -> i64 {
        i64::from(self.right - self.left) * i64::from(self.bottom - self.top)
    }
}

/// Whether `covers` (in any order) leave no more of `target` visible than a
/// sliver.
pub fn fully_covered(target: Rect, covers: &[Rect]) -> bool {
    let mut visible = vec![target];
    for cover in covers {
        visible = visible.iter().flat_map(|r| r.subtract(cover)).collect();
        if visible.is_empty() {
            return true;
        }
    }
    let longer = (target.right - target.left).max(target.bottom - target.top);
    visible.iter().map(Rect::area).sum::<i64>() <= SLIVER * i64::from(longer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    const MONITOR: Rect = Rect {
        left: 0,
        top: 0,
        right: 2560,
        bottom: 1440,
    };

    #[test]
    fn nothing_covers() {
        assert!(!fully_covered(MONITOR, &[]));
    }

    #[test]
    fn fullscreen_window_covers() {
        assert!(fully_covered(MONITOR, &[MONITOR]));
        assert!(fully_covered(MONITOR, &[rect(-10, -10, 3000, 2000)]));
    }

    #[test]
    fn maximized_window_and_taskbar_cover() {
        let work_area = rect(0, 0, 2560, 1392);
        let taskbar = rect(0, 1392, 2560, 1440);
        assert!(fully_covered(MONITOR, &[work_area, taskbar]));
        assert!(!fully_covered(MONITOR, &[work_area]));
    }

    #[test]
    fn two_snapped_halves_cover() {
        assert!(fully_covered(
            MONITOR,
            &[rect(0, 0, 1280, 1440), rect(1280, 0, 2560, 1440)]
        ));
    }

    #[test]
    fn a_gap_between_windows_stays_visible() {
        assert!(!fully_covered(
            MONITOR,
            &[rect(0, 0, 1270, 1440), rect(1290, 0, 2560, 1440)]
        ));
    }

    #[test]
    fn slivers_do_not_count() {
        // Frame bounds that miss the edges by a pixel or two.
        assert!(fully_covered(MONITOR, &[rect(0, 0, 2560, 1438)]));
        assert!(fully_covered(MONITOR, &[rect(1, 0, 2560, 1439)]));
        assert!(!fully_covered(MONITOR, &[rect(0, 0, 2560, 1436)]));
    }

    #[test]
    fn many_windows_do_not_hide_a_visible_square() {
        // Windows whose edges slice through a 140 × 140 visible area split
        // it into small pieces; it still counts as visible.
        let mut covers = vec![rect(0, 0, 2560, 600), rect(0, 740, 2560, 1440)];
        covers.push(rect(0, 600, 1000, 740));
        covers.push(rect(1140, 600, 2560, 740));
        for x in (1000..1140).step_by(10) {
            covers.push(rect(x, 0, x + 1, 1));
        }
        assert!(!fully_covered(MONITOR, &covers));
    }

    #[test]
    fn windows_on_other_monitors_do_not_cover() {
        assert!(!fully_covered(MONITOR, &[rect(-1080, 0, 0, 1920)]));
    }

    #[test]
    fn a_hole_in_the_middle_stays_visible() {
        // Four windows around a 100 × 100 hole.
        let covers = [
            rect(0, 0, 2560, 600),
            rect(0, 700, 2560, 1440),
            rect(0, 600, 1200, 700),
            rect(1300, 600, 2560, 700),
        ];
        assert!(!fully_covered(MONITOR, &covers));
    }
}
