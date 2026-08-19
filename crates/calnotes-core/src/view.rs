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
///
/// `aspect` is the target day-cell aspect ratio (width : height). Every
/// view **except Two Months** sizes its day cells to this single ratio, so
/// a handwritten note keeps its shape when you switch between Day, Week,
/// Work Week, Two Weeks, and Month. Because the cells are a fixed shape,
/// some views leave unused space at the bottom (or right) of the canvas
/// rather than stretching cells to fill it. Two Months is the one exception
/// — it keeps filling its grid, so its cells are a slightly different shape.
pub fn layout(
    view: ViewMode,
    anchor: NaiveDate,
    canvas_w: i32,
    canvas_h: i32,
    aspect: CellAspect,
) -> Vec<DateCell> {
    match view {
        ViewMode::Day => {
            let (w, h) = fit_cell(canvas_w, canvas_h, 1, 1, aspect);
            vec![DateCell {
                date: anchor,
                rect: Rect { x: 0, y: 0, w, h },
                in_focus_period: true,
            }]
        }
        ViewMode::Week => weekday_split(week_start(anchor), 1, canvas_w, canvas_h, aspect),
        ViewMode::WorkWeek => work_week_row(week_start(anchor), canvas_w, canvas_h, aspect),
        ViewMode::TwoWeeks => weekday_split(week_start(anchor), 2, canvas_w, canvas_h, aspect),
        ViewMode::Month => month_grid(anchor, canvas_w, canvas_h),
        ViewMode::TwoMonths => two_month_grid(anchor, canvas_w, canvas_h),
    }
}

/// A target day-cell aspect ratio, width : height. The reference is a single
/// Month-view cell (see [`layout`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellAspect {
    pub w: i32,
    pub h: i32,
}

/// The largest cell size with `aspect`'s width:height ratio that fits a
/// `cols` x `rows` grid inside `avail_w` x `avail_h`. The cell is limited by
/// whichever of width or height binds first, so cells are never stretched
/// and the grid simply leaves the leftover space unused.
fn fit_cell(avail_w: i32, avail_h: i32, cols: i32, rows: i32, aspect: CellAspect) -> (i32, i32) {
    let aw = aspect.w.max(1);
    let ah = aspect.h.max(1);
    let width_limited = (avail_w / cols).max(1);
    // The widest cell whose aspect-implied height still fits one row.
    let height_limited = ((avail_h / rows) * aw / ah).max(1);
    let cell_w = width_limited.min(height_limited).max(1);
    let cell_h = (cell_w * ah / aw).max(1);
    (cell_w, cell_h)
}

/// Two consecutive months shown as one **continuous** calendar: weeks flow
/// Monday→Sunday from the first month straight into the second, with no
/// duplicated boundary week. `anchor`'s month and the next month are the two
/// in-focus months; the leading/trailing days needed to fill whole weeks are
/// out of focus. Ink normalization is per-cell over the whole cell (see
/// [`ink_rect`]), so handwriting fills each cell edge to edge.
fn two_month_grid(anchor: NaiveDate, canvas_w: i32, canvas_h: i32) -> Vec<DateCell> {
    let first_month = anchor.with_day(1).unwrap();
    let second_month = first_of_next_month(anchor);
    let second_month_end = last_of_month(second_month);
    let grid_start = week_start(first_month);
    let last_week_start = week_start(second_month_end);
    let rows = ((last_week_start - grid_start).num_days() / 7 + 1) as i32;
    const COLS: i32 = 7;
    let base_w = canvas_w / COLS;
    let base_h = canvas_h / rows;
    let in_month = |d: NaiveDate, m: NaiveDate| d.year() == m.year() && d.month() == m.month();
    let mut cells = Vec::with_capacity((rows * COLS) as usize);
    for row in 0..rows {
        let h = if row == rows - 1 {
            canvas_h - base_h * (rows - 1)
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
                in_focus_period: in_month(date, first_month) || in_month(date, second_month),
            });
        }
    }
    cells
}

/// First day of the month after `date`'s month.
fn first_of_next_month(date: NaiveDate) -> NaiveDate {
    let (year, month) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).unwrap()
}

/// Last day of `date`'s month.
fn last_of_month(date: NaiveDate) -> NaiveDate {
    first_of_next_month(date).pred_opt().unwrap()
}

/// Number of working-day columns (Mon–Fri) that set the cell width for the
/// week-style views, so every day cell is the same width.
const WORK_COLS: i32 = 5;

/// A single row of the five working days (Mon–Fri), each sized to the target
/// cell aspect ratio and left-aligned. This is the Work Week view; any space
/// to the right of Friday and below the row is left unused so the cell keeps
/// the same shape as in every other view.
fn work_week_row(
    start: NaiveDate,
    canvas_w: i32,
    canvas_h: i32,
    aspect: CellAspect,
) -> Vec<DateCell> {
    let (cell_w, cell_h) = fit_cell(canvas_w, canvas_h, WORK_COLS, 1, aspect);
    (0..WORK_COLS)
        .map(|col| DateCell {
            date: start + Duration::days(col as i64),
            rect: Rect {
                x: cell_w * col,
                y: 0,
                w: cell_w,
                h: cell_h,
            },
            in_focus_period: true,
        })
        .collect()
}

/// `weeks` consecutive weeks starting Monday `start`, each split across two
/// rows: the five working days on top and the two weekend days below,
/// **left-aligned** (Saturday and Sunday in the first two columns, not
/// centred). Every cell is sized to the target aspect ratio, so days keep
/// the same shape as in every other view; leftover space below the last row
/// is left unused rather than stretching the cells.
fn weekday_split(
    start: NaiveDate,
    weeks: i32,
    canvas_w: i32,
    canvas_h: i32,
    aspect: CellAspect,
) -> Vec<DateCell> {
    let rows = weeks * 2;
    let (cell_w, cell_h) = fit_cell(canvas_w, canvas_h, WORK_COLS, rows, aspect);
    let mut cells = Vec::with_capacity((weeks * 7) as usize);
    for week in 0..weeks {
        let week_start = start + Duration::days((week * 7) as i64);
        // Working days: five cells across the top row.
        let wy = cell_h * (week * 2);
        for col in 0..WORK_COLS {
            cells.push(DateCell {
                date: week_start + Duration::days(col as i64),
                rect: Rect {
                    x: cell_w * col,
                    y: wy,
                    w: cell_w,
                    h: cell_h,
                },
                in_focus_period: true,
            });
        }
        // Weekend days: two cells, left-aligned, same size as a working day.
        let ey = cell_h * (week * 2 + 1);
        for col in 0..2 {
            cells.push(DateCell {
                date: week_start + Duration::days((WORK_COLS + col) as i64),
                rect: Rect {
                    x: cell_w * col,
                    y: ey,
                    w: cell_w,
                    h: cell_h,
                },
                in_focus_period: true,
            });
        }
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
        ViewMode::TwoMonths => {
            let start = week_start(anchor.with_day(1).unwrap());
            let second_month_end = last_of_month(first_of_next_month(anchor));
            let end = week_start(second_month_end) + Duration::days(7);
            Window { start, end }
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
        // A two-month page shows the anchor month plus the next; PREV/NEXT
        // slide the window by a single month so consecutive pages overlap
        // by one month rather than jumping past a month.
        ViewMode::TwoMonths => {
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

/// The entire cell is the writing surface: ink can be drawn anywhere in a
/// cell, edge to edge, with no letterboxed margins. (The stored strokes are
/// normalized to the cell, so the same note is rescaled to each view's cell
/// shape when you switch views.)
pub fn ink_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x,
        y: rect.y,
        w: rect.w.max(1),
        h: rect.h.max(1),
    }
}

/// Convert an absolute point into canonical ink coordinates normalized
/// `[0,1]` over the whole cell, clamping points just outside the cell to
/// the nearest edge.
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

    /// A representative target aspect (a real Month cell is ~190x264).
    const A: CellAspect = CellAspect { w: 190, h: 264 };

    #[test]
    fn day_view_is_a_single_cell_sized_to_the_target_aspect() {
        let cells = layout(ViewMode::Day, d(2026, 3, 15), 1404, 1872, A);
        assert_eq!(cells.len(), 1);
        let r = cells[0].rect;
        // Top-left anchored, not stretched to fill the canvas.
        assert_eq!((r.x, r.y), (0, 0));
        assert!(r.w <= 1404 && r.h <= 1872);
        // Sized to the shared aspect ratio.
        assert_eq!(r.w * A.h / A.w, r.h);
    }

    #[test]
    fn week_view_has_seven_equal_cells_left_aligned_in_two_row_bands() {
        // 2026-03-18 is a Wednesday.
        let cells = layout(ViewMode::Week, d(2026, 3, 18), 1400, 200, A);
        assert_eq!(cells.len(), 7);
        assert_eq!(cells[0].date, d(2026, 3, 16)); // Monday
        assert_eq!(cells[6].date, d(2026, 3, 22)); // Sunday
        let (cw, ch) = (cells[0].rect.w, cells[0].rect.h);
        // Every day cell is identical in size and shares the target aspect.
        for c in &cells {
            assert_eq!((c.rect.w, c.rect.h), (cw, ch));
        }
        assert_eq!(cw * A.h / A.w, ch);
        // Row 1: the five working days, left-aligned across the top.
        for (i, cell) in cells[..5].iter().enumerate() {
            assert_eq!(cell.rect.y, 0);
            assert_eq!(cell.rect.x, i as i32 * cw);
        }
        // Row 2: Saturday and Sunday, left-aligned on the next band.
        assert_eq!(cells[5].date.weekday(), Weekday::Sat);
        assert_eq!(cells[6].date.weekday(), Weekday::Sun);
        assert_eq!(cells[5].rect.y, ch);
        assert_eq!(cells[5].rect.x, 0);
        assert_eq!(cells[6].rect.x, cw);
    }

    #[test]
    fn work_week_view_has_five_equal_cells_monday_to_friday_in_one_row() {
        let cells = layout(ViewMode::WorkWeek, d(2026, 3, 18), 1400, 200, A);
        assert_eq!(cells.len(), 5);
        assert_eq!(cells[4].date.weekday(), Weekday::Fri);
        let (cw, ch) = (cells[0].rect.w, cells[0].rect.h);
        for (i, cell) in cells.iter().enumerate() {
            assert_eq!(cell.rect.y, 0);
            assert_eq!((cell.rect.w, cell.rect.h), (cw, ch));
            assert_eq!(cell.rect.x, i as i32 * cw);
        }
        assert_eq!(cw * A.h / A.w, ch);
    }

    #[test]
    fn two_weeks_view_is_four_rows_of_working_days_then_weekend() {
        let cells = layout(ViewMode::TwoWeeks, d(2026, 3, 18), 1400, 400, A);
        assert_eq!(cells.len(), 14);
        let ch = cells[0].rect.h;
        // Week 1 working days (band 0), weekend (band 1); week 2 working
        // (band 2), weekend (band 3).
        assert_eq!(cells[0].rect.y, 0); // Mon w1
        assert_eq!(cells[4].date.weekday(), Weekday::Fri);
        assert_eq!(cells[5].rect.y, ch); // Sat w1 (weekend band)
        assert_eq!(cells[5].rect.x, 0); // left-aligned
        assert_eq!(cells[7].rect.y, 2 * ch); // Mon w2
        assert_eq!(cells[12].rect.y, 3 * ch); // Sat w2
        assert_eq!(cells[12].rect.x, 0);
        assert_eq!(cells[13].date, cells[0].date + Duration::days(13));
    }

    #[test]
    fn month_view_is_a_6x7_grid_covering_the_whole_month() {
        let cells = layout(ViewMode::Month, d(2026, 3, 15), 1400, 1800, A);
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
        let cells = layout(ViewMode::Month, d(2026, 3, 15), 700, 600, A);
        assert_eq!(cells[0].date, d(2026, 2, 23));
        assert!(!cells[0].in_focus_period);
    }

    #[test]
    fn two_months_view_flows_continuously_without_duplicating_the_border_week() {
        let cells = layout(ViewMode::TwoMonths, d(2026, 3, 15), 1400, 1800, A);
        // One continuous grid: every cell is the day after the previous, so
        // the March/April border week is never repeated.
        for pair in cells.windows(2) {
            assert_eq!(pair[1].date, pair[0].date + Duration::days(1));
        }
        // March and April are the two in-focus months (31 + 30 days).
        let focus = cells.iter().filter(|c| c.in_focus_period).count();
        assert_eq!(focus, 31 + 30);
        assert!(cells
            .iter()
            .any(|c| c.date == d(2026, 3, 1) && c.in_focus_period));
        assert!(cells
            .iter()
            .any(|c| c.date == d(2026, 4, 30) && c.in_focus_period));
        // Leading days from February are out of focus, not a second month.
        assert!(!cells[0].in_focus_period);

        let window = window_for(ViewMode::TwoMonths, d(2026, 3, 15));
        assert!(window.start <= d(2026, 3, 1));
        assert!(window.end >= d(2026, 4, 30));
        // A two-month page slides by a single month: from March, NEXT lands
        // in April and PREV in February.
        assert_eq!(navigate(ViewMode::TwoMonths, d(2026, 3, 15), 1).month(), 4);
        assert_eq!(navigate(ViewMode::TwoMonths, d(2026, 3, 15), -1).month(), 2);
    }

    #[test]
    fn cell_at_finds_the_containing_cell_and_normalizes_a_point_within_it() {
        let cells = layout(ViewMode::Week, d(2026, 3, 18), 1400, 200, A);
        // A point inside Monday's cell (top-left cell of the working row).
        let monday = &cells[0];
        assert_eq!(monday.date, d(2026, 3, 16));
        let px = monday.rect.x + monday.rect.w / 2;
        let py = monday.rect.y + monday.rect.h / 2;
        let cell = cell_at(&cells, px, py).unwrap();
        assert_eq!(cell.date, d(2026, 3, 16));
        let (nx, ny) = normalize_within(cell.rect, px, py);
        assert!((0.0..=1.0).contains(&nx));
        assert!((0.0..=1.0).contains(&ny));
        let (rx, ry) = denormalize_within(cell.rect, nx, ny);
        assert!((rx - px).abs() <= 1);
        assert!((ry - py).abs() <= 1);
    }

    #[test]
    fn ink_rect_fills_the_whole_cell_with_no_margins() {
        let wide = Rect {
            x: 10,
            y: 20,
            w: 1200,
            h: 800,
        };
        // The writing surface is the entire cell — no letterboxed margins on
        // any side, so the user can draw right up to every edge.
        assert_eq!(ink_rect(wide), wide);

        // The extreme corners of the cell map to the normalized corners and
        // back, edge to edge.
        assert_eq!(denormalize_within(wide, 0.0, 0.0), (wide.x, wide.y));
        assert_eq!(
            denormalize_within(wide, 1.0, 1.0),
            (wide.x + wide.w, wide.y + wide.h)
        );
        // A point on the far-left edge normalizes to x = 0 (previously it sat
        // in an unusable margin).
        let (nx, _) = normalize_within(wide, wide.x, wide.y + wide.h / 2);
        assert_eq!(nx, 0.0);
    }

    #[test]
    fn window_for_month_matches_the_layout_grid_span() {
        let anchor = d(2026, 3, 15);
        let window = window_for(ViewMode::Month, anchor);
        let cells = layout(ViewMode::Month, anchor, 700, 600, A);
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
