//! Application logic: screen/navigation state, the touchscreen source
//! editor, and rendering — all built only on `calnotes-core`, so it is
//! fully testable without any device connection. `main.rs` is the thin
//! layer that feeds real QTFB input events into this module and pushes its
//! rendered [`calnotes_core::render::FrameBuffer`] to the shared
//! framebuffer.
//!
//! Most of this module's public surface is only exercised by
//! `main.rs`'s `device_loop` (Unix-only, since it drives the real QTFB
//! event loop) or by this module's own tests — never by the `preview`
//! subcommand alone. That makes plain `cargo build`/`clippy` on a non-Unix
//! host (e.g. Windows, for local development) see legitimately-used code
//! as "never constructed"; the `not(unix)` allow below only suppresses
//! that host-specific false positive; note that `#[cfg(test)]` already
//! covers it during `cargo test` on any platform.
#![cfg_attr(not(unix), allow(dead_code))]

use calnotes_core::config::AppState;
use calnotes_core::model::{CalendarSource, Event, SourceKind, SourceStatus, ViewMode};
use calnotes_core::recurrence::Window;
use calnotes_core::render::{FrameBuffer, BLACK, GRAY, WHITE};
use calnotes_core::sources::google;
use calnotes_core::timeutil::UtcOffset;
use calnotes_core::vkb::{TextField, VkbKey};
use calnotes_core::{ink::NormPoint, sources, view};
use chrono::{Datelike, Duration, NaiveDate};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

pub const CANVAS_W: i32 = calnotes_core::render::SCREEN_WIDTH as i32;
pub const CANVAS_H: i32 = calnotes_core::render::SCREEN_HEIGHT as i32;

/// Height of each toolbar row (views, navigation, then ink tools),
/// in canvas pixels.
const TOOLBAR_ROW_H: i32 = 96;
const TOOLBAR_H: i32 = TOOLBAR_ROW_H * 3;
const MONTH_LABEL_W: i32 = 52;
const UI_TEXT_SCALE: i32 = 4;
const BODY_TEXT_SCALE: i32 = 3;
const EVENT_TEXT_SCALE: i32 = 2;

/// Pen stroke width, in canvas pixels. Shared by the full re-render and the
/// incremental per-segment drawing so both produce identical ink.
pub const INK_THICKNESS: i32 = 2;

/// How much wider than the visible window events are fetched, in days.
///
/// Navigating one page (or switching views) therefore almost always stays
/// inside data that has already been fetched, so a moved view shows its
/// events immediately instead of appearing empty until the next refresh.
const FETCH_PADDING_DAYS: i64 = 45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Calendar,
    Settings,
}

/// Buttons on the second toolbar row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Settings,
    Prev,
    Today,
    Next,
    Pen,
    Erase,
    Lasso,
    Undo,
    ClearDay,
}

const NAV_ACTIONS: [Action; 4] = [Action::Settings, Action::Prev, Action::Today, Action::Next];

const TOOL_ACTIONS: [Action; 5] = [
    Action::Pen,
    Action::Erase,
    Action::Lasso,
    Action::Undo,
    Action::ClearDay,
];

impl Action {
    fn label(&self) -> &'static str {
        match self {
            Action::Settings => "SET",
            Action::Prev => "PREV",
            Action::Today => "TODAY",
            Action::Next => "NEXT",
            Action::Pen => "PEN",
            Action::Erase => "ERASE",
            Action::Lasso => "LASSO",
            Action::Undo => "UNDO",
            Action::ClearDay => "CLEAR",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InkTool {
    Pen,
    Erase,
    Lasso,
}

enum ActiveGesture {
    Draw {
        date: NaiveDate,
        stroke_index: usize,
        last_drawn: (i32, i32),
    },
    Erase {
        date: NaiveDate,
        points: Vec<NormPoint>,
    },
    Lasso {
        date: NaiveDate,
        points: Vec<NormPoint>,
        last_drawn: (i32, i32),
    },
}

/// The newest not-yet-drawn piece of a pen stroke, in absolute canvas
/// pixels. The device loop draws exactly this into its persistent
/// framebuffer and refreshes only the rectangle it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PenSegment {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub thickness: i32,
}

impl PenSegment {
    /// The dirty rectangle this segment touches, before clamping to the
    /// framebuffer.
    pub fn dirty_rect(&self) -> view::Rect {
        FrameBuffer::line_bounds(self.x0, self.y0, self.x1, self.y1, self.thickness)
    }
}

/// Result of a background source refresh, handed back to the UI thread.
struct RefreshOutcome {
    sources: Vec<CalendarSource>,
    events: HashMap<String, Vec<Event>>,
    window: Window,
}

/// Where an in-progress Google device-flow login has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleLoginPhase {
    /// Asking Google for a device/user code.
    Requesting,
    /// Waiting for the user to approve on another device.
    AwaitingApproval {
        user_code: String,
        verification_url: String,
    },
    Done,
    Failed(String),
}

/// A Google OAuth device-flow login running on a worker thread.
///
/// The network work (requesting a device code, then polling until the user
/// approves — which takes as long as the user takes) happens entirely off
/// the input/render loop; the UI only ever does a non-blocking
/// `try_recv` per tick, so the app stays responsive and redrawable
/// throughout.
pub struct GoogleLogin {
    pub source_id: String,
    pub phase: GoogleLoginPhase,
    rx: Receiver<GoogleLoginEvent>,
}

enum GoogleLoginEvent {
    CodeReady {
        user_code: String,
        verification_url: String,
    },
    Approved {
        refresh_token: String,
    },
    Failed(String),
}

pub struct App {
    pub state: AppState,
    pub screen: Screen,
    events_cache: HashMap<String, Vec<Event>>,
    /// The window the cached events were fetched for, if any.
    cached_window: Option<Window>,
    active_gesture: Option<ActiveGesture>,
    pub ink_tool: InkTool,
    pub editor: Option<SourceEditor>,
    /// Editable text field for the fixed UTC offset (minutes), shown on
    /// the settings screen. `Some` while the user is actively editing it.
    pub offset_editor: Option<TextField>,
    next_source_seq: u64,
    /// Short, user-visible status line (refresh progress, login progress).
    pub status: String,
    refresh_rx: Option<Receiver<RefreshOutcome>>,
    pub google_login: Option<GoogleLogin>,
}

impl App {
    pub fn new() -> std::io::Result<Self> {
        let mut state = AppState::load()?;
        if calnotes_core::model::is_unset_anchor(state.config.anchor_date) {
            state.config.anchor_date = UtcOffset::new(state.config.utc_offset_minutes).today();
        }
        // Show the last successful fetch immediately, before (and if) the
        // network answers. `cached_window` stays `None`, so a real refresh
        // is still started by the caller.
        let mut events_cache = HashMap::new();
        for source in state.config.sources.iter().filter(|s| s.enabled) {
            if let Ok(events) = sources::cache::load_cache(&source.id) {
                if !events.is_empty() {
                    events_cache.insert(source.id.clone(), events);
                }
            }
        }
        Ok(App {
            state,
            screen: Screen::Calendar,
            events_cache,
            cached_window: None,
            active_gesture: None,
            ink_tool: InkTool::Pen,
            editor: None,
            offset_editor: None,
            next_source_seq: 0,
            status: String::new(),
            refresh_rx: None,
            google_login: None,
        })
    }

    pub fn offset(&self) -> UtcOffset {
        UtcOffset::new(self.state.config.utc_offset_minutes)
    }

    /// Today's date at the configured fixed UTC offset.
    pub fn today(&self) -> NaiveDate {
        self.offset().today()
    }

    /// The date range currently on screen.
    pub fn window(&self) -> Window {
        view::window_for(self.state.config.view_mode, self.state.config.anchor_date)
    }

    /// The (wider) range events are fetched for — see [`FETCH_PADDING_DAYS`].
    pub fn fetch_window(&self) -> Window {
        let visible = self.window();
        Window {
            start: visible.start - Duration::days(FETCH_PADDING_DAYS),
            end: visible.end + Duration::days(FETCH_PADDING_DAYS),
        }
    }

    /// Whether already-fetched events cover everything `window` needs.
    fn cache_covers(&self, window: Window) -> bool {
        self.cached_window
            .is_some_and(|c| c.start <= window.start && c.end >= window.end)
    }

    /// Kick off a background refresh of every enabled source, if one isn't
    /// already running. Returns immediately: the event loop keeps drawing
    /// and handling input while the worker thread does the network I/O,
    /// and [`App::poll_background`] applies the result when it arrives.
    pub fn start_refresh(&mut self) {
        if self.refresh_rx.is_some() {
            return;
        }

        let window = self.fetch_window();
        let offset = self.offset();
        let mut sources: Vec<CalendarSource> = self
            .state
            .config
            .sources
            .iter()
            .filter(|s| s.enabled)
            .cloned()
            .collect();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut events = HashMap::new();
            for source in &mut sources {
                let fetched = sources::refresh_source(source, window, offset);
                events.insert(source.id.clone(), fetched);
            }
            let _ = tx.send(RefreshOutcome {
                sources,
                events,
                window,
            });
        });
        self.refresh_rx = Some(rx);
        self.status = if self.state.config.sources.iter().any(|s| s.enabled) {
            "REFRESHING...".to_string()
        } else {
            "NO ENABLED SOURCES".to_string()
        };
    }

    fn start_source_test(&mut self, source_id: &str) {
        if self.refresh_rx.is_some() {
            self.status = "WAIT FOR CURRENT REFRESH".to_string();
            return;
        }
        let Some(source) = self
            .state
            .config
            .sources
            .iter()
            .find(|source| source.id == source_id)
            .cloned()
        else {
            return;
        };
        let window = self.fetch_window();
        let offset = self.offset();
        let label = source.label.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut source = source;
            let fetched = sources::refresh_source(&mut source, window, offset);
            let mut events = HashMap::new();
            events.insert(source.id.clone(), fetched);
            let _ = tx.send(RefreshOutcome {
                sources: vec![source],
                events,
                window,
            });
        });
        self.refresh_rx = Some(rx);
        self.status = format!("TESTING {}...", label.to_uppercase());
    }

    /// Refresh synchronously. Only used by the desktop `preview` command
    /// and tests — the device event loop always uses [`App::start_refresh`]
    /// so the UI never blocks on the network.
    pub fn refresh_blocking(&mut self) {
        let window = self.fetch_window();
        let offset = self.offset();
        let mut events = HashMap::new();
        let mut updated = Vec::new();
        for source in self.state.config.sources.iter().filter(|s| s.enabled) {
            let mut clone = source.clone();
            let fetched = sources::refresh_source(&mut clone, window, offset);
            events.insert(clone.id.clone(), fetched);
            updated.push(clone);
        }
        self.apply_refresh(RefreshOutcome {
            sources: updated,
            events,
            window,
        });
    }

    fn apply_refresh(&mut self, outcome: RefreshOutcome) {
        // Whatever produced this result, no refresh is outstanding now.
        self.refresh_rx = None;
        let mut ok = 0usize;
        let mut failed = 0usize;
        for updated in &outcome.sources {
            let Some(existing) = self
                .state
                .config
                .sources
                .iter_mut()
                .find(|s| s.id == updated.id)
            else {
                // The source was deleted while the refresh was in flight.
                continue;
            };
            existing.last_status = updated.last_status.clone();
            match updated.last_status {
                SourceStatus::Error { .. } => failed += 1,
                _ => ok += 1,
            }
            if let Some(events) = outcome.events.get(&updated.id) {
                self.events_cache.insert(updated.id.clone(), events.clone());
            }
        }
        // Drop cached events for sources that no longer exist / are off.
        let live: Vec<String> = self
            .state
            .config
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.id.clone())
            .collect();
        self.events_cache.retain(|id, _| live.contains(id));
        self.cached_window = Some(outcome.window);
        self.status = if failed > 0 {
            format!("SYNCED {ok} OK, {failed} FAILED")
        } else if ok > 0 {
            format!("SYNCED {ok} SOURCES")
        } else {
            "NO ENABLED SOURCES".to_string()
        };
        let _ = self.state.save_config();
    }

    /// Poll every background worker without blocking. Returns `true` if
    /// anything changed that the caller should redraw.
    pub fn poll_background(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.refresh_rx {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.refresh_rx = None;
                    self.apply_refresh(outcome);
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.refresh_rx = None;
                    self.status = "REFRESH FAILED".to_string();
                    changed = true;
                }
            }
        }
        if self.poll_google_login() {
            changed = true;
        }
        changed
    }

    fn poll_google_login(&mut self) -> bool {
        let Some(login) = &mut self.google_login else {
            return false;
        };
        let mut changed = false;
        let mut approved: Option<String> = None;
        loop {
            match login.rx.try_recv() {
                Ok(GoogleLoginEvent::CodeReady {
                    user_code,
                    verification_url,
                }) => {
                    login.phase = GoogleLoginPhase::AwaitingApproval {
                        user_code,
                        verification_url,
                    };
                    changed = true;
                }
                Ok(GoogleLoginEvent::Approved { refresh_token }) => {
                    login.phase = GoogleLoginPhase::Done;
                    approved = Some(refresh_token);
                    changed = true;
                }
                Ok(GoogleLoginEvent::Failed(message)) => {
                    login.phase = GoogleLoginPhase::Failed(message);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !matches!(
                        login.phase,
                        GoogleLoginPhase::Done | GoogleLoginPhase::Failed(_)
                    ) {
                        login.phase = GoogleLoginPhase::Failed("login worker stopped".into());
                        changed = true;
                    }
                    break;
                }
            }
        }
        if let Some(refresh_token) = approved {
            let source_id = self.google_login.as_ref().unwrap().source_id.clone();
            self.store_google_refresh_token(&source_id, refresh_token);
            self.status = "GOOGLE LOGIN OK".to_string();
            // Newly authorized source: pull its events straight away.
            self.start_refresh();
        }
        changed
    }

    fn store_google_refresh_token(&mut self, source_id: &str, token: String) {
        if let Some(source) = self
            .state
            .config
            .sources
            .iter_mut()
            .find(|s| s.id == source_id)
        {
            if let SourceKind::GoogleCalendar { refresh_token, .. } = &mut source.kind {
                *refresh_token = Some(token);
            }
            source.last_status = SourceStatus::NeverSynced;
        }
        let _ = self.state.save_config();
    }

    /// Start the OAuth 2.0 device authorization flow for a Google source.
    pub fn start_google_login(&mut self, source_id: &str) {
        let Some(source) = self.state.config.sources.iter().find(|s| s.id == source_id) else {
            return;
        };
        let SourceKind::GoogleCalendar {
            client_id,
            client_secret,
            ..
        } = &source.kind
        else {
            return;
        };
        if client_id.trim().is_empty() || client_secret.trim().is_empty() {
            self.status = "ENTER CLIENT ID AND SECRET FIRST".to_string();
            return;
        }
        let client_id = client_id.clone();
        let client_secret = client_secret.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let code = match google::request_device_code(&client_id) {
                Ok(code) => code,
                Err(e) => {
                    let _ = tx.send(GoogleLoginEvent::Failed(e.to_string()));
                    return;
                }
            };
            if tx
                .send(GoogleLoginEvent::CodeReady {
                    user_code: code.user_code.clone(),
                    verification_url: code.verification_url.clone(),
                })
                .is_err()
            {
                return; // UI dropped the login (cancelled)
            }
            let mut interval = std::time::Duration::from_secs(code.interval.clamp(1, 60));
            let deadline =
                Instant::now() + std::time::Duration::from_secs(code.expires_in.min(1800));
            while Instant::now() < deadline {
                std::thread::sleep(interval);
                match google::poll_device_token(&client_id, &client_secret, &code.device_code) {
                    Ok(google::PollOutcome::Pending) => continue,
                    Ok(google::PollOutcome::SlowDown) => {
                        interval += std::time::Duration::from_secs(5);
                    }
                    Ok(google::PollOutcome::Approved { refresh_token, .. }) => {
                        let _ = tx.send(GoogleLoginEvent::Approved { refresh_token });
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(GoogleLoginEvent::Failed(e.to_string()));
                        return;
                    }
                }
            }
            let _ = tx.send(GoogleLoginEvent::Failed("login timed out".into()));
        });
        self.google_login = Some(GoogleLogin {
            source_id: source_id.to_string(),
            phase: GoogleLoginPhase::Requesting,
            rx,
        });
        self.status = "GOOGLE LOGIN STARTED".to_string();
    }

    pub fn events_for(&self, date: NaiveDate) -> Vec<&Event> {
        self.events_cache
            .values()
            .flatten()
            .filter(|e| e.time.dates().contains(&date))
            .collect()
    }

    pub fn set_view_mode(&mut self, mode: ViewMode) {
        self.state.config.view_mode = mode;
        let _ = self.state.save_config();
        self.refresh_if_window_uncovered();
    }

    pub fn navigate(&mut self, delta: i32) {
        self.state.config.anchor_date = view::navigate(
            self.state.config.view_mode,
            self.state.config.anchor_date,
            delta,
        );
        let _ = self.state.save_config();
        self.refresh_if_window_uncovered();
    }

    /// Jump back to the real current date at the configured offset.
    pub fn go_to_today(&mut self) {
        self.state.config.anchor_date = self.today();
        let _ = self.state.save_config();
        self.refresh_if_window_uncovered();
    }

    /// After any navigation/view change, only hit the network if the new
    /// view needs data the wider cached window doesn't already cover.
    fn refresh_if_window_uncovered(&mut self) {
        let needed = self.window();
        if !self.cache_covers(needed) {
            self.start_refresh();
        }
    }

    pub fn undo_current_day(&mut self) {
        if self.state.ink.undo(self.state.config.anchor_date) {
            let _ = self.state.save_ink();
        }
    }

    pub fn clear_current_day(&mut self) {
        self.state.ink.clear_day(self.state.config.anchor_date);
        let _ = self.state.save_ink();
    }

    fn grid_cells(&self) -> Vec<view::DateCell> {
        let month_gutter = if self.state.config.view_mode == ViewMode::Month {
            MONTH_LABEL_W
        } else {
            0
        };
        view::layout(
            self.state.config.view_mode,
            self.state.config.anchor_date,
            CANVAS_W - month_gutter,
            CANVAS_H - TOOLBAR_H,
        )
        .into_iter()
        .map(|mut c| {
            c.rect.x += month_gutter;
            c.rect.y += TOOLBAR_H;
            c
        })
        .collect()
    }

    /// Toolbar button rectangles, one per [`ViewMode`], in display order.
    fn view_buttons(&self) -> Vec<(ViewMode, view::Rect)> {
        let modes = ViewMode::ALL;
        let button_w = CANVAS_W / modes.len() as i32;
        modes
            .iter()
            .enumerate()
            .map(|(i, m)| {
                (
                    *m,
                    view::Rect {
                        x: i as i32 * button_w,
                        y: 0,
                        w: button_w,
                        h: TOOLBAR_ROW_H,
                    },
                )
            })
            .collect()
    }

    fn action_buttons_for(actions: &[Action], row: i32) -> Vec<(Action, view::Rect)> {
        let button_w = CANVAS_W / actions.len() as i32;
        actions
            .iter()
            .enumerate()
            .map(|(i, a)| {
                (
                    *a,
                    view::Rect {
                        x: i as i32 * button_w,
                        y: row * TOOLBAR_ROW_H,
                        w: button_w,
                        h: TOOLBAR_ROW_H,
                    },
                )
            })
            .collect()
    }

    fn action_buttons(&self) -> Vec<(Action, view::Rect)> {
        Self::action_buttons_for(&NAV_ACTIONS, 1)
            .into_iter()
            .chain(Self::action_buttons_for(&TOOL_ACTIONS, 2))
            .collect()
    }

    /// Handle a touch tap at `(x, y)` in canvas pixels.
    pub fn handle_touch_tap(&mut self, x: i32, y: i32) {
        if self.screen == Screen::Settings {
            self.handle_settings_tap(x, y);
            return;
        }
        for (mode, rect) in self.view_buttons() {
            if within(rect, x, y) {
                self.set_view_mode(mode);
                return;
            }
        }
        for (action, rect) in self.action_buttons() {
            if within(rect, x, y) {
                self.perform_action(action);
                return;
            }
        }
        if self.state.config.view_mode == ViewMode::Day {
            return;
        }
        let cells = self.grid_cells();
        if let Some(cell) = view::cell_at(&cells, x, y) {
            self.state.config.anchor_date = cell.date;
            self.set_view_mode(ViewMode::Day);
        }
    }

    fn perform_action(&mut self, action: Action) {
        match action {
            Action::Settings => {
                self.screen = Screen::Settings;
                self.editor = None;
            }
            Action::Prev => self.navigate(-1),
            Action::Next => self.navigate(1),
            Action::Today => self.go_to_today(),
            Action::Pen => self.ink_tool = InkTool::Pen,
            Action::Erase => self.ink_tool = InkTool::Erase,
            Action::Lasso => self.ink_tool = InkTool::Lasso,
            Action::Undo => self.undo_current_day(),
            Action::ClearDay => self.clear_current_day(),
        }
    }

    /// Begin a pen stroke at `(x, y)`. Pen and touch are delivered as
    /// distinct QTFB input kinds (see `calnotes-device::qtfb`), so this is
    /// only ever called for genuine pen-down events — it never fires from
    /// a touch/navigation gesture, and vice versa. Ink only exists on the
    /// calendar screen; pen samples over the settings screen are ignored.
    pub fn pen_down(&mut self, x: i32, y: i32, pressure: f32) {
        if self.screen != Screen::Calendar || y < TOOLBAR_H {
            return;
        }
        let cells = self.grid_cells();
        let Some(cell) = view::cell_at(&cells, x, y) else {
            return;
        };
        let rect = cell.rect;
        let date = cell.date;
        let (nx, ny) = view::normalize_within(rect, x, y);
        let point = NormPoint {
            x: nx,
            y: ny,
            pressure,
        };
        let last_drawn = view::denormalize_within(rect, nx, ny);
        self.active_gesture = Some(match self.ink_tool {
            InkTool::Pen => {
                let stroke_index = self.state.ink.begin_stroke(date);
                self.state.ink.push_point(date, stroke_index, point);
                ActiveGesture::Draw {
                    date,
                    stroke_index,
                    last_drawn,
                }
            }
            InkTool::Erase => ActiveGesture::Erase {
                date,
                points: vec![point],
            },
            InkTool::Lasso => ActiveGesture::Lasso {
                date,
                points: vec![point],
                last_drawn,
            },
        });
    }

    /// Continue the active stroke, returning the one new segment to draw.
    ///
    /// The caller (the device loop) draws just this segment into the
    /// framebuffer it already holds and refreshes only its dirty rect —
    /// no full re-render, no full-frame copy, per pen sample.
    pub fn pen_move(&mut self, x: i32, y: i32, pressure: f32) -> Option<PenSegment> {
        let (date, px0, py0) = match self.active_gesture.as_ref()? {
            ActiveGesture::Draw {
                date, last_drawn, ..
            }
            | ActiveGesture::Lasso {
                date, last_drawn, ..
            } => (*date, last_drawn.0, last_drawn.1),
            ActiveGesture::Erase { date, .. } => (*date, 0, 0),
        };
        let cells = self.grid_cells();
        // A stroke stays bound to the cell it started in even if the pen
        // drifts slightly over a cell boundary, so a single mark never
        // silently splits across two dates.
        let rect = cells
            .iter()
            .find(|c| c.date == date)
            .map(|c| c.rect)
            .unwrap_or(view::Rect {
                x: 0,
                y: 0,
                w: CANVAS_W,
                h: CANVAS_H,
            });
        let (nx, ny) = view::normalize_within(rect, x, y);
        let point = NormPoint {
            x: nx,
            y: ny,
            pressure,
        };
        let (px1, py1) = view::denormalize_within(rect, nx, ny);
        match self.active_gesture.as_mut()? {
            ActiveGesture::Draw {
                stroke_index,
                last_drawn,
                ..
            } => {
                self.state.ink.push_point(date, *stroke_index, point);
                *last_drawn = (px1, py1);
            }
            ActiveGesture::Erase { points, .. } => {
                points.push(point);
                return None;
            }
            ActiveGesture::Lasso {
                points, last_drawn, ..
            } => {
                points.push(point);
                *last_drawn = (px1, py1);
            }
        }
        Some(PenSegment {
            x0: px0,
            y0: py0,
            x1: px1,
            y1: py1,
            thickness: INK_THICKNESS,
        })
    }

    /// Finish the current pen gesture. Returns `true` when temporary ink
    /// or deleted strokes require a full redraw.
    pub fn pen_up(&mut self) -> bool {
        let Some(active) = self.active_gesture.take() else {
            return false;
        };
        let redraw = match active {
            ActiveGesture::Draw {
                date, stroke_index, ..
            } => {
                self.state.ink.discard_if_empty(date, stroke_index);
                false
            }
            ActiveGesture::Erase { date, points } => {
                self.state.ink.erase_path(date, &points, 0.035);
                true
            }
            ActiveGesture::Lasso { date, points, .. } => {
                self.state.ink.delete_inside_lasso(date, &points);
                true
            }
        };
        let _ = self.state.save_ink();
        redraw
    }

    /// Feed one raw VKB key code (an `INPUT_VKB_PRESS` event's key code,
    /// which QTFB carries in the event's `x` field) to whichever
    /// settings-screen text field currently has focus: the UTC-offset
    /// field takes priority while actively being edited, otherwise the
    /// source editor (if any).
    pub fn handle_vkb(&mut self, raw: i32) {
        let (key, _mods) = calnotes_core::vkb::decode(raw);
        if let Some(offset) = &mut self.offset_editor {
            offset.apply_key(key);
        } else if let Some(editor) = &mut self.editor {
            editor.handle_key(key);
        }
    }

    /// Render the current screen into a fresh full-size framebuffer.
    ///
    /// This is the *full* redraw path, used on startup, navigation, view
    /// changes, and UI changes. Individual pen samples deliberately do not
    /// go through it — see [`App::pen_move`] and the device loop.
    pub fn render(&self) -> FrameBuffer {
        let mut fb = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
        self.render_into(&mut fb);
        fb
    }

    /// Render the current screen into an existing framebuffer, reusing its
    /// allocation.
    pub fn render_into(&self, fb: &mut FrameBuffer) {
        match self.screen {
            Screen::Calendar => self.render_calendar(fb),
            Screen::Settings => self.render_settings(fb),
        }
    }

    fn render_calendar(&self, fb: &mut FrameBuffer) {
        fb.clear(WHITE);
        for (mode, rect) in self.view_buttons() {
            let active = mode == self.state.config.view_mode;
            draw_button(fb, rect, mode.label(), active);
        }
        for (action, rect) in self.action_buttons() {
            let active = matches!(
                (action, self.ink_tool),
                (Action::Pen, InkTool::Pen)
                    | (Action::Erase, InkTool::Erase)
                    | (Action::Lasso, InkTool::Lasso)
            );
            draw_button(fb, rect, action.label(), active);
        }
        if !self.status.is_empty() {
            // Bottom edge: the only strip of the calendar screen that is
            // neither a toolbar button nor useful writing space.
            fb.draw_text(4, CANVAS_H - 12, &self.status, GRAY, 2);
        }

        let today = self.today();
        let cells = self.grid_cells();
        if self.state.config.view_mode == ViewMode::Month {
            draw_vertical_text(
                fb,
                8,
                TOOLBAR_H + (CANVAS_H - TOOLBAR_H) / 2,
                &self.state.config.anchor_date.format("%B").to_string(),
                BODY_TEXT_SCALE,
            );
        }
        for (index, cell) in cells.iter().enumerate() {
            fb.draw_rect_outline(cell.rect, GRAY);
            if cell.date == today {
                // Double outline marks the real current date.
                fb.draw_rect_outline(
                    view::Rect {
                        x: cell.rect.x + 1,
                        y: cell.rect.y + 1,
                        w: cell.rect.w - 2,
                        h: cell.rect.h - 2,
                    },
                    BLACK,
                );
            }
            let day_label = format!("{}", cell.date.day());
            let label_gray = if cell.in_focus_period { BLACK } else { GRAY };
            fb.draw_text(cell.rect.x + 4, cell.rect.y + 4, &day_label, label_gray, 2);

            // Event summaries, one line each, below the day number.
            let mut text_y = cell.rect.y + 24;
            for event in self.events_for(cell.date) {
                if text_y + 12 > cell.rect.y + cell.rect.h {
                    break;
                }
                let summary = fit_text(&event.summary, cell.rect.w - 8, EVENT_TEXT_SCALE);
                fb.draw_text(cell.rect.x + 4, text_y, &summary, BLACK, EVENT_TEXT_SCALE);
                text_y += 12;
            }

            // Handwritten ink, denormalized back into this cell's rect —
            // the same stroke data renders correctly at whatever size this
            // cell happens to be in the active view.
            for stroke in self.state.ink.strokes_for(cell.date) {
                let mut prev: Option<(i32, i32)> = None;
                for point in &stroke.points {
                    let (px, py) = view::denormalize_within(cell.rect, point.x, point.y);
                    if let Some((ox, oy)) = prev {
                        fb.draw_line(ox, oy, px, py, BLACK, INK_THICKNESS);
                    }
                    prev = Some((px, py));
                }
            }
            if self.state.config.view_mode == ViewMode::Month {
                draw_month_boundaries(fb, &cells, index);
            }
        }
    }

    // ---- Settings / source editor -----------------------------------

    fn handle_settings_tap(&mut self, x: i32, y: i32) {
        let layout = self.settings_layout();
        if within(layout.back_button, x, y) {
            self.screen = Screen::Calendar;
            self.editor = None;
            return;
        }
        if within(layout.refresh_button, x, y) {
            self.start_refresh();
            return;
        }
        for row in &layout.source_rows {
            if within(row.delete, x, y) {
                let id = self.state.config.sources[row.index].id.clone();
                self.state.config.sources.remove(row.index);
                self.events_cache.remove(&id);
                if self
                    .google_login
                    .as_ref()
                    .is_some_and(|l| l.source_id == id)
                {
                    self.google_login = None;
                }
                let _ = self.state.save_config();
                return;
            }
            if within(row.toggle, x, y) {
                self.state.config.sources[row.index].enabled =
                    !self.state.config.sources[row.index].enabled;
                let _ = self.state.save_config();
                return;
            }
            if let Some(login) = row.login {
                if within(login, x, y) {
                    let id = self.state.config.sources[row.index].id.clone();
                    self.start_google_login(&id);
                    return;
                }
            }
            if within(row.test, x, y) {
                let id = self.state.config.sources[row.index].id.clone();
                self.start_source_test(&id);
                return;
            }
            if within(row.edit, x, y) {
                self.editor = Some(SourceEditor::new_for_edit(
                    &self.state.config.sources[row.index],
                ));
                self.offset_editor = None;
                return;
            }
        }
        for (kind, rect) in &layout.add_buttons {
            if within(*rect, x, y) {
                self.editor = Some(SourceEditor::new_for_add(*kind));
                self.offset_editor = None;
                return;
            }
        }
        for (field, rect) in &layout.editor_fields {
            if within(*rect, x, y) {
                if let Some(editor) = &mut self.editor {
                    editor.focus = *field;
                }
                self.offset_editor = None;
                self.status = "USE APPLOAD KEYBOARD BUTTON".to_string();
                return;
            }
        }
        if let Some(save) = layout.save_button {
            if within(save, x, y) {
                if let Some(editor) = self.editor.take() {
                    let is_edit = editor.editing_id.is_some();
                    let id = editor
                        .editing_id
                        .clone()
                        .unwrap_or_else(|| self.generate_source_id());
                    let source = editor.build_source(id);
                    if is_edit {
                        if let Some(existing) = self
                            .state
                            .config
                            .sources
                            .iter_mut()
                            .find(|s| s.id == source.id)
                        {
                            *existing = source;
                        }
                    } else {
                        self.state.config.sources.push(source);
                    }
                    let _ = self.state.save_config();
                }
                return;
            }
        }
        if let Some(cancel) = layout.cancel_button {
            if within(cancel, x, y) {
                self.editor = None;
            }
        }
        if within(layout.offset_row, x, y) {
            if self.offset_editor.is_none() {
                self.offset_editor = Some(TextField::new(
                    self.state.config.utc_offset_minutes.to_string(),
                ));
            }
            self.status = "USE APPLOAD KEYBOARD BUTTON".to_string();
            return;
        }
        if let Some(save) = layout.offset_save_button {
            if within(save, x, y) {
                if let Some(field) = self.offset_editor.take() {
                    if let Ok(minutes) = field.text.trim().parse::<i32>() {
                        self.state.config.utc_offset_minutes =
                            calnotes_core::timeutil::UtcOffset::new(minutes).minutes;
                        let _ = self.state.save_config();
                    }
                }
            }
        }
    }

    /// Generate a new, collision-resistant source id.
    ///
    /// Ids must be unique for the lifetime of the config (they key the
    /// offline event caches on disk), including across restarts and after
    /// sources have been deleted — a plain `sources.len()` counter reuses
    /// ids and would make a new source inherit a deleted one's cache. This
    /// combines the current wall-clock time in milliseconds with a
    /// per-process counter, and finally checks the result against the ids
    /// already in use, without pulling in a UUID dependency.
    fn generate_source_id(&mut self) -> String {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        loop {
            self.next_source_seq += 1;
            let candidate = format!("src-{millis:x}-{:x}", self.next_source_seq);
            if !self.state.config.sources.iter().any(|s| s.id == candidate) {
                return candidate;
            }
        }
    }

    fn settings_layout(&self) -> SettingsLayout {
        let back_button = view::Rect {
            x: 20,
            y: 16,
            w: 220,
            h: 88,
        };
        let refresh_button = view::Rect {
            x: 260,
            y: 16,
            w: 280,
            h: 88,
        };
        let offset_row = view::Rect {
            x: 20,
            y: 120,
            w: CANVAS_W - 300,
            h: 80,
        };
        let offset_save_button = self.offset_editor.is_some().then_some(view::Rect {
            x: CANVAS_W - 260,
            y: 120,
            w: 240,
            h: 80,
        });
        let mut y = 260;
        let mut source_rows = Vec::new();
        if self.editor.is_none() {
            for (index, source) in self.state.config.sources.iter().enumerate() {
                let row_rect = view::Rect {
                    x: 20,
                    y,
                    w: CANVAS_W - 40,
                    h: 88,
                };
                let is_google = matches!(source.kind, SourceKind::GoogleCalendar { .. });
                let edit_w = if is_google { 660 } else { 500 };
                source_rows.push(SourceRow {
                    index,
                    edit: view::Rect {
                        x: row_rect.x,
                        y: row_rect.y,
                        w: row_rect.w - edit_w,
                        h: row_rect.h,
                    },
                    login: is_google.then_some(view::Rect {
                        x: row_rect.x + row_rect.w - 650,
                        y: row_rect.y,
                        w: 150,
                        h: row_rect.h,
                    }),
                    test: view::Rect {
                        x: row_rect.x + row_rect.w - 490,
                        y: row_rect.y,
                        w: 150,
                        h: row_rect.h,
                    },
                    toggle: view::Rect {
                        x: row_rect.x + row_rect.w - 330,
                        y: row_rect.y,
                        w: 150,
                        h: row_rect.h,
                    },
                    delete: view::Rect {
                        x: row_rect.x + row_rect.w - 170,
                        y: row_rect.y,
                        w: 170,
                        h: row_rect.h,
                    },
                });
                y += 104;
            }
        }
        y += 24;
        let add_kinds = [
            SourceKindChoice::LocalIcs,
            SourceKindChoice::HttpsIcs,
            SourceKindChoice::Google,
            SourceKindChoice::Icloud,
        ];
        let add_button_w = (CANVAS_W - 40) / add_kinds.len() as i32;
        let add_buttons: Vec<_> = if self.editor.is_none() {
            add_kinds
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    (
                        *k,
                        view::Rect {
                            x: 20 + i as i32 * add_button_w,
                            y,
                            w: add_button_w - 10,
                            h: 88,
                        },
                    )
                })
                .collect()
        } else {
            Vec::new()
        };

        let mut editor_fields = Vec::new();
        let (save_button, cancel_button) = if let Some(editor) = &self.editor {
            y = 280;
            for field in editor.fields_for_kind() {
                editor_fields.push((
                    field,
                    view::Rect {
                        x: 20,
                        y,
                        w: CANVAS_W - 40,
                        h: 104,
                    },
                ));
                y += 120;
            }
            (
                Some(view::Rect {
                    x: 20,
                    y,
                    w: 260,
                    h: 88,
                }),
                Some(view::Rect {
                    x: 300,
                    y,
                    w: 260,
                    h: 88,
                }),
            )
        } else {
            (None, None)
        };

        SettingsLayout {
            back_button,
            refresh_button,
            offset_row,
            offset_save_button,
            source_rows,
            add_buttons,
            editor_fields,
            save_button,
            cancel_button,
        }
    }

    fn render_settings(&self, fb: &mut FrameBuffer) {
        fb.clear(WHITE);
        let layout = self.settings_layout();
        draw_button(fb, layout.back_button, "BACK", false);
        draw_button(fb, layout.refresh_button, "REFRESH", false);

        let offset_text = if let Some(field) = &self.offset_editor {
            format!(
                "UTC OFFSET MINUTES: {}",
                text_with_cursor(&field.text, field.cursor)
            )
        } else {
            let label =
                calnotes_core::timeutil::UtcOffset::new(self.state.config.utc_offset_minutes)
                    .label();
            format!("UTC OFFSET: {label} (TAP TO EDIT)")
        };
        fb.draw_rect_outline(
            layout.offset_row,
            if self.offset_editor.is_some() {
                BLACK
            } else {
                GRAY
            },
        );
        fb.draw_text(
            layout.offset_row.x + 12,
            layout.offset_row.y + 32,
            &fit_text(&offset_text, layout.offset_row.w - 24, BODY_TEXT_SCALE),
            BLACK,
            BODY_TEXT_SCALE,
        );
        if let Some(save) = layout.offset_save_button {
            draw_button(fb, save, "SAVE", false);
        }

        if self.editor.is_none() {
            fb.draw_text(20, 222, "SOURCES", BLACK, BODY_TEXT_SCALE);
        }

        for row in &layout.source_rows {
            let source = &self.state.config.sources[row.index];
            let status = status_label(&source.last_status);
            let label = fit_text(
                &format!("{} {}", source.label, status),
                row.edit.w - 12,
                BODY_TEXT_SCALE,
            );
            fb.draw_rect_outline(row.edit, BLACK);
            fb.draw_text(
                row.edit.x + 8,
                row.edit.y + 34,
                &label,
                BLACK,
                BODY_TEXT_SCALE,
            );
            if let Some(login) = row.login {
                let logged_in = matches!(
                    &source.kind,
                    SourceKind::GoogleCalendar {
                        refresh_token: Some(_),
                        ..
                    }
                );
                draw_button(
                    fb,
                    login,
                    if logged_in { "RE-LOG IN" } else { "LOG IN" },
                    logged_in,
                );
            }
            draw_button(fb, row.test, "TEST", false);
            draw_button(
                fb,
                row.toggle,
                if source.enabled { "ON" } else { "OFF" },
                source.enabled,
            );
            draw_button(fb, row.delete, "DEL", false);
        }

        for (kind, rect) in &layout.add_buttons {
            draw_button(fb, *rect, add_button_label(*kind), false);
        }

        if let Some(login) = &self.google_login {
            let base_y = CANVAS_H - 150;
            for (i, line) in google_login_lines(login).iter().enumerate() {
                let line = fit_text(line, CANVAS_W - 40, BODY_TEXT_SCALE);
                fb.draw_text(20, base_y + i as i32 * 24, &line, BLACK, BODY_TEXT_SCALE);
            }
        }
        if !self.status.is_empty() {
            let status = fit_text(&self.status, CANVAS_W - 40, BODY_TEXT_SCALE);
            fb.draw_text(20, CANVAS_H - 28, &status, BLACK, BODY_TEXT_SCALE);
        }

        if let Some(editor) = &self.editor {
            fb.draw_text(
                20,
                224,
                "EDIT SOURCE - TAP A FIELD, THEN USE APPLOAD KEYBOARD",
                BLACK,
                BODY_TEXT_SCALE,
            );
            for (field, rect) in &layout.editor_fields {
                editor.render_field(fb, *field, *rect);
            }
            if let Some(save) = layout.save_button {
                draw_button(fb, save, "SAVE", false);
            }
            if let Some(cancel) = layout.cancel_button {
                draw_button(fb, cancel, "CANCEL", false);
            }
        }
    }
}

/// The lines shown on the settings screen for an in-progress Google login.
/// The verification URL and user code are shown in full (they are
/// single-use, user-facing values, not secrets); no token ever is.
fn google_login_lines(login: &GoogleLogin) -> Vec<String> {
    match &login.phase {
        GoogleLoginPhase::Requesting => vec!["GOOGLE LOGIN: REQUESTING CODE...".to_string()],
        GoogleLoginPhase::AwaitingApproval {
            user_code,
            verification_url,
        } => vec![
            format!("1. ON ANOTHER DEVICE OPEN: {verification_url}"),
            format!("2. ENTER CODE: {user_code}"),
            "3. WAITING FOR APPROVAL... (KEEP THIS SCREEN OPEN)".to_string(),
        ],
        GoogleLoginPhase::Done => vec!["GOOGLE LOGIN COMPLETE".to_string()],
        GoogleLoginPhase::Failed(message) => {
            vec![format!("GOOGLE LOGIN FAILED: {}", message.to_uppercase())]
        }
    }
}

fn status_label(status: &SourceStatus) -> String {
    match status {
        SourceStatus::NeverSynced => "NEVER SYNCED".to_string(),
        SourceStatus::Ok { event_count, .. } => format!("OK {event_count} EVENTS"),
        SourceStatus::Error { message, .. } => format!("ERROR: {}", message.to_uppercase()),
    }
}

fn add_button_label(kind: SourceKindChoice) -> &'static str {
    match kind {
        SourceKindChoice::LocalIcs => "+ FILE",
        SourceKindChoice::HttpsIcs => "+ URL",
        SourceKindChoice::Google => "+ GOOGLE",
        SourceKindChoice::Icloud => "+ ICLOUD",
    }
}

fn draw_button(fb: &mut FrameBuffer, rect: view::Rect, label: &str, active: bool) {
    fb.draw_rect_outline(rect, BLACK);
    if active {
        fb.fill_rect(
            view::Rect {
                x: rect.x + 2,
                y: rect.y + 2,
                w: rect.w - 4,
                h: rect.h - 4,
            },
            GRAY,
        );
    }
    let scale = (rect.w / (label.chars().count().max(1) as i32 * 4)).clamp(2, UI_TEXT_SCALE);
    let tx = rect.x + ((rect.w - FrameBuffer::text_width(label, scale)) / 2).max(2);
    let ty = rect.y + (rect.h - 5 * scale) / 2;
    fb.draw_text(tx, ty, label, BLACK, scale);
}

fn fit_text(text: &str, max_width: i32, scale: i32) -> String {
    let max_chars = (max_width / (4 * scale)).max(0) as usize;
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    let mut result: String = text.chars().take(max_chars - 3).collect();
    result.push_str("...");
    result
}

fn text_with_cursor(text: &str, cursor: usize) -> String {
    let mut result = String::new();
    for (index, character) in text.chars().enumerate() {
        if index == cursor {
            result.push('|');
        }
        result.push(character);
    }
    if cursor >= text.chars().count() {
        result.push('|');
    }
    result
}

fn draw_vertical_text(fb: &mut FrameBuffer, x: i32, center_y: i32, text: &str, scale: i32) {
    let height = text.chars().count() as i32 * 7 * scale;
    let mut y = center_y - height / 2;
    for character in text.chars() {
        fb.draw_text(x, y, &character.to_string(), BLACK, scale);
        y += 7 * scale;
    }
}

fn draw_month_boundaries(fb: &mut FrameBuffer, cells: &[view::DateCell], index: usize) {
    const COLS: usize = 7;
    const THICKNESS: i32 = 5;
    let cell = cells[index];
    if index % COLS < COLS - 1 && cells[index + 1].date.month() != cell.date.month() {
        fb.fill_rect(
            view::Rect {
                x: cell.rect.x + cell.rect.w - THICKNESS / 2,
                y: cell.rect.y,
                w: THICKNESS,
                h: cell.rect.h,
            },
            BLACK,
        );
    }
    if index + COLS < cells.len() && cells[index + COLS].date.month() != cell.date.month() {
        fb.fill_rect(
            view::Rect {
                x: cell.rect.x,
                y: cell.rect.y + cell.rect.h - THICKNESS / 2,
                w: cell.rect.w,
                h: THICKNESS,
            },
            BLACK,
        );
    }
}

fn within(rect: view::Rect, x: i32, y: i32) -> bool {
    x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
}

struct SourceRow {
    index: usize,
    edit: view::Rect,
    /// "LOG IN" button, present only for Google Calendar sources.
    login: Option<view::Rect>,
    test: view::Rect,
    toggle: view::Rect,
    delete: view::Rect,
}

struct SettingsLayout {
    back_button: view::Rect,
    refresh_button: view::Rect,
    offset_row: view::Rect,
    offset_save_button: Option<view::Rect>,
    source_rows: Vec<SourceRow>,
    add_buttons: Vec<(SourceKindChoice, view::Rect)>,
    editor_fields: Vec<(EditorField, view::Rect)>,
    save_button: Option<view::Rect>,
    cancel_button: Option<view::Rect>,
}

/// Which field of the source-under-edit currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorField {
    Label,
    Path,
    Url,
    ClientId,
    ClientSecret,
    CalendarId,
    AppleId,
    AppSpecificPassword,
    CalendarUrl,
}

/// State for the touchscreen source editor: add/edit calendar sources,
/// with text entry driven entirely by AppLoad virtual keyboard key events
/// (see [`calnotes_core::vkb`]).
pub struct SourceEditor {
    pub editing_id: Option<String>,
    pub kind_being_created: SourceKindChoice,
    pub label: TextField,
    pub path: TextField,
    pub url: TextField,
    pub client_id: TextField,
    pub client_secret: TextField,
    pub calendar_id: TextField,
    pub apple_id: TextField,
    pub app_specific_password: TextField,
    pub calendar_url: TextField,
    pub focus: EditorField,
    /// Carried through an edit so re-saving a Google source never discards
    /// a completed login. The token is never shown or editable in the UI.
    existing_refresh_token: Option<String>,
    /// Carried through an edit so re-saving never resets a source's
    /// sync status to "never synced".
    existing_status: SourceStatus,
    existing_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKindChoice {
    LocalIcs,
    HttpsIcs,
    Google,
    Icloud,
}

impl SourceEditor {
    pub fn new_for_add(kind: SourceKindChoice) -> Self {
        SourceEditor {
            editing_id: None,
            kind_being_created: kind,
            label: TextField::default(),
            path: TextField::default(),
            url: TextField::default(),
            client_id: TextField::default(),
            client_secret: TextField::default(),
            calendar_id: TextField::new("primary"),
            apple_id: TextField::default(),
            app_specific_password: TextField::default(),
            calendar_url: TextField::default(),
            focus: EditorField::Label,
            existing_refresh_token: None,
            existing_status: SourceStatus::NeverSynced,
            existing_enabled: true,
        }
    }

    pub fn new_for_edit(source: &CalendarSource) -> Self {
        let mut editor = match &source.kind {
            SourceKind::LocalIcs { path } => {
                let mut e = Self::new_for_add(SourceKindChoice::LocalIcs);
                e.path = TextField::new(path.clone());
                e
            }
            SourceKind::HttpsIcs { url } => {
                let mut e = Self::new_for_add(SourceKindChoice::HttpsIcs);
                e.url = TextField::new(url.clone());
                e
            }
            SourceKind::GoogleCalendar {
                client_id,
                client_secret,
                calendar_id,
                refresh_token,
            } => {
                let mut e = Self::new_for_add(SourceKindChoice::Google);
                e.client_id = TextField::new(client_id.clone());
                e.client_secret = TextField::new(client_secret.clone());
                e.calendar_id = TextField::new(calendar_id.clone());
                e.existing_refresh_token = refresh_token.clone();
                e
            }
            SourceKind::IcloudCalDav {
                apple_id,
                app_specific_password,
                calendar_url,
            } => {
                let mut e = Self::new_for_add(SourceKindChoice::Icloud);
                e.apple_id = TextField::new(apple_id.clone());
                e.app_specific_password = TextField::new(app_specific_password.clone());
                e.calendar_url = TextField::new(calendar_url.clone());
                e
            }
        };
        editor.label = TextField::new(source.label.clone());
        editor.editing_id = Some(source.id.clone());
        editor.existing_status = source.last_status.clone();
        editor.existing_enabled = source.enabled;
        editor
    }

    fn focused_field(&mut self) -> &mut TextField {
        match self.focus {
            EditorField::Label => &mut self.label,
            EditorField::Path => &mut self.path,
            EditorField::Url => &mut self.url,
            EditorField::ClientId => &mut self.client_id,
            EditorField::ClientSecret => &mut self.client_secret,
            EditorField::CalendarId => &mut self.calendar_id,
            EditorField::AppleId => &mut self.apple_id,
            EditorField::AppSpecificPassword => &mut self.app_specific_password,
            EditorField::CalendarUrl => &mut self.calendar_url,
        }
    }

    /// Feed one decoded VKB key into whichever field currently has focus,
    /// or advance focus on `Tab`.
    pub fn handle_key(&mut self, key: VkbKey) {
        if key == VkbKey::Tab {
            self.focus = self.next_field(self.focus);
            return;
        }
        self.focused_field().apply_key(key);
    }

    fn fields_for_kind(&self) -> Vec<EditorField> {
        let mut fields = vec![EditorField::Label];
        fields.extend(match self.kind_being_created {
            SourceKindChoice::LocalIcs => vec![EditorField::Path],
            SourceKindChoice::HttpsIcs => vec![EditorField::Url],
            SourceKindChoice::Google => {
                vec![
                    EditorField::ClientId,
                    EditorField::ClientSecret,
                    EditorField::CalendarId,
                ]
            }
            SourceKindChoice::Icloud => vec![
                EditorField::AppleId,
                EditorField::AppSpecificPassword,
                EditorField::CalendarUrl,
            ],
        });
        fields
    }

    fn next_field(&self, current: EditorField) -> EditorField {
        let fields = self.fields_for_kind();
        let idx = fields.iter().position(|f| *f == current).unwrap_or(0);
        fields[(idx + 1) % fields.len()]
    }

    /// Build the [`CalendarSource`] this editor currently describes, under
    /// the given id (the existing one when editing, a freshly generated
    /// one when adding).
    ///
    /// Editing preserves everything the editor does not expose: a Google
    /// source's refresh token, the source's enabled flag, and its last
    /// sync status.
    pub fn build_source(&self, id: String) -> CalendarSource {
        let kind = match self.kind_being_created {
            SourceKindChoice::LocalIcs => SourceKind::LocalIcs {
                path: self.path.text.clone(),
            },
            SourceKindChoice::HttpsIcs => SourceKind::HttpsIcs {
                url: self.url.text.clone(),
            },
            SourceKindChoice::Google => SourceKind::GoogleCalendar {
                client_id: self.client_id.text.clone(),
                client_secret: self.client_secret.text.clone(),
                calendar_id: self.calendar_id.text.clone(),
                refresh_token: self.existing_refresh_token.clone(),
            },
            SourceKindChoice::Icloud => SourceKind::IcloudCalDav {
                apple_id: self.apple_id.text.clone(),
                app_specific_password: self.app_specific_password.text.clone(),
                calendar_url: self.calendar_url.text.clone(),
            },
        };
        CalendarSource {
            id,
            label: self.label.text.clone(),
            enabled: self.existing_enabled,
            kind,
            last_status: self.existing_status.clone(),
        }
    }

    fn render_field(&self, fb: &mut FrameBuffer, field: EditorField, rect: view::Rect) {
        let (name, value, secret) = match field {
            EditorField::Label => ("LABEL", &self.label, false),
            EditorField::Path => ("ICS FILE PATH", &self.path, false),
            EditorField::Url => ("ICS URL", &self.url, false),
            EditorField::ClientId => ("GOOGLE CLIENT ID", &self.client_id, false),
            EditorField::ClientSecret => ("GOOGLE CLIENT SECRET", &self.client_secret, true),
            EditorField::CalendarId => ("GOOGLE CALENDAR ID", &self.calendar_id, false),
            EditorField::AppleId => ("APPLE ID", &self.apple_id, false),
            EditorField::AppSpecificPassword => (
                "ICLOUD APP-SPECIFIC PASSWORD",
                &self.app_specific_password,
                true,
            ),
            EditorField::CalendarUrl => ("ICLOUD CALENDAR URL", &self.calendar_url, false),
        };
        let focused = self.focus == field;
        fb.draw_rect_outline(rect, if focused { BLACK } else { GRAY });
        if focused {
            fb.draw_rect_outline(
                view::Rect {
                    x: rect.x + 2,
                    y: rect.y + 2,
                    w: rect.w - 4,
                    h: rect.h - 4,
                },
                BLACK,
            );
        }
        fb.draw_text(rect.x + 12, rect.y + 12, name, BLACK, BODY_TEXT_SCALE);
        let raw = if secret {
            calnotes_core::config::mask_secret(&value.text)
        } else {
            value.text.clone()
        };
        let shown = if focused {
            text_with_cursor(&raw, value.cursor.min(raw.chars().count()))
        } else {
            raw
        };
        fb.draw_text(
            rect.x + 12,
            rect.y + 62,
            &fit_text(&shown, rect.w - 24, BODY_TEXT_SCALE),
            BLACK,
            BODY_TEXT_SCALE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calnotes_core::persistence;

    fn with_temp_data_dir<T>(f: impl FnOnce() -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        let result = f();
        std::env::remove_var(persistence::DATA_DIR_ENV);
        result
    }

    #[test]
    #[serial_test::serial]
    fn toolbar_tap_switches_view_mode() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            let (_, week_button) = app.view_buttons()[1];
            app.handle_touch_tap(week_button.x + 5, week_button.y + 5);
            assert_eq!(app.state.config.view_mode, ViewMode::Week);
        });
    }

    #[test]
    #[serial_test::serial]
    fn action_buttons_navigate_and_open_settings() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            let start = app.state.config.anchor_date;
            let (next_action, next_button) = app.action_buttons()[3];
            assert_eq!(next_action, Action::Next);
            app.handle_touch_tap(next_button.x + 5, next_button.y + 5);
            assert!(app.state.config.anchor_date > start);

            let (_, settings_button) = app.action_buttons()[0]; // Action::Settings
            app.handle_touch_tap(settings_button.x + 5, settings_button.y + 5);
            assert_eq!(app.screen, Screen::Settings);
        });
    }

    #[test]
    #[serial_test::serial]
    fn utc_offset_can_be_edited_and_saved_from_settings() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.screen = Screen::Settings;
            assert_eq!(app.state.config.utc_offset_minutes, 0);

            let layout = app.settings_layout();
            app.handle_touch_tap(layout.offset_row.x + 2, layout.offset_row.y + 2);
            assert!(app.offset_editor.is_some());

            // Clear the pre-filled "0" and type "-300".
            app.offset_editor
                .as_mut()
                .unwrap()
                .apply_key(VkbKey::Backspace);
            for c in "-300".chars() {
                app.handle_vkb(c as i32);
            }

            let layout = app.settings_layout();
            let save = layout.offset_save_button.unwrap();
            app.handle_touch_tap(save.x + 2, save.y + 2);
            assert_eq!(app.state.config.utc_offset_minutes, -300);
            assert!(app.offset_editor.is_none());
        });
    }

    #[test]
    #[serial_test::serial]
    fn tapping_a_month_cell_drills_down_to_day_view() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Month);
            let cells = app.grid_cells();
            let target = cells[10].date;
            let cx = cells[10].rect.x + 5;
            let cy = cells[10].rect.y + 5;
            app.handle_touch_tap(cx, cy);
            assert_eq!(app.state.config.view_mode, ViewMode::Day);
            assert_eq!(app.state.config.anchor_date, target);
        });
    }

    #[test]
    #[serial_test::serial]
    fn pen_stroke_round_trips_through_ink_store_and_renders() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Day);
            app.pen_down(100, 500, 1.0);
            app.pen_move(120, 520, 1.0);
            app.pen_up();
            assert_eq!(
                app.state
                    .ink
                    .strokes_for(app.state.config.anchor_date)
                    .len(),
                1
            );
            let fb = app.render();
            // A page with an ink stroke drawn should not be pure white.
            assert!(fb.as_rgb565_bytes().iter().any(|&b| b != 0xFF));
        });
    }

    #[test]
    #[serial_test::serial]
    fn pen_input_above_toolbar_is_ignored() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.pen_down(100, 10, 1.0); // inside the toolbar rows
            assert!(app.active_gesture.is_none());
        });
    }

    #[test]
    #[serial_test::serial]
    fn undo_and_clear_day_affect_only_the_anchor_date() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Day);
            app.pen_down(100, 500, 1.0);
            app.pen_move(110, 510, 1.0);
            app.pen_up();
            let date = app.state.config.anchor_date;
            assert_eq!(app.state.ink.strokes_for(date).len(), 1);
            app.undo_current_day();
            assert_eq!(app.state.ink.strokes_for(date).len(), 0);
        });
    }

    #[test]
    #[serial_test::serial]
    fn settings_screen_add_edit_delete_enable_and_refresh_flow() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.screen = Screen::Settings;

            // Add a new local .ics source via touch + VKB text entry.
            let layout = app.settings_layout();
            let (kind, rect) = layout.add_buttons[0];
            assert_eq!(kind, SourceKindChoice::LocalIcs);
            app.handle_touch_tap(rect.x + 2, rect.y + 2);
            assert!(app.editor.is_some());
            for c in "Home".chars() {
                app.handle_vkb(c as i32);
            }
            app.editor.as_mut().unwrap().handle_key(VkbKey::Tab);
            for c in "/tmp/home.ics".chars() {
                app.handle_vkb(c as i32);
            }

            let layout = app.settings_layout();
            let save = layout.save_button.unwrap();
            app.handle_touch_tap(save.x + 2, save.y + 2);
            assert_eq!(app.state.config.sources.len(), 1);
            assert_eq!(app.state.config.sources[0].label, "Home");
            assert!(app.editor.is_none());

            // Toggle enable off.
            let layout = app.settings_layout();
            let toggle = layout.source_rows[0].toggle;
            app.handle_touch_tap(toggle.x + 2, toggle.y + 2);
            assert!(!app.state.config.sources[0].enabled);

            // Edit it back on and open the editor again.
            let layout = app.settings_layout();
            let edit = layout.source_rows[0].edit;
            app.handle_touch_tap(edit.x + 2, edit.y + 2);
            assert!(app.editor.is_some());
            assert_eq!(app.editor.as_ref().unwrap().label.text, "Home");

            // Cancel instead of saving.
            let layout = app.settings_layout();
            let cancel = layout.cancel_button.unwrap();
            app.handle_touch_tap(cancel.x + 2, cancel.y + 2);
            assert!(app.editor.is_none());

            // Delete it.
            let layout = app.settings_layout();
            let delete = layout.source_rows[0].delete;
            app.handle_touch_tap(delete.x + 2, delete.y + 2);
            assert!(app.state.config.sources.is_empty());

            // Back button returns to the calendar screen.
            let layout = app.settings_layout();
            app.handle_touch_tap(layout.back_button.x + 2, layout.back_button.y + 2);
            assert_eq!(app.screen, Screen::Calendar);
        });
    }

    #[test]
    fn source_editor_tab_cycles_through_fields_for_google_source() {
        let mut editor = SourceEditor::new_for_add(SourceKindChoice::Google);
        assert_eq!(editor.focus, EditorField::Label);
        editor.handle_key(VkbKey::Tab);
        assert_eq!(editor.focus, EditorField::ClientId);
        editor.handle_key(VkbKey::Tab);
        assert_eq!(editor.focus, EditorField::ClientSecret);
        editor.handle_key(VkbKey::Tab);
        assert_eq!(editor.focus, EditorField::CalendarId);
        editor.handle_key(VkbKey::Tab);
        assert_eq!(editor.focus, EditorField::Label);
    }

    #[test]
    fn source_editor_types_into_focused_field() {
        let mut editor = SourceEditor::new_for_add(SourceKindChoice::HttpsIcs);
        editor.handle_key(VkbKey::Tab); // Label -> Url
        for c in "https://example.com/cal.ics".chars() {
            editor.handle_key(VkbKey::Char(c));
        }
        assert_eq!(editor.url.text, "https://example.com/cal.ics");
    }

    #[test]
    fn source_editor_builds_matching_calendar_source() {
        let mut editor = SourceEditor::new_for_add(SourceKindChoice::LocalIcs);
        for c in "My Calendar".chars() {
            editor.handle_key(VkbKey::Char(c));
        }
        editor.handle_key(VkbKey::Tab);
        for c in "/home/root/cal.ics".chars() {
            editor.handle_key(VkbKey::Char(c));
        }
        let source = editor.build_source("generated-id".to_string());
        assert_eq!(source.label, "My Calendar");
        assert!(
            matches!(source.kind, SourceKind::LocalIcs { path } if path == "/home/root/cal.ics")
        );
    }

    #[test]
    fn editing_existing_source_prefills_fields() {
        let existing = CalendarSource {
            id: "s1".into(),
            label: "Work".into(),
            enabled: true,
            kind: SourceKind::HttpsIcs {
                url: "https://example.com/work.ics".into(),
            },
            last_status: SourceStatus::NeverSynced,
        };
        let editor = SourceEditor::new_for_edit(&existing);
        assert_eq!(editor.label.text, "Work");
        assert_eq!(editor.url.text, "https://example.com/work.ics");
        assert_eq!(editor.editing_id, Some("s1".to_string()));
    }

    #[test]
    fn editing_a_google_source_preserves_its_refresh_token_and_status() {
        let synced_at = chrono::NaiveDate::from_ymd_opt(2026, 3, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let existing = CalendarSource {
            id: "g1".into(),
            label: "Personal".into(),
            enabled: false,
            kind: SourceKind::GoogleCalendar {
                client_id: "cid".into(),
                client_secret: "secret".into(),
                calendar_id: "primary".into(),
                refresh_token: Some("persisted-token".into()),
            },
            last_status: SourceStatus::Ok {
                synced_at_utc: synced_at,
                event_count: 7,
            },
        };
        let mut editor = SourceEditor::new_for_edit(&existing);
        // Rename the source; everything the editor does not expose must
        // survive the round trip.
        editor.focus = EditorField::Label;
        editor.handle_key(VkbKey::Char('!'));
        let rebuilt = editor.build_source("g1".to_string());

        assert_eq!(rebuilt.id, "g1");
        assert_eq!(rebuilt.label, "Personal!");
        assert!(!rebuilt.enabled);
        assert_eq!(
            rebuilt.last_status,
            SourceStatus::Ok {
                synced_at_utc: synced_at,
                event_count: 7
            }
        );
        match rebuilt.kind {
            SourceKind::GoogleCalendar { refresh_token, .. } => {
                assert_eq!(refresh_token.as_deref(), Some("persisted-token"));
            }
            _ => panic!("expected a Google source"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn saving_an_edit_through_the_settings_screen_keeps_token_and_status() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.screen = Screen::Settings;
            app.state.config.sources.push(CalendarSource {
                id: "g1".into(),
                label: "G".into(),
                enabled: true,
                kind: SourceKind::GoogleCalendar {
                    client_id: "cid".into(),
                    client_secret: "secret".into(),
                    calendar_id: "primary".into(),
                    refresh_token: Some("token".into()),
                },
                last_status: SourceStatus::Ok {
                    synced_at_utc: chrono::NaiveDate::from_ymd_opt(2026, 3, 1)
                        .unwrap()
                        .and_hms_opt(0, 0, 0)
                        .unwrap(),
                    event_count: 3,
                },
            });

            let layout = app.settings_layout();
            let edit = layout.source_rows[0].edit;
            app.handle_touch_tap(edit.x + 2, edit.y + 2);
            assert!(app.editor.is_some());
            let layout = app.settings_layout();
            let save = layout.save_button.unwrap();
            app.handle_touch_tap(save.x + 2, save.y + 2);

            assert_eq!(app.state.config.sources.len(), 1);
            let saved = &app.state.config.sources[0];
            assert!(matches!(
                &saved.kind,
                SourceKind::GoogleCalendar { refresh_token: Some(t), .. } if t == "token"
            ));
            assert!(matches!(saved.last_status, SourceStatus::Ok { .. }));
        });
    }

    #[test]
    #[serial_test::serial]
    fn generated_source_ids_are_unique_and_not_reused_after_deletion() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            let mut ids = Vec::new();
            for _ in 0..5 {
                let id = app.generate_source_id();
                assert!(!ids.contains(&id), "duplicate id generated: {id}");
                app.state.config.sources.push(CalendarSource {
                    id: id.clone(),
                    label: "x".into(),
                    enabled: true,
                    kind: SourceKind::LocalIcs { path: "x".into() },
                    last_status: SourceStatus::NeverSynced,
                });
                ids.push(id);
            }
            // Deleting the last source must not make the next id collide
            // with it (which a `sources.len()` counter would).
            app.state.config.sources.pop();
            let next = app.generate_source_id();
            assert!(!ids.contains(&next));
        });
    }

    #[test]
    #[serial_test::serial]
    fn fresh_install_anchors_on_today_at_the_configured_offset() {
        with_temp_data_dir(|| {
            let app = App::new().unwrap();
            let expected = calnotes_core::timeutil::UtcOffset::new(0).today();
            assert_eq!(app.state.config.anchor_date, expected);
            assert!(!calnotes_core::model::is_unset_anchor(
                app.state.config.anchor_date
            ));
        });
    }

    #[test]
    #[serial_test::serial]
    fn today_button_returns_to_the_current_date_after_navigating_away() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            let today = app.today();
            app.navigate(5);
            assert_ne!(app.state.config.anchor_date, today);
            let (action, rect) = app.action_buttons()[2];
            assert_eq!(action, Action::Today);
            app.handle_touch_tap(rect.x + 5, rect.y + 5);
            assert_eq!(app.state.config.anchor_date, today);
        });
    }

    #[test]
    #[serial_test::serial]
    fn fetch_window_is_wider_than_the_visible_window_on_both_sides() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Week);
            let visible = app.window();
            let fetch = app.fetch_window();
            assert!(fetch.start < visible.start);
            assert!(fetch.end > visible.end);
            // A page of navigation stays inside the fetched window, so the
            // moved view is never blank waiting for the network.
            app.navigate(1);
            let moved = app.window();
            assert!(fetch.start <= moved.start && fetch.end >= moved.end);
        });
    }

    #[test]
    #[serial_test::serial]
    fn navigation_within_the_cached_window_does_not_start_a_refresh() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Week);
            // Pretend a refresh already covered the padded window.
            app.apply_refresh(RefreshOutcome {
                sources: Vec::new(),
                events: HashMap::new(),
                window: app.fetch_window(),
            });
            assert!(app.refresh_rx.is_none());
            app.navigate(1);
            assert!(
                app.refresh_rx.is_none(),
                "a one-page move should be served from the wider cached window"
            );
            // Jumping far outside it does need fresh data.
            app.navigate(40);
            assert!(app.refresh_rx.is_some());
        });
    }

    #[test]
    #[serial_test::serial]
    fn pen_move_reports_one_small_dirty_rect_per_sample() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Day);
            app.pen_down(300, 500, 1.0);
            let segment = app.pen_move(304, 503, 1.0).expect("a segment to draw");
            assert_eq!((segment.x0, segment.y0), (300, 500));
            assert_eq!((segment.x1, segment.y1), (304, 503));
            let dirty = segment.dirty_rect();
            // Small: nothing like the 1404x1872 full screen.
            assert!(
                dirty.w < 20 && dirty.h < 20,
                "dirty rect too large: {dirty:?}"
            );
            assert!(dirty.x <= 300 && dirty.y <= 500);
            assert!(dirty.x + dirty.w >= 305 && dirty.y + dirty.h >= 504);

            // With no pen down there is nothing to draw.
            app.pen_up();
            assert!(app.pen_move(120, 520, 1.0).is_none());
        });
    }

    #[test]
    #[serial_test::serial]
    fn incremental_segments_match_a_full_re_render_of_the_same_stroke() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Day);

            // Start from the rendered "before" frame and apply only the
            // incremental segments, exactly as the device loop does.
            let mut incremental = app.render();
            app.pen_down(200, 600, 1.0);
            for (x, y) in [(210, 610), (225, 620), (240, 615)] {
                let segment = app.pen_move(x, y, 1.0).unwrap();
                incremental.draw_line(
                    segment.x0,
                    segment.y0,
                    segment.x1,
                    segment.y1,
                    calnotes_core::render::BLACK,
                    segment.thickness,
                );
            }
            app.pen_up();

            let full = app.render();
            assert_eq!(
                incremental.as_rgb565_bytes(),
                full.as_rgb565_bytes(),
                "incremental pen drawing must be pixel-identical to a full re-render"
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn eraser_tool_removes_a_touched_stroke_and_requests_redraw() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Day);
            app.pen_down(300, 700, 1.0);
            app.pen_move(500, 700, 1.0).unwrap();
            assert!(!app.pen_up());
            assert_eq!(
                app.state
                    .ink
                    .strokes_for(app.state.config.anchor_date)
                    .len(),
                1
            );

            app.ink_tool = InkTool::Erase;
            app.pen_down(390, 680, 1.0);
            assert!(app.pen_move(410, 720, 1.0).is_none());
            assert!(app.pen_up());
            assert!(app
                .state
                .ink
                .strokes_for(app.state.config.anchor_date)
                .is_empty());
            app.undo_current_day();
            assert_eq!(
                app.state
                    .ink
                    .strokes_for(app.state.config.anchor_date)
                    .len(),
                1
            );
        });
    }

    #[test]
    #[serial_test::serial]
    fn lasso_tool_removes_only_the_enclosed_stroke() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Day);
            for (from, to) in [((300, 700), (360, 760)), ((900, 1100), (960, 1160))] {
                app.pen_down(from.0, from.1, 1.0);
                app.pen_move(to.0, to.1, 1.0).unwrap();
                app.pen_up();
            }
            app.ink_tool = InkTool::Lasso;
            app.pen_down(250, 650, 1.0);
            for (x, y) in [(420, 650), (420, 820), (250, 820), (250, 650)] {
                app.pen_move(x, y, 1.0).unwrap();
            }
            assert!(app.pen_up());
            let remaining = app.state.ink.strokes_for(app.state.config.anchor_date);
            assert_eq!(remaining.len(), 1);
            assert!(remaining[0].points[0].x > 0.5);
        });
    }

    #[test]
    #[serial_test::serial]
    fn tapping_an_editor_field_moves_visible_keyboard_focus() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.screen = Screen::Settings;
            app.editor = Some(SourceEditor::new_for_add(SourceKindChoice::Google));
            let layout = app.settings_layout();
            let (_, secret_rect) = layout
                .editor_fields
                .iter()
                .find(|(field, _)| *field == EditorField::ClientSecret)
                .unwrap();
            app.handle_touch_tap(secret_rect.x + 10, secret_rect.y + 10);
            assert_eq!(
                app.editor.as_ref().unwrap().focus,
                EditorField::ClientSecret
            );
            assert_eq!(app.status, "USE APPLOAD KEYBOARD BUTTON");
        });
    }

    #[test]
    #[serial_test::serial]
    fn applying_a_refresh_updates_status_events_and_cached_window() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.state.config.sources.push(CalendarSource {
                id: "s1".into(),
                label: "S".into(),
                enabled: true,
                kind: SourceKind::LocalIcs {
                    path: "x.ics".into(),
                },
                last_status: SourceStatus::NeverSynced,
            });
            let date = app.state.config.anchor_date;
            let mut events = HashMap::new();
            events.insert(
                "s1".to_string(),
                vec![Event {
                    id: "e1".into(),
                    source_id: "s1".into(),
                    summary: "Lunch".into(),
                    location: None,
                    time: calnotes_core::model::EventTime::AllDay {
                        start: date,
                        end_exclusive: date.succ_opt().unwrap(),
                    },
                }],
            );
            let mut synced = app.state.config.sources.clone();
            synced[0].last_status = SourceStatus::Ok {
                synced_at_utc: chrono::Utc::now().naive_utc(),
                event_count: 1,
            };
            let window = app.fetch_window();
            app.apply_refresh(RefreshOutcome {
                sources: synced,
                events,
                window,
            });

            assert_eq!(app.events_for(date).len(), 1);
            assert_eq!(app.status, "SYNCED 1 SOURCES");
            assert!(matches!(
                app.state.config.sources[0].last_status,
                SourceStatus::Ok { .. }
            ));
            assert_eq!(app.cached_window, Some(window));
        });
    }

    #[test]
    #[serial_test::serial]
    fn refresh_result_for_a_deleted_source_is_ignored() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            let ghost = CalendarSource {
                id: "gone".into(),
                label: "Gone".into(),
                enabled: true,
                kind: SourceKind::LocalIcs {
                    path: "x.ics".into(),
                },
                last_status: SourceStatus::NeverSynced,
            };
            let mut events = HashMap::new();
            events.insert("gone".to_string(), Vec::new());
            app.apply_refresh(RefreshOutcome {
                sources: vec![ghost],
                events,
                window: app.fetch_window(),
            });
            assert!(app.state.config.sources.is_empty());
            assert!(app.events_cache.is_empty());
        });
    }

    #[test]
    #[serial_test::serial]
    fn google_login_is_only_offered_for_google_sources_and_needs_credentials() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.screen = Screen::Settings;
            app.state.config.sources.push(CalendarSource {
                id: "local".into(),
                label: "L".into(),
                enabled: true,
                kind: SourceKind::LocalIcs {
                    path: "x.ics".into(),
                },
                last_status: SourceStatus::NeverSynced,
            });
            app.state.config.sources.push(CalendarSource {
                id: "g1".into(),
                label: "G".into(),
                enabled: true,
                kind: SourceKind::GoogleCalendar {
                    client_id: String::new(),
                    client_secret: String::new(),
                    calendar_id: "primary".into(),
                    refresh_token: None,
                },
                last_status: SourceStatus::NeverSynced,
            });

            let layout = app.settings_layout();
            assert!(layout.source_rows[0].login.is_none());
            let login = layout.source_rows[1].login.expect("google login button");

            // Without client credentials nothing is started; the user is
            // told what is missing instead of a worker silently failing.
            app.handle_touch_tap(login.x + 2, login.y + 2);
            assert!(app.google_login.is_none());
            assert_eq!(app.status, "ENTER CLIENT ID AND SECRET FIRST");
        });
    }

    #[test]
    #[serial_test::serial]
    fn a_completed_google_login_persists_the_refresh_token() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.state.config.sources.push(CalendarSource {
                id: "g1".into(),
                label: "G".into(),
                enabled: true,
                kind: SourceKind::GoogleCalendar {
                    client_id: "cid".into(),
                    client_secret: "secret".into(),
                    calendar_id: "primary".into(),
                    refresh_token: None,
                },
                last_status: SourceStatus::NeverSynced,
            });
            app.store_google_refresh_token("g1", "fresh-token".to_string());

            assert!(matches!(
                &app.state.config.sources[0].kind,
                SourceKind::GoogleCalendar { refresh_token: Some(t), .. } if t == "fresh-token"
            ));
            // ...and survives a reload from disk.
            let reloaded = App::new().unwrap();
            assert!(matches!(
                &reloaded.state.config.sources[0].kind,
                SourceKind::GoogleCalendar { refresh_token: Some(t), .. } if t == "fresh-token"
            ));
        });
    }

    #[test]
    #[serial_test::serial]
    fn google_login_phase_lines_never_include_a_token() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let login = GoogleLogin {
            source_id: "g1".into(),
            phase: GoogleLoginPhase::AwaitingApproval {
                user_code: "ABCD-EFGH".into(),
                verification_url: "https://www.google.com/device".into(),
            },
            rx,
        };
        let lines = google_login_lines(&login);
        assert!(lines.iter().any(|l| l.contains("ABCD-EFGH")));
        assert!(lines.iter().any(|l| l.contains("google.com/device")));
        assert!(!lines.iter().any(|l| l.to_lowercase().contains("token")));
    }
}
