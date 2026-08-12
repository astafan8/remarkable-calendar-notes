//! Pure geometry: mapping a [`ViewMode`] and an anchor date onto a grid of
//! per-date rectangles within a canvas of a given pixel size. No rendering
//! or I/O happens here, which is what makes it exhaustively unit-testable
//! without any device.

use crate::model::ViewMode;
use crate::recurrence::Window;
use chrono::{Datelike, Duration, NaiveDate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateCell {
    pub date: NaiveDate,
    pub rect: Rect,
    /// `false` for Month view's leading/trailing days that belong to an
    /// adjacent month; always `true` for every other view.
    pub in_focus_period: bool,
}

/// The Monday on or before `date` — this app always starts weeks on
/// Monday. (Not currently user-configurable; see docs/LIMITATIONS.md.)
fn week_start(date: NaiveDate) -> NaiveDate {
    date - Duration::days(date.weekday().num_days_from_monday() as i64)
}

/// Lay out `view` anchored at `anchor` within a `canvas_w`x`canvas_h` pixel
/// area. Returns one [`DateCell`] per grid cell, row-major.
pub fn layout(view: ViewMode, anchor: NaiveDate, canvas_w: i32, canvas_h: i32) -> Vec<DateCell> {
    match view {
        ViewMode::Day => vec![DateCell {
            date: anchor,
            rect: Rect {
                x: 0,
                y: 0,
                w: canvas_w,
                h: canvas_h,
            },
            in_focus_period: true,
        }],
        ViewMode::Week => grid_of_days(week_start(anchor), 7, 3, canvas_w, canvas_h),
        ViewMode::WorkWeek => grid_of_days(week_start(anchor), 5, 3, canvas_w, canvas_h),
        ViewMode::TwoWeeks => grid_of_days(week_start(anchor), 14, 4, canvas_w, canvas_h),
        ViewMode::Month => month_grid(anchor, canvas_w, canvas_h),
    }
}

fn grid_of_days(
    start: NaiveDate,
    count: i32,
    columns: i32,
    canvas_w: i32,
    canvas_h: i32,
) -> Vec<DateCell> {
    let mut cells = Vec::with_capacity(count as usize);
    let rows = (count + columns - 1) / columns;
    let base_w = canvas_w / columns;
    let base_h = canvas_h / rows;
    for i in 0..count {
        let column = i % columns;
        let row = i / columns;
        let items_in_row = if row == rows - 1 {
            count - row * columns
        } else {
            columns
        };
        let row_offset = if items_in_row < columns {
            (columns - items_in_row) * base_w / 2
        } else {
            0
        };
        let w = if column == columns - 1 {
            canvas_w - base_w * (columns - 1)
        } else {
            base_w
        };
        let h = if row == rows - 1 {
            canvas_h - base_h * (rows - 1)
        } else {
            base_h
        };
        cells.push(DateCell {
            date: start + Duration::days(i as i64),
            rect: Rect {
                x: row_offset + column * base_w,
                y: row * base_h,
                w,
                h,
            },
            in_focus_period: true,
        });
    }
    cells
}

/// A fixed 6-row-by-7-column grid (the standard month-view shape), including
/// the leading days from the previous month and trailing days from the next
/// month needed to fill whole weeks.
fn month_grid(anchor: NaiveDate, canvas_w: i32, canvas_h: i32) -> Vec<DateCell> {
    let first_of_month = anchor.with_day(1).unwrap();
    let grid_start = week_start(first_of_month);
    const ROWS: i32 = 6;
    const COLS: i32 = 7;
    let base_w = canvas_w / COLS;
    let base_h = canvas_h / ROWS;
    let mut cells = Vec::with_capacity((ROWS * COLS) as usize);
    for row in 0..ROWS {
        let h = if row == ROWS - 1 {
            canvas_h - base_h * (ROWS - 1)
        } else {
            base_h
        };
        let y = base_h * row;
        for col in 0..COLS {
            let w = if col == COLS - 1 {
                canvas_w - base_w * (COLS - 1)
            } else {
                base_w
            };
            let x = base_w * col;
            let date = grid_start + Duration::days((row * COLS + col) as i64);
            cells.push(DateCell {
                date,
                rect: Rect { x, y, w, h },
                in_focus_period: date.month() == anchor.month(),
            });
        }
    }
    cells
}

/// The half-open date range a view's grid actually covers for `anchor`,
/// matching exactly what [`layout`] would draw — used to bound how far
/// out calendar sources need to expand recurring events for the current
/// screen.
pub fn window_for(view: ViewMode, anchor: NaiveDate) -> Window {
    match view {
        ViewMode::Day => Window {
            start: anchor,
            end: anchor + Duration::days(1),
        },
        ViewMode::Week => {
            let start = week_start(anchor);
            Window {
                start,
                end: start + Duration::days(7),
            }
        }
        ViewMode::WorkWeek => {
            let start = week_start(anchor);
            Window {
                start,
                end: start + Duration::days(5),
            }
        }
        ViewMode::TwoWeeks => {
            let start = week_start(anchor);
            Window {
                start,
                end: start + Duration::days(14),
            }
        }
        ViewMode::Month => {
            let first_of_month = anchor.with_day(1).unwrap();
            let start = week_start(first_of_month);
            Window {
                start,
                end: start + Duration::days(42),
            }
        }
    }
}

/// Move `anchor` forward (`delta = 1`) or backward (`delta = -1`) by one
/// page of `view` (a day, a week, two weeks, or a month).
pub fn navigate(view: ViewMode, anchor: NaiveDate, delta: i32) -> NaiveDate {
    match view {
        ViewMode::Day => anchor + Duration::days(delta as i64),
        ViewMode::Week | ViewMode::WorkWeek => anchor + Duration::days(7 * delta as i64),
        ViewMode::TwoWeeks => anchor + Duration::days(14 * delta as i64),
        ViewMode::Month => {
            let total = anchor.year() * 12 + (anchor.month() as i32 - 1) + delta;
            let year = total.div_euclid(12);
            let month = (total.rem_euclid(12) + 1) as u32;
            let day = anchor.day().min(days_in_month(year, month));
            NaiveDate::from_ymd_opt(year, month, day).unwrap()
        }
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .unwrap()
        .pred_opt()
        .unwrap()
        .day()
}

/// Map a display point (e.g. a raw pen/touch coordinate) to the [`DateCell`]
/// containing it, if any.
pub fn cell_at(cells: &[DateCell], px: i32, py: i32) -> Option<&DateCell> {
    cells.iter().find(|c| {
        px >= c.rect.x && px < c.rect.x + c.rect.w && py >= c.rect.y && py < c.rect.y + c.rect.h
    })
}

/// The largest centered 3:4 writing surface that fits inside `rect`.
///
/// Every calendar view uses this same canonical aspect ratio for ink.
/// Cells may letterbox a little, but handwriting is never stretched
/// independently on the X and Y axes when changing views.
pub fn ink_rect(rect: Rect) -> Rect {
    const ASPECT_W: i32 = 3;
    const ASPECT_H: i32 = 4;
    if rect.w * ASPECT_H > rect.h * ASPECT_W {
        let w = (rect.h * ASPECT_W / ASPECT_H).max(1);
        Rect {
            x: rect.x + (rect.w - w) / 2,
            y: rect.y,
            w,
            h: rect.h.max(1),
        }
    } else {
        let h = (rect.w * ASPECT_H / ASPECT_W).max(1);
        Rect {
            x: rect.x,
            y: rect.y + (rect.h - h) / 2,
            w: rect.w.max(1),
            h,
        }
    }
}

/// Convert an absolute point into canonical ink coordinates normalized
/// `[0,1]`, clamping points in the letterboxed margin to the writing
/// surface's nearest edge.
pub fn normalize_within(rect: Rect, px: i32, py: i32) -> (f32, f32) {
    let rect = ink_rect(rect);
    let nx = ((px - rect.x) as f32 / rect.w.max(1) as f32).clamp(0.0, 1.0);
    let ny = ((py - rect.y) as f32 / rect.h.max(1) as f32).clamp(0.0, 1.0);
    (nx, ny)
}

/// Inverse of [`normalize_within`]: map a normalized point back to absolute
/// pixel coordinates within `rect`'s canonical writing surface.
pub fn denormalize_within(rect: Rect, nx: f32, ny: f32) -> (i32, i32) {
    let rect = ink_rect(rect);
    let px = rect.x + (nx.clamp(0.0, 1.0) * rect.w as f32).round() as i32;
    let py = rect.y + (ny.clamp(0.0, 1.0) * rect.h as f32).round() as i32;
    (px, py)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Weekday;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn day_view_is_a_single_full_canvas_cell() {
        let cells = layout(ViewMode::Day, d(2026, 3, 15), 1404, 1872);
        assert_eq!(cells.len(), 1);
        assert_eq!(
            cells[0].rect,
            Rect {
                x: 0,
                y: 0,
                w: 1404,
                h: 1872
            }
        );
    }

    #[test]
    fn week_view_has_seven_cells_starting_monday_in_a_compact_grid() {
        // 2026-03-18 is a Wednesday.
        let cells = layout(ViewMode::Week, d(2026, 3, 18), 1400, 200);
        assert_eq!(cells.len(), 7);
        assert_eq!(cells[0].date, d(2026, 3, 16)); // Monday
        assert_eq!(cells[6].date, d(2026, 3, 22)); // Sunday
        assert_eq!(cells[0].rect.y, 0);
        assert_eq!(cells[3].rect.y, 200 / 3);
        assert_eq!(cells[6].rect.y, 2 * (200 / 3));
    }

    #[test]
    fn work_week_view_has_five_cells_monday_to_friday() {
        let cells = layout(ViewMode::WorkWeek, d(2026, 3, 18), 1400, 200);
        assert_eq!(cells.len(), 5);
        assert_eq!(cells[4].date.weekday(), Weekday::Fri);
        assert_eq!(cells[3].rect.y, 100);
    }

    #[test]
    fn two_weeks_view_has_fourteen_cells_in_a_four_column_grid() {
        let cells = layout(ViewMode::TwoWeeks, d(2026, 3, 18), 1400, 400);
        assert_eq!(cells.len(), 14);
        assert_eq!(cells[0].rect.y, 0);
        assert_eq!(cells[4].rect.y, 100);
        assert_eq!(cells[12].rect.y, 300);
        assert_eq!(cells[13].date, cells[0].date + Duration::days(13));
    }

    #[test]
    fn month_view_is_a_6x7_grid_covering_the_whole_month() {
        let cells = layout(ViewMode::Month, d(2026, 3, 15), 1400, 1800);
        assert_eq!(cells.len(), 42);
        let in_focus: Vec<_> = cells.iter().filter(|c| c.in_focus_period).collect();
        assert_eq!(in_focus.len(), 31); // March has 31 days
        assert!(in_focus.iter().any(|c| c.date == d(2026, 3, 1)));
        assert!(in_focus.iter().any(|c| c.date == d(2026, 3, 31)));
    }

    #[test]
    fn month_view_march_2026_starts_on_a_sunday_so_grid_leads_with_february() {
        // March 1, 2026 is a Sunday, so the grid's first cell is the Monday
        // of the prior week (Feb 23).
        let cells = layout(ViewMode::Month, d(2026, 3, 15), 700, 600);
        assert_eq!(cells[0].date, d(2026, 2, 23));
        assert!(!cells[0].in_focus_period);
    }

    #[test]
    fn cell_at_finds_the_containing_cell_and_normalizes_a_point_within_it() {
        let cells = layout(ViewMode::Week, d(2026, 3, 18), 1400, 200);
        let cell = cell_at(&cells, 250, 30).unwrap();
        assert_eq!(cell.date, d(2026, 3, 16));
        let (nx, ny) = normalize_within(cell.rect, 250, 30);
        assert!((0.0..=1.0).contains(&nx));
        assert!((0.0..=1.0).contains(&ny));
        let (px, py) = denormalize_within(cell.rect, nx, ny);
        assert!((px - 250).abs() <= 1);
        assert!((py - 30).abs() <= 1);
    }

    #[test]
    fn ink_mapping_preserves_one_aspect_ratio_in_differently_shaped_cells() {
        let wide = Rect {
            x: 0,
            y: 0,
            w: 1200,
            h: 800,
        };
        let tall = Rect {
            x: 0,
            y: 0,
            w: 300,
            h: 900,
        };
        let wide_ink = ink_rect(wide);
        let tall_ink = ink_rect(tall);
        assert_eq!(wide_ink.w * 4, wide_ink.h * 3);
        assert_eq!(tall_ink.w * 4, tall_ink.h * 3);

        let wide_dx = denormalize_within(wide, 1.0, 0.5).0 - denormalize_within(wide, 0.0, 0.5).0;
        let wide_dy = denormalize_within(wide, 0.5, 1.0).1 - denormalize_within(wide, 0.5, 0.0).1;
        let tall_dx = denormalize_within(tall, 1.0, 0.5).0 - denormalize_within(tall, 0.0, 0.5).0;
        let tall_dy = denormalize_within(tall, 0.5, 1.0).1 - denormalize_within(tall, 0.5, 0.0).1;
        assert_eq!(wide_dx * 4, wide_dy * 3);
        assert_eq!(tall_dx * 4, tall_dy * 3);
    }

    #[test]
    fn window_for_month_matches_the_layout_grid_span() {
        let anchor = d(2026, 3, 15);
        let window = window_for(ViewMode::Month, anchor);
        let cells = layout(ViewMode::Month, anchor, 700, 600);
        assert_eq!(window.start, cells[0].date);
        assert_eq!(window.end, cells[41].date + Duration::days(1));
    }

    #[test]
    fn navigate_month_clamps_day_when_target_month_is_shorter() {
        // Jan 31 + 1 month should land on the last day of February, not
        // roll over into March.
        let next = navigate(ViewMode::Month, d(2026, 1, 31), 1);
        assert_eq!(next, d(2026, 2, 28));
    }

    #[test]
    fn navigate_week_moves_by_seven_days() {
        let next = navigate(ViewMode::Week, d(2026, 3, 18), 1);
        assert_eq!(next, d(2026, 3, 25));
        let prev = navigate(ViewMode::Week, d(2026, 3, 18), -1);
        assert_eq!(prev, d(2026, 3, 11));
    }
}
