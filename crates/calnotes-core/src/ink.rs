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

/// The full ink store: every date that has at least one stroke, plus the
/// in-memory removed-strokes stack (not persisted) that powers Undo of an
/// erase/lasso/clear.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InkStore {
    pub days: BTreeMap<NaiveDate, DayNotes>,
    #[serde(skip)]
    removed_stack: BTreeMap<NaiveDate, Vec<Vec<Stroke>>>,
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
        // A newly drawn stroke becomes the latest edit, so an older erase
        // must no longer take priority over it when the user taps Undo.
        self.removed_stack.remove(&date);
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
        if let Some(stack) = self.removed_stack.get_mut(&date) {
            if let Some(strokes) = stack.pop() {
                self.days.entry(date).or_default().strokes.extend(strokes);
                if stack.is_empty() {
                    self.removed_stack.remove(&date);
                }
                return true;
            }
        }
        if let Some(day) = self.days.get_mut(&date) {
            if day.strokes.pop().is_some() {
                return true;
            }
        }
        false
    }

    /// Clear all ink for a single date. Undoable via the removed-strokes
    /// stack (so an accidental Clear tap can be reversed).
    pub fn clear_day(&mut self, date: NaiveDate) {
        if let Some(day) = self.days.remove(&date) {
            if !day.strokes.is_empty() {
                self.removed_stack
                    .entry(date)
                    .or_default()
                    .push(day.strokes);
            }
        }
    }

    /// Remove whole strokes touched by an eraser path.
    pub fn erase_path(&mut self, date: NaiveDate, path: &[NormPoint], radius: f32) -> usize {
        if path.is_empty() {
            return 0;
        }
        self.remove_matching(date, |stroke| {
            stroke.points.iter().any(|point| {
                path.iter()
                    .any(|eraser| point_distance(*point, *eraser) <= radius)
            }) || stroke.points.windows(2).any(|stroke_segment| {
                path.windows(2).any(|eraser_segment| {
                    segment_distance(
                        stroke_segment[0],
                        stroke_segment[1],
                        eraser_segment[0],
                        eraser_segment[1],
                    ) <= radius
                })
            })
        })
    }

    /// Remove whole strokes selected by a closed lasso polygon.
    pub fn delete_inside_lasso(&mut self, date: NaiveDate, polygon: &[NormPoint]) -> usize {
        if polygon.len() < 3 {
            return 0;
        }
        self.remove_matching(date, |stroke| {
            stroke
                .points
                .iter()
                .any(|point| point_inside_polygon(*point, polygon))
        })
    }

    fn remove_matching(
        &mut self,
        date: NaiveDate,
        mut should_remove: impl FnMut(&Stroke) -> bool,
    ) -> usize {
        let Some(day) = self.days.get_mut(&date) else {
            return 0;
        };
        let mut removed = Vec::new();
        day.strokes.retain(|stroke| {
            if should_remove(stroke) {
                removed.push(stroke.clone());
                false
            } else {
                true
            }
        });
        let count = removed.len();
        if day.strokes.is_empty() {
            self.days.remove(&date);
        }
        if count > 0 {
            self.removed_stack.entry(date).or_default().push(removed);
        }
        count
    }
}

fn point_distance(a: NormPoint, b: NormPoint) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

fn point_segment_distance(point: NormPoint, start: NormPoint, end: NormPoint) -> f32 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_sq = dx * dx + dy * dy;
    if length_sq <= f32::EPSILON {
        return point_distance(point, start);
    }
    let t = (((point.x - start.x) * dx + (point.y - start.y) * dy) / length_sq).clamp(0.0, 1.0);
    point_distance(
        point,
        NormPoint {
            x: start.x + t * dx,
            y: start.y + t * dy,
            pressure: 1.0,
        },
    )
}

fn segment_distance(a0: NormPoint, a1: NormPoint, b0: NormPoint, b1: NormPoint) -> f32 {
    if segments_intersect(a0, a1, b0, b1) {
        return 0.0;
    }
    point_segment_distance(a0, b0, b1)
        .min(point_segment_distance(a1, b0, b1))
        .min(point_segment_distance(b0, a0, a1))
        .min(point_segment_distance(b1, a0, a1))
}

fn segments_intersect(a0: NormPoint, a1: NormPoint, b0: NormPoint, b1: NormPoint) -> bool {
    fn cross(a: NormPoint, b: NormPoint, c: NormPoint) -> f32 {
        (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
    }
    let c1 = cross(a0, a1, b0);
    let c2 = cross(a0, a1, b1);
    let c3 = cross(b0, b1, a0);
    let c4 = cross(b0, b1, a1);
    c1 * c2 <= 0.0 && c3 * c4 <= 0.0
}

fn point_inside_polygon(point: NormPoint, polygon: &[NormPoint]) -> bool {
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        let crosses = (current.y > point.y) != (previous.y > point.y)
            && point.x
                < (previous.x - current.x) * (point.y - current.y) / (previous.y - current.y)
                    + current.x;
        if crosses {
            inside = !inside;
        }
        previous = current;
    }
    inside
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
    fn undo_removes_last_stroke() {
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
        // Nothing left to undo.
        assert!(!store.undo(d(2)));
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
        assert!(store.undo(d(3)));
        assert_eq!(store.strokes_for(d(3)).len(), 1);
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

    fn add_line(store: &mut InkStore, date: NaiveDate, from: (f32, f32), to: (f32, f32)) {
        let idx = store.begin_stroke(date);
        for (x, y) in [from, to] {
            store.push_point(
                date,
                idx,
                NormPoint {
                    x,
                    y,
                    pressure: 1.0,
                },
            );
        }
    }

    #[test]
    fn eraser_removes_only_strokes_touched_by_its_path() {
        let mut store = InkStore::new();
        add_line(&mut store, d(7), (0.1, 0.1), (0.3, 0.3));
        add_line(&mut store, d(7), (0.7, 0.7), (0.9, 0.9));
        let erased = store.erase_path(
            d(7),
            &[
                NormPoint {
                    x: 0.0,
                    y: 0.2,
                    pressure: 1.0,
                },
                NormPoint {
                    x: 0.4,
                    y: 0.2,
                    pressure: 1.0,
                },
            ],
            0.05,
        );
        assert_eq!(erased, 1);
        assert_eq!(store.strokes_for(d(7)).len(), 1);
    }

    #[test]
    fn lasso_removes_strokes_with_points_inside_polygon() {
        let mut store = InkStore::new();
        add_line(&mut store, d(8), (0.2, 0.2), (0.3, 0.3));
        add_line(&mut store, d(8), (0.7, 0.7), (0.8, 0.8));
        let polygon = [
            NormPoint {
                x: 0.1,
                y: 0.1,
                pressure: 1.0,
            },
            NormPoint {
                x: 0.4,
                y: 0.1,
                pressure: 1.0,
            },
            NormPoint {
                x: 0.4,
                y: 0.4,
                pressure: 1.0,
            },
            NormPoint {
                x: 0.1,
                y: 0.4,
                pressure: 1.0,
            },
        ];
        assert_eq!(store.delete_inside_lasso(d(8), &polygon), 1);
        assert_eq!(store.strokes_for(d(8)).len(), 1);
        assert_eq!(store.strokes_for(d(8))[0].points[0].x, 0.7);
        assert!(store.undo(d(8)));
        assert_eq!(store.strokes_for(d(8)).len(), 2);
    }

    #[test]
    fn undo_restores_consecutive_erase_operations_in_reverse_order() {
        let mut store = InkStore::new();
        add_line(&mut store, d(9), (0.1, 0.1), (0.2, 0.2));
        add_line(&mut store, d(9), (0.4, 0.4), (0.5, 0.5));
        assert_eq!(
            store.erase_path(
                d(9),
                &[NormPoint {
                    x: 0.15,
                    y: 0.15,
                    pressure: 1.0,
                }],
                0.1,
            ),
            1
        );
        assert_eq!(
            store.erase_path(
                d(9),
                &[NormPoint {
                    x: 0.45,
                    y: 0.45,
                    pressure: 1.0,
                }],
                0.1,
            ),
            1
        );
        assert!(store.strokes_for(d(9)).is_empty());
        assert!(store.undo(d(9)));
        assert_eq!(store.strokes_for(d(9)).len(), 1);
        assert!(store.undo(d(9)));
        assert_eq!(store.strokes_for(d(9)).len(), 2);
    }
}
