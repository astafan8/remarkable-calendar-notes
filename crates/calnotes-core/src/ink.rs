//! Persistent handwritten notes, one stroke set per calendar date.
//!
//! Points are stored normalized to `[0.0, 1.0]` within the date's grid
//! cell (not in framebuffer pixels), so the same strokes render correctly
//! whichever view (Day/Week/WorkWeek/TwoWeeks/Month) currently draws that
//! date's cell, at whatever size that cell happens to be.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A single normalized point within a date cell. `(0,0)` is the cell's
/// top-left corner, `(1,1)` its bottom-right.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormPoint {
    pub x: f32,
    pub y: f32,
    /// 0.0..=1.0, defaults to 1.0 for input devices without pressure.
    #[serde(default = "default_pressure")]
    pub pressure: f32,
}

fn default_pressure() -> f32 {
    1.0
}

/// One continuous pen-down-to-pen-up stroke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Stroke {
    pub points: Vec<NormPoint>,
}

impl Stroke {
    pub fn is_empty(&self) -> bool {
        self.points.len() < 2
    }
}

/// All handwritten strokes for one calendar date.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DayNotes {
    pub strokes: Vec<Stroke>,
}

/// The full ink store: every date that has at least one stroke, plus an
/// in-memory undo stack (last-removed stroke per date, not persisted).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InkStore {
    pub days: BTreeMap<NaiveDate, DayNotes>,
    #[serde(skip)]
    undo_stack: BTreeMap<NaiveDate, Vec<Stroke>>,
}

impl InkStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn strokes_for(&self, date: NaiveDate) -> &[Stroke] {
        self.days
            .get(&date)
            .map(|d| d.strokes.as_slice())
            .unwrap_or(&[])
    }

    /// Begin a new stroke on `date`, returning its index for `push_point`.
    pub fn begin_stroke(&mut self, date: NaiveDate) -> usize {
        let day = self.days.entry(date).or_default();
        day.strokes.push(Stroke::default());
        day.strokes.len() - 1
    }

    pub fn push_point(&mut self, date: NaiveDate, stroke_index: usize, point: NormPoint) {
        if let Some(day) = self.days.get_mut(&date) {
            if let Some(stroke) = day.strokes.get_mut(stroke_index) {
                let clamped = NormPoint {
                    x: point.x.clamp(0.0, 1.0),
                    y: point.y.clamp(0.0, 1.0),
                    pressure: point.pressure.clamp(0.0, 1.0),
                };
                stroke.points.push(clamped);
            }
        }
    }

    /// Drop the just-started stroke if the pen lifted before it had at
    /// least two points (i.e. a tap, not a mark) — avoids polluting the
    /// undo stack and the persisted file with empty strokes.
    pub fn discard_if_empty(&mut self, date: NaiveDate, stroke_index: usize) {
        if let Some(day) = self.days.get_mut(&date) {
            if day.strokes.get(stroke_index).is_some_and(Stroke::is_empty) {
                day.strokes.remove(stroke_index);
            }
        }
    }

    /// Undo the most recent stroke on `date`. Returns `true` if a stroke was
    /// undone.
    pub fn undo(&mut self, date: NaiveDate) -> bool {
        if let Some(day) = self.days.get_mut(&date) {
            if let Some(stroke) = day.strokes.pop() {
                self.undo_stack.entry(date).or_default().push(stroke);
                return true;
            }
        }
        false
    }

    /// Redo the last undone stroke on `date`, if any.
    pub fn redo(&mut self, date: NaiveDate) -> bool {
        if let Some(stack) = self.undo_stack.get_mut(&date) {
            if let Some(stroke) = stack.pop() {
                self.days.entry(date).or_default().strokes.push(stroke);
                return true;
            }
        }
        false
    }

    /// Clear all ink for a single date. This is destructive and not part of
    /// the undo stack (matches the "clear day" control's intent as a hard
    /// reset), but the cleared strokes are kept for one `redo` as a safety
    /// net against an accidental tap.
    pub fn clear_day(&mut self, date: NaiveDate) {
        if let Some(day) = self.days.remove(&date) {
            if !day.strokes.is_empty() {
                self.undo_stack.insert(date, day.strokes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 4, day).unwrap()
    }

    #[test]
    fn stroke_points_are_clamped_to_unit_square() {
        let mut store = InkStore::new();
        let idx = store.begin_stroke(d(1));
        store.push_point(
            d(1),
            idx,
            NormPoint {
                x: -0.5,
                y: 1.5,
                pressure: 2.0,
            },
        );
        let p = store.strokes_for(d(1))[0].points[0];
        assert_eq!(p.x, 0.0);
        assert_eq!(p.y, 1.0);
        assert_eq!(p.pressure, 1.0);
    }

    #[test]
    fn undo_removes_last_stroke_and_redo_restores_it() {
        let mut store = InkStore::new();
        let idx = store.begin_stroke(d(2));
        store.push_point(
            d(2),
            idx,
            NormPoint {
                x: 0.1,
                y: 0.1,
                pressure: 1.0,
            },
        );
        store.push_point(
            d(2),
            idx,
            NormPoint {
                x: 0.2,
                y: 0.2,
                pressure: 1.0,
            },
        );
        assert_eq!(store.strokes_for(d(2)).len(), 1);
        assert!(store.undo(d(2)));
        assert_eq!(store.strokes_for(d(2)).len(), 0);
        assert!(store.redo(d(2)));
        assert_eq!(store.strokes_for(d(2)).len(), 1);
    }

    #[test]
    fn clear_day_removes_all_strokes_for_that_date_only() {
        let mut store = InkStore::new();
        let i1 = store.begin_stroke(d(3));
        store.push_point(
            d(3),
            i1,
            NormPoint {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
            },
        );
        store.push_point(
            d(3),
            i1,
            NormPoint {
                x: 0.1,
                y: 0.1,
                pressure: 1.0,
            },
        );
        let i2 = store.begin_stroke(d(4));
        store.push_point(
            d(4),
            i2,
            NormPoint {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
            },
        );
        store.push_point(
            d(4),
            i2,
            NormPoint {
                x: 0.1,
                y: 0.1,
                pressure: 1.0,
            },
        );

        store.clear_day(d(3));
        assert_eq!(store.strokes_for(d(3)).len(), 0);
        assert_eq!(store.strokes_for(d(4)).len(), 1);
    }

    #[test]
    fn discard_if_empty_drops_a_tap_without_dragging() {
        let mut store = InkStore::new();
        let idx = store.begin_stroke(d(5));
        store.push_point(
            d(5),
            idx,
            NormPoint {
                x: 0.5,
                y: 0.5,
                pressure: 1.0,
            },
        );
        store.discard_if_empty(d(5), idx);
        assert_eq!(store.strokes_for(d(5)).len(), 0);
    }

    #[test]
    fn ink_store_round_trips_through_json() {
        let mut store = InkStore::new();
        let idx = store.begin_stroke(d(6));
        store.push_point(
            d(6),
            idx,
            NormPoint {
                x: 0.25,
                y: 0.75,
                pressure: 0.5,
            },
        );
        let json = serde_json::to_string(&store).unwrap();
        let restored: InkStore = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.strokes_for(d(6)), store.strokes_for(d(6)));
    }
}
