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
use calnotes_core::model::{AppConfig, CalendarSource, Event, SourceKind, SourceStatus, ViewMode};
use calnotes_core::recurrence::Window;
use calnotes_core::render::{Font, FrameBuffer, BLACK, GRAY, LIGHT_GRAY, WHITE};
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
pub const TOOLBAR_ROW_H: i32 = 96;
const TOOLBAR_H: i32 = TOOLBAR_ROW_H * 3;
const MONTH_LABEL_W: i32 = 72;
const UI_TEXT_SCALE: i32 = 4;
const BODY_TEXT_SCALE: i32 = 3;
const EVENT_TEXT_SCALE: i32 = 2;
/// Day-number size inside a calendar cell — a little smaller than the
/// toolbar button text (`UI_TEXT_SCALE`) so the dates read clearly.
const DAY_NUMBER_SCALE: i32 = 3;
/// Vertical month-name label size (down the left gutter in Month view) —
/// deliberately large for at-a-glance readability.
const MONTH_LABEL_SCALE: i32 = 6;
/// Width of the transient eraser feedback trail, in canvas pixels.
const ERASER_FEEDBACK_THICKNESS: i32 = 8;

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
            Action::Settings => "SETTINGS",
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
        rect: view::Rect,
        stroke_index: usize,
        last_drawn: (i32, i32),
    },
    Erase {
        date: NaiveDate,
        rect: view::Rect,
        points: Vec<NormPoint>,
        last_drawn: (i32, i32),
    },
    Lasso {
        date: NaiveDate,
        rect: view::Rect,
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
    /// Grey level to draw this segment with (`BLACK` for real pen ink,
    /// lighter greys for transient lasso/eraser feedback).
    pub gray: u8,
    /// Whether to draw the segment as a dashed line (used for the lasso
    /// selection outline, so it never looks like real ink).
    pub dashed: bool,
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
    /// Editable text field for the event text size, shown on the settings
    /// screen. `Some` while the user is typing a size directly.
    pub event_size_editor: Option<TextField>,
    next_source_seq: u64,
    /// Short, user-visible status line (refresh progress, login progress).
    pub status: String,
    refresh_rx: Option<Receiver<RefreshOutcome>>,
    /// A one-shot worker running an in-editor source TEST (built from the
    /// unsaved editor fields). `Some` while the test is in flight.
    editor_test_rx: Option<Receiver<CalendarSource>>,
    /// Full, multi-line result text of the most recent in-editor TEST,
    /// shown wrapped under the editor's TEST button so long error messages
    /// are fully readable. Cleared whenever the editor opens or closes.
    editor_test_result: Option<String>,
    /// The id of the source whose per-row **TEST** button was last pressed
    /// and whose test is still running. Shown as a "TESTING…" line on that
    /// row so the button press is always visibly acknowledged, even when
    /// the eventual result is unchanged. Cleared when the result arrives.
    testing_source_id: Option<String>,
    pub google_login: Option<GoogleLogin>,
    /// Dates edited (drawn/erased/lasso'd/cleared) in order, so Undo can
    /// reverse the user's actual last action regardless of which cell it
    /// was in — the anchor date is not necessarily where they were writing.
    /// Not persisted: undo history is per session.
    edit_history: Vec<NaiveDate>,
}

impl App {
    pub fn new() -> std::io::Result<Self> {
        let mut state = AppState::load()?;
        if calnotes_core::model::is_unset_anchor(state.config.anchor_date) {
            state.config.anchor_date = UtcOffset::new(state.config.utc_offset_minutes).today();
        }
        // Open on the configured startup view: either the last-used view
        // (the value just loaded from disk) or the chosen default, each
        // clamped to a currently-visible view.
        let last_used = state.config.view_mode;
        state.config.view_mode = state.config.startup_view(last_used);
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
            event_size_editor: None,
            next_source_seq: 0,
            status: String::new(),
            refresh_rx: None,
            editor_test_rx: None,
            editor_test_result: None,
            testing_source_id: None,
            google_login: None,
            edit_history: Vec::new(),
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
        self.testing_source_id = Some(source_id.to_string());
        self.status = format!("TESTING {}...", label.to_uppercase());
    }

    /// Test the source currently described by the open editor — using its
    /// unsaved field values — on a worker thread. The full result (or full
    /// error message) is stored in `editor_test_result` and shown wrapped
    /// under the editor's TEST button, so the user can read the entire
    /// message rather than a truncated status line.
    fn start_editor_test(&mut self) {
        if self.refresh_rx.is_some() || self.editor_test_rx.is_some() {
            self.editor_test_result = Some("Please wait for the current test to finish.".into());
            return;
        }
        let Some(editor) = &self.editor else {
            return;
        };
        let source = editor.build_source("__editor_test__".to_string());
        let window = self.fetch_window();
        let offset = self.offset();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut source = source;
            let _ = sources::refresh_source(&mut source, window, offset);
            let _ = tx.send(source);
        });
        self.editor_test_rx = Some(rx);
        self.editor_test_result = Some("Testing…".to_string());
        self.status = "TESTING SOURCE...".to_string();
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
        self.testing_source_id = None;
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
                    self.testing_source_id = None;
                    self.status = "REFRESH FAILED".to_string();
                    changed = true;
                }
            }
        }
        if let Some(rx) = &self.editor_test_rx {
            match rx.try_recv() {
                Ok(source) => {
                    self.editor_test_rx = None;
                    self.editor_test_result = Some(full_status_text(&source.last_status));
                    self.status = String::new();
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.editor_test_rx = None;
                    self.editor_test_result = Some("Test failed to run.".to_string());
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
        // Iterate sources in their configured order so that same-day events
        // from different calendars stack in the order the user arranged in
        // settings (via the up/down reorder buttons).
        let mut out = Vec::new();
        for source in &self.state.config.sources {
            if let Some(events) = self.events_cache.get(&source.id) {
                out.extend(events.iter().filter(|e| e.time.dates().contains(&date)));
            }
        }
        // Include any cached events whose source is no longer in the list
        // (defensive: normally cleaned up on delete).
        for (id, events) in &self.events_cache {
            if !self.state.config.sources.iter().any(|s| &s.id == id) {
                out.extend(events.iter().filter(|e| e.time.dates().contains(&date)));
            }
        }
        out
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

    /// Undo the most recent ink edit anywhere on screen — the last stroke
    /// drawn, or the last erase/lasso/clear that removed strokes — not just
    /// edits on the anchor date. Erasing and lassoing are fully undoable.
    pub fn undo_current_day(&mut self) {
        while let Some(date) = self.edit_history.pop() {
            if self.state.ink.undo(date) {
                let _ = self.state.save_ink_day(date);
                return;
            }
        }
        // Fall back to the anchor date if nothing is in the session history
        // (e.g. ink drawn in a previous session).
        let anchor = self.state.config.anchor_date;
        if self.state.ink.undo(anchor) {
            let _ = self.state.save_ink_day(anchor);
        }
    }

    /// Clear a whole day's ink. Targets the date the user was last writing
    /// on (so it works in multi-day views), falling back to the anchor.
    pub fn clear_current_day(&mut self) {
        let date = self
            .edit_history
            .last()
            .copied()
            .unwrap_or(self.state.config.anchor_date);
        self.state.ink.clear_day(date);
        self.edit_history.push(date);
        let _ = self.state.save_ink_day(date);
    }

    /// The reference day-cell aspect ratio shared by every view except Two
    /// Months: a single Month-view cell (7 columns x 6 rows over the writing
    /// area, i.e. the screen minus the toolbar and the month-label gutter).
    fn cell_aspect() -> view::CellAspect {
        view::CellAspect {
            w: (CANVAS_W - MONTH_LABEL_W) / 7,
            h: (CANVAS_H - TOOLBAR_H) / 6,
        }
    }

    fn grid_cells(&self) -> Vec<view::DateCell> {
        let month_gutter = if matches!(
            self.state.config.view_mode,
            ViewMode::Month | ViewMode::TwoMonths
        ) {
            MONTH_LABEL_W
        } else {
            0
        };
        view::layout(
            self.state.config.view_mode,
            self.state.config.anchor_date,
            CANVAS_W - month_gutter,
            CANVAS_H - TOOLBAR_H,
            Self::cell_aspect(),
        )
        .into_iter()
        .map(|mut c| {
            c.rect.x += month_gutter;
            c.rect.y += TOOLBAR_H;
            c
        })
        .collect()
    }

    /// Toolbar button rectangles, one per selected [`ViewMode`], in the
    /// user-configured display order.
    fn view_buttons(&self) -> Vec<(ViewMode, view::Rect)> {
        let modes = self.state.config.ordered_views();
        let button_w = CANVAS_W / modes.len().max(1) as i32;
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

    /// A stylus press on a toolbar button (or anywhere on the settings
    /// screen) behaves like a finger tap, so the pen can operate the UI as
    /// it does everywhere else on the reMarkable. Returns `Some(needs_full_
    /// redraw)` when the press was a UI interaction, or `None` when it falls
    /// on the writing surface and should instead begin an ink stroke.
    pub fn handle_pen_tap(&mut self, x: i32, y: i32) -> Option<bool> {
        if self.screen == Screen::Settings {
            self.handle_settings_tap(x, y);
            return Some(true);
        }
        for (mode, rect) in self.view_buttons() {
            if within(rect, x, y) {
                self.set_view_mode(mode);
                return Some(true);
            }
        }
        for (action, rect) in self.action_buttons() {
            if within(rect, x, y) {
                self.perform_action(action);
                return Some(true);
            }
        }
        None
    }

    /// Whether a touch at `(x, y)` lands on interactive chrome (a toolbar
    /// button, or anywhere on the settings screen) rather than the writing
    /// surface. Chrome touches act immediately so buttons stay responsive;
    /// writing-surface touches go through palm rejection before opening a
    /// day.
    pub fn touch_hits_ui(&self, x: i32, y: i32) -> bool {
        if self.screen == Screen::Settings {
            return true;
        }
        self.view_buttons().iter().any(|(_, r)| within(*r, x, y))
            || self.action_buttons().iter().any(|(_, r)| within(*r, x, y))
    }

    /// Desktop-preview helper: open the settings screen with a sample source
    /// editor so the settings font/layout can be rendered. Not used on
    /// device.
    pub fn show_settings_for_preview(&mut self) {
        self.screen = Screen::Settings;
        let mut editor = SourceEditor::new_for_add(SourceKindChoice::Icloud);
        editor.label = TextField::new("Family iCloud");
        editor.apple_id = TextField::new("john.doe@icloud.com");
        editor.app_specific_password = TextField::new("abcd-efgh-ijkl-mnop");
        editor.calendar_url = TextField::new("https://caldav.icloud.com/1234567/calendars/home/");
        editor.focus = EditorField::CalendarUrl;
        self.editor = Some(editor);
    }

    /// Desktop-preview helper: the settings screen showing the source list
    /// and the Display section (view picker, startup default, event size),
    /// i.e. not the source editor.
    pub fn show_settings_list_for_preview(&mut self) {
        self.screen = Screen::Settings;
        self.editor = None;
        self.state.config.sources = vec![CalendarSource {
            id: "demo".into(),
            label: "Family iCloud".into(),
            enabled: true,
            kind: SourceKind::IcloudCalDav {
                apple_id: "john.doe@icloud.com".into(),
                app_specific_password: "secret".into(),
                calendar_url: "https://caldav.icloud.com/home/".into(),
            },
            last_status: SourceStatus::Ok {
                synced_at_utc: self.today().and_hms_opt(9, 0, 0).unwrap(),
                event_count: 12,
            },
        }];
    }

    /// Desktop-preview helper: drop a few sample handwritten notes onto days
    /// of the current month so a screenshot shows what the ink looks like.
    /// Not used on device.
    pub fn add_demo_scribbles(&mut self) {
        use calnotes_core::ink::NormPoint;
        use std::f32::consts::PI;
        let (year, month) = (self.today().year(), self.today().month());
        let ink = &mut self.state.ink;
        let mut stroke = |day: u32, points: &[(f32, f32)]| {
            if let Some(date) = NaiveDate::from_ymd_opt(year, month, day) {
                let idx = ink.begin_stroke(date);
                for &(x, y) in points {
                    ink.push_point(
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
        };
        // A checkmark.
        stroke(3, &[(0.20, 0.55), (0.40, 0.80), (0.82, 0.22)]);
        // A wavy "note" underline.
        let wave: Vec<(f32, f32)> = (0..=24)
            .map(|i| {
                let t = i as f32 / 24.0;
                (0.12 + 0.76 * t, 0.55 + 0.14 * (t * PI * 3.0).sin())
            })
            .collect();
        stroke(10, &wave);
        // A five-point star (today).
        stroke(
            14,
            &[
                (0.50, 0.18),
                (0.62, 0.55),
                (0.86, 0.55),
                (0.66, 0.72),
                (0.75, 0.86),
                (0.50, 0.63),
                (0.25, 0.86),
                (0.34, 0.72),
                (0.14, 0.55),
                (0.38, 0.55),
                (0.50, 0.18),
            ],
        );
        // A loopy cursive scribble.
        let loops: Vec<(f32, f32)> = (0..=48)
            .map(|i| {
                let t = i as f32 / 48.0;
                (0.12 + 0.76 * t, 0.5 + 0.26 * (t * PI * 4.0).sin())
            })
            .collect();
        stroke(21, &loops);
        // A little checkbox with a tick.
        stroke(
            26,
            &[
                (0.22, 0.35),
                (0.44, 0.35),
                (0.44, 0.62),
                (0.22, 0.62),
                (0.22, 0.35),
            ],
        );
        stroke(26, &[(0.50, 0.48), (0.60, 0.62), (0.82, 0.30)]);
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
                    rect,
                    stroke_index,
                    last_drawn,
                }
            }
            InkTool::Erase => ActiveGesture::Erase {
                date,
                rect,
                points: vec![point],
                last_drawn,
            },
            InkTool::Lasso => ActiveGesture::Lasso {
                date,
                rect,
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
        // A stroke stays bound to the cell it started in — its `rect` is
        // captured once at pen-down, so a pen sample does no per-sample
        // layout work (no grid rebuild, no cell search): just normalize,
        // record, and emit one segment. This keeps the hot path allocation-
        // and compute-free so samples are consumed as fast as they arrive.
        let gesture = self.active_gesture.as_mut()?;
        let (date, rect, last_drawn, gray, thickness, dashed) = match gesture {
            ActiveGesture::Draw {
                date,
                rect,
                last_drawn,
                ..
            } => (*date, *rect, last_drawn, BLACK, INK_THICKNESS, false),
            ActiveGesture::Erase {
                date,
                rect,
                last_drawn,
                ..
            } => (
                *date,
                *rect,
                last_drawn,
                LIGHT_GRAY,
                ERASER_FEEDBACK_THICKNESS,
                false,
            ),
            ActiveGesture::Lasso {
                date,
                rect,
                last_drawn,
                ..
            } => (*date, *rect, last_drawn, GRAY, INK_THICKNESS, true),
        };
        let (px0, py0) = *last_drawn;
        let (nx, ny) = view::normalize_within(rect, x, y);
        let point = NormPoint {
            x: nx,
            y: ny,
            pressure,
        };
        let (px1, py1) = view::denormalize_within(rect, nx, ny);
        *last_drawn = (px1, py1);
        // Each tool produces the same incremental line, styled so the user
        // can tell them apart: solid black ink for the pen, a faint solid
        // trail for the eraser, and a dashed grey outline for the lasso.
        // Only the pen's marks are persisted; the eraser/lasso feedback is
        // wiped by the full redraw on pen-up.
        match gesture {
            ActiveGesture::Draw { stroke_index, .. } => {
                self.state.ink.push_point(date, *stroke_index, point);
            }
            ActiveGesture::Erase { points, .. } | ActiveGesture::Lasso { points, .. } => {
                points.push(point);
            }
        }
        Some(PenSegment {
            x0: px0,
            y0: py0,
            x1: px1,
            y1: py1,
            thickness,
            gray,
            dashed,
        })
    }

    /// Finish the current pen gesture. Returns `true` when temporary ink
    /// or deleted strokes require a full redraw.
    pub fn pen_up(&mut self) -> bool {
        let Some(active) = self.active_gesture.take() else {
            return false;
        };
        let edited_date = match &active {
            ActiveGesture::Draw { date, .. }
            | ActiveGesture::Erase { date, .. }
            | ActiveGesture::Lasso { date, .. } => *date,
        };
        let redraw = match active {
            ActiveGesture::Draw {
                date, stroke_index, ..
            } => {
                // A real mark (>= 2 points) is an undoable edit; a mere tap
                // is discarded and recorded nothing.
                let kept = self
                    .state
                    .ink
                    .strokes_for(date)
                    .get(stroke_index)
                    .is_some_and(|s| !s.is_empty());
                self.state.ink.discard_if_empty(date, stroke_index);
                if kept {
                    self.edit_history.push(date);
                }
                false
            }
            ActiveGesture::Erase { date, points, .. } => {
                if self.state.ink.erase_path(date, &points, 0.035) > 0 {
                    self.edit_history.push(date);
                }
                true
            }
            ActiveGesture::Lasso { date, points, .. } => {
                if self.state.ink.delete_inside_lasso(date, &points) > 0 {
                    self.edit_history.push(date);
                }
                true
            }
        };
        // Persist only the affected day, so save cost stays constant as
        // notes accumulate across other dates.
        let _ = self.state.save_ink_day(edited_date);
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
        } else if self.event_size_editor.is_some() {
            if let Some(field) = &mut self.event_size_editor {
                field.apply_key(key);
            }
            // Commit the typed size live (clamped) so the preview updates.
            // Accept whole or half points (e.g. "3" or "3.5").
            if let Some(field) = &self.event_size_editor {
                if let Ok(points) = field.text.trim().parse::<f32>() {
                    let tenths = (points * 10.0).round() as i32;
                    self.state.config.set_event_text_scale_tenths(tenths);
                    let _ = self.state.save_config();
                }
            }
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
            draw_button(fb, rect, &mode.label().to_uppercase(), active, Font::Ui);
        }
        for (action, rect) in self.action_buttons() {
            let active = matches!(
                (action, self.ink_tool),
                (Action::Pen, InkTool::Pen)
                    | (Action::Erase, InkTool::Erase)
                    | (Action::Lasso, InkTool::Lasso)
            );
            draw_icon_button(fb, rect, icon_for(action), action.label(), active);
        }
        if !self.status.is_empty() {
            // Bottom edge: the only strip of the calendar screen that is
            // neither a toolbar button nor useful writing space.
            fb.draw_text(4, CANVAS_H - 12, &self.status, GRAY, 2, Font::Ui);
        }

        let today = self.today();
        let cells = self.grid_cells();
        match self.state.config.view_mode {
            ViewMode::Month => {
                draw_vertical_text(
                    fb,
                    MONTH_LABEL_W / 2,
                    TOOLBAR_H + (CANVAS_H - TOOLBAR_H) / 2,
                    &self
                        .state
                        .config
                        .anchor_date
                        .format("%B")
                        .to_string()
                        .to_uppercase(),
                    MONTH_LABEL_SCALE,
                );
            }
            ViewMode::TwoMonths => {
                // One vertical label per month, centred on that month's rows.
                let anchor = self.state.config.anchor_date;
                let first = anchor.with_day(1).unwrap();
                let second = next_month_first(anchor);
                for month in [first, second] {
                    if let Some(center_y) = month_center_y(&cells, month.year(), month.month()) {
                        draw_vertical_text(
                            fb,
                            MONTH_LABEL_W / 2,
                            center_y,
                            &month.format("%B").to_string().to_uppercase(),
                            MONTH_LABEL_SCALE,
                        );
                    }
                }
            }
            _ => {}
        }
        let month_like = matches!(
            self.state.config.view_mode,
            ViewMode::Month | ViewMode::TwoMonths
        );
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
            fb.draw_text(
                cell.rect.x + 4,
                cell.rect.y + 4,
                &day_label,
                label_gray,
                DAY_NUMBER_SCALE,
                Font::Ui,
            );

            // Event summaries, below the day number. Each event's text is
            // word/letter-wrapped to the cell width; continuation lines are
            // indented one character (a hanging indent) so it is clear where
            // a new event begins. The size is user-configurable.
            let event_scale = self.state.config.event_text_scale_f32();
            let line_h = FrameBuffer::text_height_scaled(event_scale, Font::Ui) + 2;
            let indent = FrameBuffer::text_width_scaled("M", event_scale, Font::Ui);
            let avail = (cell.rect.w - 8 - indent).max(1);
            let bottom = cell.rect.y + cell.rect.h;
            let mut text_y = cell.rect.y + 6 + FrameBuffer::text_height(DAY_NUMBER_SCALE, Font::Ui);
            'events: for event in self.events_for(cell.date) {
                for (i, line) in wrap_text(&event.summary, avail, event_scale, Font::Ui)
                    .iter()
                    .enumerate()
                {
                    if text_y + line_h > bottom {
                        break 'events;
                    }
                    let x = cell.rect.x + 4 + if i == 0 { 0 } else { indent };
                    fb.draw_text_scaled(x, text_y, line, BLACK, event_scale, Font::Ui);
                    text_y += line_h;
                }
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
            if month_like {
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
            self.editor_test_result = None;
            self.event_size_editor = None;
            return;
        }
        if within(layout.refresh_button, x, y) {
            self.start_refresh();
            return;
        }
        for row in &layout.source_rows {
            if within(row.move_up, x, y) {
                if row.index > 0 {
                    self.state.config.sources.swap(row.index, row.index - 1);
                    let _ = self.state.save_config();
                }
                return;
            }
            if within(row.move_down, x, y) {
                if row.index + 1 < self.state.config.sources.len() {
                    self.state.config.sources.swap(row.index, row.index + 1);
                    let _ = self.state.save_config();
                }
                return;
            }
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
                self.editor_test_result = None;
                self.offset_editor = None;
                return;
            }
        }
        for (kind, rect) in &layout.add_buttons {
            if within(*rect, x, y) {
                self.editor = Some(SourceEditor::new_for_add(*kind));
                self.editor_test_result = None;
                self.offset_editor = None;
                return;
            }
        }
        for (field, rect) in &layout.editor_fields {
            if within(*rect, x, y) {
                if let Some(editor) = &mut self.editor {
                    // Position the cursor where the tap landed (finger or
                    // pen); tapping past the text puts it at the end.
                    editor.place_cursor_from_tap(*field, rect.x + 12, x);
                }
                self.offset_editor = None;
                self.status = "USE APPLOAD KEYBOARD BUTTON".to_string();
                return;
            }
        }
        if let Some(test) = layout.editor_test_button {
            if within(test, x, y) {
                self.start_editor_test();
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
                    self.editor_test_result = None;
                    let _ = self.state.save_config();
                }
                return;
            }
        }
        if let Some(cancel) = layout.cancel_button {
            if within(cancel, x, y) {
                self.editor = None;
                self.editor_test_result = None;
            }
        }
        if within(layout.offset_row, x, y) {
            if self.offset_editor.is_none() {
                self.offset_editor = Some(TextField::new(
                    self.state.config.utc_offset_minutes.to_string(),
                ));
            }
            self.event_size_editor = None;
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
                return;
            }
        }
        self.handle_display_settings_tap(&layout, x, y);
    }

    /// Handle a tap on the display-settings controls (view picker, startup
    /// default cycler, event text-size stepper).
    fn handle_display_settings_tap(&mut self, layout: &SettingsLayout, x: i32, y: i32) {
        for (mode, rect) in &layout.view_toggles {
            if within(*rect, x, y) {
                // Deduplicate, then toggle: a first tap appends (so tap order
                // becomes button order), a second tap removes it.
                let mut views: Vec<ViewMode> = Vec::new();
                for v in &self.state.config.visible_views {
                    if !views.contains(v) {
                        views.push(*v);
                    }
                }
                if let Some(pos) = views.iter().position(|v| v == mode) {
                    views.remove(pos);
                } else {
                    views.push(*mode);
                }
                self.state.config.visible_views = views;
                // Keep the active and default views valid/visible.
                let visible = self.state.config.ordered_views();
                if !visible.contains(&self.state.config.view_mode) {
                    self.state.config.view_mode = visible[0];
                }
                self.offset_editor = None;
                self.event_size_editor = None;
                let _ = self.state.save_config();
                return;
            }
        }
        if let Some(rect) = layout.default_view_button {
            if within(rect, x, y) {
                // Cycle through: each visible view in order, then "LAST
                // USED", then back to the first view.
                let views = self.state.config.ordered_views();
                if self.state.config.startup_last_used {
                    // LAST USED → first view.
                    self.state.config.startup_last_used = false;
                    self.state.config.default_view = views[0];
                } else {
                    match views
                        .iter()
                        .position(|v| *v == self.state.config.default_view)
                    {
                        Some(i) if i + 1 < views.len() => {
                            self.state.config.default_view = views[i + 1];
                        }
                        // Last view (or a stale default) → LAST USED.
                        _ => self.state.config.startup_last_used = true,
                    }
                }
                let _ = self.state.save_config();
                return;
            }
        }
        if let Some(rect) = layout.event_minus_button {
            if within(rect, x, y) {
                self.adjust_event_text_scale(-AppConfig::EVENT_TEXT_SCALE_TENTHS_STEP);
                return;
            }
        }
        if let Some(rect) = layout.event_plus_button {
            if within(rect, x, y) {
                self.adjust_event_text_scale(AppConfig::EVENT_TEXT_SCALE_TENTHS_STEP);
                return;
            }
        }
        if let Some(rect) = layout.event_size_row {
            if within(rect, x, y) {
                if self.event_size_editor.is_none() {
                    self.event_size_editor =
                        Some(TextField::new(self.state.config.event_text_scale_label()));
                    self.offset_editor = None;
                }
                self.status = "USE APPLOAD KEYBOARD BUTTON".to_string();
                return;
            }
        }
        if let Some(rect) = layout.raw_pen_button {
            if within(rect, x, y) {
                self.state.config.raw_pen_input = !self.state.config.raw_pen_input;
                let _ = self.state.save_config();
            }
        }
    }

    /// Nudge the event text size by `delta_tenths` tenths of a point,
    /// clamped, and persist it.
    fn adjust_event_text_scale(&mut self, delta_tenths: i32) {
        let next = self.state.config.event_text_scale_tenths_clamped() + delta_tenths;
        self.state.config.set_event_text_scale_tenths(next);
        self.event_size_editor = None;
        let _ = self.state.save_config();
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
                // A narrow reorder column on the far left, two stacked
                // half-height buttons.
                let reorder_w = 56;
                let reorder_gap = 8;
                source_rows.push(SourceRow {
                    index,
                    move_up: view::Rect {
                        x: row_rect.x,
                        y: row_rect.y,
                        w: reorder_w,
                        h: (row_rect.h - 4) / 2,
                    },
                    move_down: view::Rect {
                        x: row_rect.x,
                        y: row_rect.y + (row_rect.h - 4) / 2 + 4,
                        w: reorder_w,
                        h: (row_rect.h - 4) / 2,
                    },
                    edit: view::Rect {
                        x: row_rect.x + reorder_w + reorder_gap,
                        y: row_rect.y,
                        w: row_rect.w - edit_w - reorder_w - reorder_gap,
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
        let (save_button, cancel_button, editor_test_button, editor_result_origin) =
            if let Some(editor) = &self.editor {
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
                let buttons_y = y;
                (
                    Some(view::Rect {
                        x: 20,
                        y: buttons_y,
                        w: 260,
                        h: 88,
                    }),
                    Some(view::Rect {
                        x: 300,
                        y: buttons_y,
                        w: 260,
                        h: 88,
                    }),
                    Some(view::Rect {
                        x: 580,
                        y: buttons_y,
                        w: 260,
                        h: 88,
                    }),
                    Some((20, buttons_y + 104)),
                )
            } else {
                (None, None, None, None)
            };

        // Display settings (view picker, startup default, event text size)
        // — shown only when not editing a source, flowing below the add
        // buttons.
        let mut view_toggles = Vec::new();
        let mut default_view_button = None;
        let mut event_minus_button = None;
        let mut event_plus_button = None;
        let mut event_size_row = None;
        let mut raw_pen_button = None;
        let mut display_section_y = None;
        if self.editor.is_none() {
            let section_y = y + 112;
            display_section_y = Some(section_y);
            // Row of view toggles (heading is drawn ~46px above).
            let toggles_y = section_y + 52;
            let all = ViewMode::ALL;
            let toggle_w = (CANVAS_W - 40) / all.len() as i32;
            for (i, m) in all.iter().enumerate() {
                view_toggles.push((
                    *m,
                    view::Rect {
                        x: 20 + i as i32 * toggle_w,
                        y: toggles_y,
                        w: toggle_w - 8,
                        h: 92,
                    },
                ));
            }
            // "Starts on" cycler + event size controls on the next row.
            let controls_y = toggles_y + 128;
            default_view_button = Some(view::Rect {
                x: 20,
                y: controls_y,
                w: 520,
                h: 84,
            });
            event_minus_button = Some(view::Rect {
                x: 700,
                y: controls_y,
                w: 84,
                h: 84,
            });
            event_size_row = Some(view::Rect {
                x: 792,
                y: controls_y,
                w: 220,
                h: 84,
            });
            event_plus_button = Some(view::Rect {
                x: 1020,
                y: controls_y,
                w: 84,
                h: 84,
            });
            raw_pen_button = Some(view::Rect {
                x: 20,
                y: controls_y + 100,
                w: 520,
                h: 84,
            });
        }

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
            editor_test_button,
            editor_result_origin,
            view_toggles,
            default_view_button,
            event_minus_button,
            event_plus_button,
            event_size_row,
            raw_pen_button,
            display_section_y,
        }
    }

    /// Render the display-settings section: the view picker (tap to select
    /// and order), the startup-default cycler, and the event text-size
    /// stepper. Shown only when not editing a calendar source.
    fn render_display_settings(&self, fb: &mut FrameBuffer, layout: &SettingsLayout) {
        let Some(section_y) = layout.display_section_y else {
            return;
        };
        fb.draw_text(20, section_y, "Display", BLACK, BODY_TEXT_SCALE, Font::Ui);
        fb.draw_text(
            20,
            section_y + 24,
            "Views: tap to show/hide; tap order = button order",
            GRAY,
            EVENT_TEXT_SCALE,
            Font::Ui,
        );
        let order = self.state.config.ordered_views();
        for (mode, rect) in &layout.view_toggles {
            let selected = order.iter().position(|v| v == mode);
            draw_button(
                fb,
                *rect,
                &mode.label().to_uppercase(),
                selected.is_some(),
                Font::Ui,
            );
            if let Some(pos) = selected {
                // Selection-order badge in the corner.
                fb.draw_text(
                    rect.x + 6,
                    rect.y + 6,
                    &format!("{}", pos + 1),
                    BLACK,
                    EVENT_TEXT_SCALE,
                    Font::Ui,
                );
            }
        }
        if let Some(rect) = layout.default_view_button {
            let label = if self.state.config.startup_last_used {
                "LAST USED".to_string()
            } else {
                self.state.config.default_view.label().to_uppercase()
            };
            draw_button(fb, rect, &format!("STARTS ON: {label}"), false, Font::Ui);
        }
        if let Some(rect) = layout.event_minus_button {
            draw_button(fb, rect, "-", false, Font::Ui);
        }
        if let Some(rect) = layout.event_plus_button {
            draw_button(fb, rect, "+", false, Font::Ui);
        }
        if let Some(rect) = layout.event_size_row {
            fb.draw_rect_outline(
                rect,
                if self.event_size_editor.is_some() {
                    BLACK
                } else {
                    GRAY
                },
            );
            let text = if let Some(field) = &self.event_size_editor {
                format!("TEXT {}", text_with_cursor(&field.text, field.cursor))
            } else {
                format!("TEXT {}", self.state.config.event_text_scale_label())
            };
            fb.draw_text(
                rect.x + 12,
                rect.y + 30,
                &text,
                BLACK,
                BODY_TEXT_SCALE,
                Font::Ui,
            );
        }
        if let Some(rect) = layout.raw_pen_button {
            let mode = if self.state.config.raw_pen_input {
                "RAW (smooth)"
            } else {
                "QTFB (fallback)"
            };
            draw_button(
                fb,
                rect,
                &format!("PEN INPUT: {mode}"),
                self.state.config.raw_pen_input,
                Font::Ui,
            );
        }
    }

    fn render_settings(&self, fb: &mut FrameBuffer) {
        fb.clear(WHITE);
        let layout = self.settings_layout();
        draw_button(fb, layout.back_button, "BACK", false, Font::Ui);
        draw_button(fb, layout.refresh_button, "REFRESH", false, Font::Ui);

        let offset_text = if let Some(field) = &self.offset_editor {
            format!(
                "UTC offset minutes: {}",
                text_with_cursor(&field.text, field.cursor)
            )
        } else {
            let label =
                calnotes_core::timeutil::UtcOffset::new(self.state.config.utc_offset_minutes)
                    .label();
            format!("UTC offset: {label} (tap to edit)")
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
            &fit_text(
                &offset_text,
                layout.offset_row.w - 24,
                BODY_TEXT_SCALE,
                Font::Ui,
            ),
            BLACK,
            BODY_TEXT_SCALE,
            Font::Ui,
        );
        if let Some(save) = layout.offset_save_button {
            draw_button(fb, save, "SAVE", false, Font::Ui);
        }

        if self.editor.is_none() {
            let heading = if self.state.config.sources.len() >= 2 {
                "Sources  (^/v reorder; order sets same-day event order)"
            } else {
                "Sources"
            };
            fb.draw_text(20, 222, heading, BLACK, BODY_TEXT_SCALE, Font::Ui);
        }

        for row in &layout.source_rows {
            let source = &self.state.config.sources[row.index];
            // Reorder buttons: the arrow only shows when the move is
            // possible, so the ends of the list read as disabled.
            let can_up = row.index > 0;
            let can_down = row.index + 1 < self.state.config.sources.len();
            draw_button(
                fb,
                row.move_up,
                if can_up { "^" } else { "" },
                false,
                Font::Ui,
            );
            draw_button(
                fb,
                row.move_down,
                if can_down { "v" } else { "" },
                false,
                Font::Ui,
            );
            fb.draw_rect_outline(row.edit, BLACK);
            // Line 1: the source label on its own, so it is always legible
            // and never runs into the status text.
            let label = fit_text(&source.label, row.edit.w - 16, BODY_TEXT_SCALE, Font::Ui);
            fb.draw_text(
                row.edit.x + 8,
                row.edit.y + 12,
                &label,
                BLACK,
                BODY_TEXT_SCALE,
                Font::Ui,
            );
            // Line 2: the status/result on its own line in a smaller font.
            // While this row's TEST is running it shows "TESTING…" so the
            // button press is always visibly acknowledged, even if the
            // eventual OK/ERROR is identical to before.
            let is_testing = self
                .testing_source_id
                .as_deref()
                .is_some_and(|id| id == source.id);
            let status = if is_testing {
                "TESTING…".to_string()
            } else {
                status_label(&source.last_status)
            };
            let status = fit_text(&status, row.edit.w - 16, EVENT_TEXT_SCALE, Font::Ui);
            fb.draw_text(
                row.edit.x + 8,
                row.edit.y + 52,
                &status,
                if is_testing { BLACK } else { GRAY },
                EVENT_TEXT_SCALE,
                Font::Ui,
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
                    Font::Ui,
                );
            }
            draw_button(fb, row.test, "TEST", false, Font::Ui);
            draw_button(
                fb,
                row.toggle,
                if source.enabled { "ON" } else { "OFF" },
                source.enabled,
                Font::Ui,
            );
            draw_button(fb, row.delete, "DEL", false, Font::Ui);
        }

        for (kind, rect) in &layout.add_buttons {
            draw_button(fb, *rect, add_button_label(*kind), false, Font::Ui);
        }

        self.render_display_settings(fb, &layout);

        if let Some(login) = &self.google_login {
            let base_y = CANVAS_H - 150;
            for (i, line) in google_login_lines(login).iter().enumerate() {
                let line = fit_text(line, CANVAS_W - 40, BODY_TEXT_SCALE, Font::Ui);
                fb.draw_text(
                    20,
                    base_y + i as i32 * 24,
                    &line,
                    BLACK,
                    BODY_TEXT_SCALE,
                    Font::Ui,
                );
            }
        }
        if !self.status.is_empty() {
            let status = fit_text(&self.status, CANVAS_W - 40, BODY_TEXT_SCALE, Font::Ui);
            fb.draw_text(20, CANVAS_H - 28, &status, BLACK, BODY_TEXT_SCALE, Font::Ui);
        }

        if let Some(editor) = &self.editor {
            fb.draw_text(
                20,
                224,
                "Edit source - tap a field, then use the AppLoad keyboard",
                BLACK,
                BODY_TEXT_SCALE,
                Font::Ui,
            );
            for (field, rect) in &layout.editor_fields {
                editor.render_field(fb, *field, *rect);
            }
            if let Some(save) = layout.save_button {
                draw_button(fb, save, "SAVE", false, Font::Ui);
            }
            if let Some(cancel) = layout.cancel_button {
                draw_button(fb, cancel, "CANCEL", false, Font::Ui);
            }
            if let Some(test) = layout.editor_test_button {
                draw_button(fb, test, "TEST", false, Font::Ui);
            }
            if let (Some(result), Some((rx, ry))) =
                (&self.editor_test_result, layout.editor_result_origin)
            {
                // Full result/error, wrapped over as many lines as needed in
                // a smaller font so long messages are entirely readable.
                let line_h = FrameBuffer::text_height(EVENT_TEXT_SCALE, Font::Ui) + 4;
                for (i, line) in wrap_text(result, CANVAS_W - 40, EVENT_TEXT_SCALE as f32, Font::Ui)
                    .iter()
                    .enumerate()
                {
                    let y = ry + i as i32 * line_h;
                    if y + line_h > CANVAS_H {
                        break;
                    }
                    fb.draw_text(rx, y, line, BLACK, EVENT_TEXT_SCALE, Font::Ui);
                }
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

/// The full, untruncated result of a source test, for the multi-line
/// panel under the editor's TEST button. Unlike [`status_label`] this keeps
/// the original error message verbatim (any case, any length) so the user
/// can read exactly what went wrong.
fn full_status_text(status: &SourceStatus) -> String {
    match status {
        SourceStatus::NeverSynced => "Not tested yet.".to_string(),
        SourceStatus::Ok { event_count, .. } => {
            format!("OK — fetched {event_count} events.")
        }
        SourceStatus::Error { message, .. } => format!("Error: {message}"),
    }
}

/// Greedily wrap `text` to as many lines as needed so each rendered line
/// fits within `max_width` pixels at `scale` in `font`. Wraps on spaces
/// where possible and hard-splits any single word (e.g. a long URL) that
/// is itself wider than one line.
fn wrap_text(text: &str, max_width: i32, scale: f32, font: Font) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split(' ') {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if FrameBuffer::text_width_scaled(&candidate, scale, font) <= max_width {
                current = candidate;
                continue;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            // The word alone may still be wider than a line: split it by
            // character across as many lines as needed.
            let mut piece = String::new();
            for ch in word.chars() {
                let trial = format!("{piece}{ch}");
                if FrameBuffer::text_width_scaled(&trial, scale, font) <= max_width {
                    piece = trial;
                } else {
                    if !piece.is_empty() {
                        lines.push(std::mem::take(&mut piece));
                    }
                    piece.push(ch);
                }
            }
            current = piece;
        }
        lines.push(current);
    }
    lines
}

fn add_button_label(kind: SourceKindChoice) -> &'static str {
    match kind {
        SourceKindChoice::LocalIcs => "+ FILE",
        SourceKindChoice::HttpsIcs => "+ URL",
        SourceKindChoice::Google => "+ GOOGLE",
        SourceKindChoice::Icloud => "+ ICLOUD",
    }
}

fn draw_button(fb: &mut FrameBuffer, rect: view::Rect, label: &str, active: bool, font: Font) {
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
    // Pick the largest size (up to UI_TEXT_SCALE) whose measured width fits
    // the button, so labels never overflow or clip in either font.
    let scale = (2..=UI_TEXT_SCALE)
        .rev()
        .find(|s| FrameBuffer::text_width(label, *s, font) <= rect.w - 8)
        .unwrap_or(2);
    let tx = rect.x + ((rect.w - FrameBuffer::text_width(label, scale, font)) / 2).max(2);
    let ty = rect.y + (rect.h - FrameBuffer::text_height(scale, font)) / 2;
    fb.draw_text(tx, ty, label, BLACK, scale, font);
}

/// A small, hand-drawn line-art glyph for a toolbar action. Drawn with
/// framebuffer primitives (the embedded font is a Latin-only subset with no
/// symbol/emoji glyphs), which also keeps the look consistent with the
/// app's minimalist style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Icon {
    Pen,
    Erase,
    Lasso,
    Undo,
    Clear,
    Prev,
    Next,
    Today,
    Settings,
}

fn icon_for(action: Action) -> Icon {
    match action {
        Action::Settings => Icon::Settings,
        Action::Prev => Icon::Prev,
        Action::Today => Icon::Today,
        Action::Next => Icon::Next,
        Action::Pen => Icon::Pen,
        Action::Erase => Icon::Erase,
        Action::Lasso => Icon::Lasso,
        Action::Undo => Icon::Undo,
        Action::ClearDay => Icon::Clear,
    }
}

fn fill_triangle(fb: &mut FrameBuffer, area: view::Rect, pointing_right: bool) {
    let x0 = area.x + area.w / 4;
    let x1 = area.x + area.w * 3 / 4;
    let cy = area.y + area.h / 2;
    let half = (area.h / 3).max(2);
    for x in x0..=x1 {
        let t = (x - x0) as f32 / (x1 - x0).max(1) as f32;
        let grow = if pointing_right { 1.0 - t } else { t };
        let hh = (half as f32 * grow) as i32;
        fb.fill_rect(
            view::Rect {
                x,
                y: cy - hh,
                w: 1,
                h: 2 * hh + 1,
            },
            BLACK,
        );
    }
}

/// Draw `icon` centred in the square-ish `area`.
fn draw_icon(fb: &mut FrameBuffer, area: view::Rect, icon: Icon) {
    let cx = area.x + area.w / 2;
    let cy = area.y + area.h / 2;
    let r = (area.w.min(area.h) / 2 - 2).max(4);
    match icon {
        Icon::Pen => {
            fb.draw_line(
                area.x + 4,
                area.y + area.h - 4,
                area.x + area.w - 6,
                area.y + 6,
                BLACK,
                3,
            );
            // Nib tick near the writing tip.
            fb.draw_line(
                area.x + 4,
                area.y + area.h - 4,
                area.x + 12,
                area.y + area.h - 6,
                BLACK,
                2,
            );
        }
        Icon::Erase => {
            // An angled eraser block.
            let a = (area.x + 3, cy + 8);
            let b = (cx + 4, cy + 8);
            let c = (cx + 10, cy - 8);
            let d = (area.x + 9, cy - 8);
            fb.draw_line(a.0, a.1, b.0, b.1, BLACK, 2);
            fb.draw_line(b.0, b.1, c.0, c.1, BLACK, 2);
            fb.draw_line(c.0, c.1, d.0, d.1, BLACK, 2);
            fb.draw_line(d.0, d.1, a.0, a.1, BLACK, 2);
            fb.draw_line(area.x + 6, cy, cx + 7, cy, BLACK, 2);
        }
        Icon::Lasso => {
            // A dashed loop with a small tail.
            let pts = 8;
            let mut prev: Option<(i32, i32)> = None;
            for i in 0..=pts {
                let ang = std::f32::consts::TAU * i as f32 / pts as f32;
                let x = cx + (r as f32 * ang.cos()) as i32;
                let y = cy - 2 + (r as f32 * 0.8 * ang.sin()) as i32;
                if let Some((px, py)) = prev {
                    fb.draw_line_styled(px, py, x, y, BLACK, 2, Some((3, 3)));
                }
                prev = Some((x, y));
            }
            fb.draw_line(cx, cy - 2 + r, cx - 4, area.y + area.h - 2, BLACK, 2);
        }
        Icon::Undo => {
            // A left-pointing arrow (undo / go back).
            fb.draw_line(area.x + 4, cy, area.x + area.w - 4, cy, BLACK, 3);
            fb.draw_line(area.x + 4, cy, area.x + 14, cy - 9, BLACK, 3);
            fb.draw_line(area.x + 4, cy, area.x + 14, cy + 9, BLACK, 3);
        }
        Icon::Clear => {
            fb.draw_line(
                area.x + 4,
                area.y + 4,
                area.x + area.w - 4,
                area.y + area.h - 4,
                BLACK,
                3,
            );
            fb.draw_line(
                area.x + area.w - 4,
                area.y + 4,
                area.x + 4,
                area.y + area.h - 4,
                BLACK,
                3,
            );
        }
        Icon::Prev => fill_triangle(fb, area, false),
        Icon::Next => fill_triangle(fb, area, true),
        Icon::Today => {
            // A little calendar page: outline with a thick top bar (mirrors
            // the on-screen "today" double outline).
            let box_rect = view::Rect {
                x: cx - r,
                y: cy - r,
                w: 2 * r,
                h: 2 * r,
            };
            fb.draw_rect_outline(box_rect, BLACK);
            fb.fill_rect(
                view::Rect {
                    x: box_rect.x,
                    y: box_rect.y,
                    w: box_rect.w,
                    h: 6,
                },
                BLACK,
            );
        }
        Icon::Settings => {
            // Three slider tracks with offset knobs.
            for (i, ky) in [cy - 9, cy, cy + 9].iter().enumerate() {
                fb.fill_rect(
                    view::Rect {
                        x: area.x + 3,
                        y: *ky - 1,
                        w: area.w - 6,
                        h: 2,
                    },
                    BLACK,
                );
                let knob_x = match i {
                    0 => area.x + area.w / 4,
                    1 => area.x + area.w * 3 / 4 - 8,
                    _ => cx - 4,
                };
                fb.fill_rect(
                    view::Rect {
                        x: knob_x,
                        y: *ky - 5,
                        w: 8,
                        h: 10,
                    },
                    BLACK,
                );
            }
        }
    }
}

/// A toolbar button with a leading line-art icon and a label to its right.
fn draw_icon_button(fb: &mut FrameBuffer, rect: view::Rect, icon: Icon, label: &str, active: bool) {
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
    let pad = 10;
    let icon_size = (rect.h - 2 * pad).clamp(16, 48);
    let icon_area = view::Rect {
        x: rect.x + pad,
        y: rect.y + (rect.h - icon_size) / 2,
        w: icon_size,
        h: icon_size,
    };
    draw_icon(fb, icon_area, icon);
    // Label centred in the space to the right of the icon.
    let text_left = icon_area.x + icon_size + 8;
    let text_w = (rect.x + rect.w - 6 - text_left).max(0);
    let scale = (2..=UI_TEXT_SCALE)
        .rev()
        .find(|s| FrameBuffer::text_width(label, *s, Font::Ui) <= text_w)
        .unwrap_or(2);
    let tw = FrameBuffer::text_width(label, scale, Font::Ui);
    let tx = text_left + ((text_w - tw) / 2).max(0);
    let ty = rect.y + (rect.h - FrameBuffer::text_height(scale, Font::Ui)) / 2;
    fb.draw_text(tx, ty, label, BLACK, scale, Font::Ui);
}

/// Truncate `text` (with an ellipsis) to whatever fits in `max_width`
/// pixels at `scale` in `font`.
fn fit_text(text: &str, max_width: i32, scale: i32, font: Font) -> String {
    if FrameBuffer::text_width(text, scale, font) <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut end = chars.len();
    while end > 0 {
        let candidate: String = chars[..end].iter().collect::<String>() + "...";
        if FrameBuffer::text_width(&candidate, scale, font) <= max_width {
            return candidate;
        }
        end -= 1;
    }
    String::new()
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

/// The character index whose boundary is nearest `tap_x`, for `displayed`
/// text rendered starting at `text_start_x` (in [`Font::Ui`] at
/// [`BODY_TEXT_SCALE`]). Tapping past the last character returns the end
/// index, so the cursor lands after the final character.
fn cursor_index_for_tap(displayed: &str, text_start_x: i32, tap_x: i32) -> usize {
    let chars: Vec<char> = displayed.chars().collect();
    let mut best_index = 0usize;
    let mut best_delta = i32::MAX;
    for index in 0..=chars.len() {
        let prefix: String = chars[..index].iter().collect();
        let width = FrameBuffer::text_width(&prefix, BODY_TEXT_SCALE, Font::Ui);
        let delta = (text_start_x + width - tap_x).abs();
        if delta < best_delta {
            best_delta = delta;
            best_index = index;
        }
    }
    best_index
}

/// Normalize a user-entered calendar URL: trim surrounding whitespace and,
/// if no scheme was typed, assume HTTPS. A URL that already carries an
/// http(s) scheme is left as typed (a plain `http://` URL is still refused
/// at fetch time). This is why an address pasted with a stray leading space
/// — or entered without the scheme — no longer trips the "non-HTTPS" guard.
pub(crate) fn normalize_https_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

/// The month-name label down the calendar's left gutter, in the embedded
/// JetBrains Mono [`Font::Ui`] to match the rest of the calendar chrome.
/// The column of glyphs is horizontally centred on `center_x` (the middle
/// of the gutter), not left-aligned against the screen edge.
fn draw_vertical_text(fb: &mut FrameBuffer, center_x: i32, center_y: i32, text: &str, scale: i32) {
    let line = FrameBuffer::text_height(scale, Font::Ui);
    let height = text.chars().count() as i32 * line;
    let mut y = center_y - height / 2;
    for character in text.chars() {
        let glyph = character.to_string();
        let glyph_w = FrameBuffer::text_width(&glyph, scale, Font::Ui);
        fb.draw_text(center_x - glyph_w / 2, y, &glyph, BLACK, scale, Font::Ui);
        y += line;
    }
}

/// First day of the month after `date`'s month.
fn next_month_first(date: NaiveDate) -> NaiveDate {
    let (year, month) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).unwrap()
}

/// Vertical centre (canvas y) of the cells belonging to `year`/`month`, used
/// to place that month's label in the two-month view.
fn month_center_y(cells: &[view::DateCell], year: i32, month: u32) -> Option<i32> {
    let mut top = i32::MAX;
    let mut bottom = i32::MIN;
    for cell in cells
        .iter()
        .filter(|c| c.date.year() == year && c.date.month() == month)
    {
        top = top.min(cell.rect.y);
        bottom = bottom.max(cell.rect.y + cell.rect.h);
    }
    (top != i32::MAX).then_some((top + bottom) / 2)
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
    /// Reorder buttons: move this source up/down in the list. The order also
    /// determines how same-day events from different sources are stacked.
    move_up: view::Rect,
    move_down: view::Rect,
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
    editor_test_button: Option<view::Rect>,
    /// Where the wrapped, multi-line test result is drawn (top-left).
    editor_result_origin: Option<(i32, i32)>,
    /// Display-settings controls, shown only when not editing a source.
    view_toggles: Vec<(ViewMode, view::Rect)>,
    default_view_button: Option<view::Rect>,
    event_minus_button: Option<view::Rect>,
    event_plus_button: Option<view::Rect>,
    event_size_row: Option<view::Rect>,
    raw_pen_button: Option<view::Rect>,
    /// Top-left of the "Display" section, for the section's heading text.
    display_section_y: Option<i32>,
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

    /// Focus `field` and move its text cursor to the character boundary
    /// nearest a tap at `tap_x`, where the field's value is rendered
    /// starting at `text_start_x`. Tapping past the last character puts the
    /// cursor at the end. Driven by both finger and pen taps.
    pub fn place_cursor_from_tap(&mut self, field: EditorField, text_start_x: i32, tap_x: i32) {
        self.focus = field;
        let secret = matches!(
            field,
            EditorField::ClientSecret | EditorField::AppSpecificPassword
        );
        let value = self.focused_field();
        let displayed = if secret {
            calnotes_core::config::mask_secret(&value.text)
        } else {
            value.text.clone()
        };
        let count = value.text.chars().count();
        value.cursor = cursor_index_for_tap(&displayed, text_start_x, tap_x).min(count);
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
                path: self.path.text.trim().to_string(),
            },
            SourceKindChoice::HttpsIcs => SourceKind::HttpsIcs {
                url: normalize_https_url(&self.url.text),
            },
            SourceKindChoice::Google => SourceKind::GoogleCalendar {
                client_id: self.client_id.text.clone(),
                client_secret: self.client_secret.text.clone(),
                calendar_id: self.calendar_id.text.clone(),
                refresh_token: self.existing_refresh_token.clone(),
            },
            SourceKindChoice::Icloud => SourceKind::IcloudCalDav {
                apple_id: self.apple_id.text.trim().to_string(),
                app_specific_password: self.app_specific_password.text.clone(),
                calendar_url: normalize_https_url(&self.calendar_url.text),
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
            EditorField::Label => ("Label", &self.label, false),
            EditorField::Path => ("ICS file path", &self.path, false),
            EditorField::Url => ("ICS URL", &self.url, false),
            EditorField::ClientId => ("Google client ID", &self.client_id, false),
            EditorField::ClientSecret => ("Google client secret", &self.client_secret, true),
            EditorField::CalendarId => ("Google calendar ID", &self.calendar_id, false),
            EditorField::AppleId => ("Apple ID", &self.apple_id, false),
            EditorField::AppSpecificPassword => (
                "iCloud app-specific password",
                &self.app_specific_password,
                true,
            ),
            EditorField::CalendarUrl => ("iCloud calendar URL", &self.calendar_url, false),
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
        fb.draw_text(
            rect.x + 12,
            rect.y + 12,
            name,
            BLACK,
            BODY_TEXT_SCALE,
            Font::Ui,
        );
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
            &fit_text(&shown, rect.w - 24, BODY_TEXT_SCALE, Font::Ui),
            BLACK,
            BODY_TEXT_SCALE,
            Font::Ui,
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
    fn settings_display_controls_pick_views_default_and_event_size() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.screen = Screen::Settings;

            // Deselect every view except Day and Week, in that tap order.
            // Start from all-selected: tapping a selected view removes it.
            for target_remove in [
                ViewMode::WorkWeek,
                ViewMode::TwoWeeks,
                ViewMode::Month,
                ViewMode::TwoMonths,
            ] {
                let layout = app.settings_layout();
                let (_, rect) = layout
                    .view_toggles
                    .iter()
                    .find(|(m, _)| *m == target_remove)
                    .copied()
                    .unwrap();
                app.handle_touch_tap(rect.x + 5, rect.y + 5);
            }
            assert_eq!(
                app.state.config.ordered_views(),
                vec![ViewMode::Day, ViewMode::Week]
            );

            // The startup cycler steps through the visible views and a
            // "LAST USED" state, then wraps around. Visible views are
            // [Day, Week], so the cycle is Day → Week → LAST USED → Day.
            let cycler = app.settings_layout().default_view_button.unwrap();
            let mut seen_last_used = false;
            let mut seen_a_view = false;
            for _ in 0..3 {
                app.handle_touch_tap(cycler.x + 5, cycler.y + 5);
                if app.state.config.startup_last_used {
                    seen_last_used = true;
                } else {
                    seen_a_view = true;
                    // A concrete default is always one of the visible views.
                    assert!(app
                        .state
                        .config
                        .ordered_views()
                        .contains(&app.state.config.default_view));
                }
            }
            assert!(seen_last_used && seen_a_view);

            // Event text size +/- steppers move by a half point and clamp
            // within range.
            let start = app.state.config.event_text_scale_tenths_clamped();
            let layout = app.settings_layout();
            let plus = layout.event_plus_button.unwrap();
            app.handle_touch_tap(plus.x + 5, plus.y + 5);
            assert_eq!(
                app.state.config.event_text_scale_tenths_clamped(),
                start + AppConfig::EVENT_TEXT_SCALE_TENTHS_STEP
            );
            let layout = app.settings_layout();
            let minus = layout.event_minus_button.unwrap();
            app.handle_touch_tap(minus.x + 5, minus.y + 5);
            assert_eq!(app.state.config.event_text_scale_tenths_clamped(), start);
        });
    }

    #[test]
    #[serial_test::serial]
    fn toolbar_tap_switches_view_mode() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            // Tap whichever button is the Week view, regardless of order.
            let (_, week_button) = app
                .view_buttons()
                .into_iter()
                .find(|(m, _)| *m == ViewMode::Week)
                .unwrap();
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
    fn calendar_and_settings_screens_render_substantial_visible_chrome() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            for mode in ViewMode::ALL {
                app.set_view_mode(mode);
                let non_white = app.render().non_white_pixel_count();
                assert!(
                    non_white > 10_000,
                    "{mode:?} rendered only {non_white} non-white pixels"
                );
            }
            app.screen = Screen::Settings;
            let non_white = app.render().non_white_pixel_count();
            assert!(
                non_white > 10_000,
                "settings rendered only {non_white} non-white pixels"
            );
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
            for c in "home".chars() {
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
            assert_eq!(app.state.config.sources[0].label, "home");
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
            assert_eq!(app.editor.as_ref().unwrap().label.text, "home");

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
    fn tapping_in_a_field_moves_the_cursor_to_the_tap_position() {
        let text = "https://ex.com/c.ics";
        let mut editor = SourceEditor::new_for_add(SourceKindChoice::HttpsIcs);
        editor.focus = EditorField::Url;
        for c in text.chars() {
            editor.url.apply_key(VkbKey::Char(c));
        }
        let start_x = 100;

        // Tap at the boundary after the first 5 characters.
        let target = 5;
        let prefix: String = text.chars().take(target).collect();
        let w = FrameBuffer::text_width(&prefix, BODY_TEXT_SCALE, Font::Ui);
        editor.place_cursor_from_tap(EditorField::Url, start_x, start_x + w);
        assert_eq!(editor.focus, EditorField::Url);
        assert_eq!(editor.url.cursor, target);

        // Tapping far past the text puts the cursor at the very end.
        editor.place_cursor_from_tap(EditorField::Url, start_x, start_x + 100_000);
        assert_eq!(editor.url.cursor, text.chars().count());

        // Tapping to the left of the text puts it at the start.
        editor.place_cursor_from_tap(EditorField::Url, start_x, start_x - 100_000);
        assert_eq!(editor.url.cursor, 0);

        // Tapping a different field focuses it (works for finger and pen,
        // which share this path).
        editor.place_cursor_from_tap(EditorField::Label, start_x, start_x);
        assert_eq!(editor.focus, EditorField::Label);
        assert_eq!(editor.label.cursor, 0);
    }

    #[test]
    fn wrap_text_splits_long_unbroken_text_across_multiple_lines() {
        let long = "x".repeat(400);
        let lines = wrap_text(&long, 600, EVENT_TEXT_SCALE as f32, Font::Ui);
        assert!(lines.len() > 1, "a 400-char string must wrap");
        for line in &lines {
            assert!(
                FrameBuffer::text_width(line, EVENT_TEXT_SCALE, Font::Ui) <= 600,
                "each wrapped line must fit the width"
            );
        }
        assert_eq!(lines.concat(), long, "wrapping must not drop characters");
    }

    #[test]
    fn full_status_text_keeps_the_whole_error_message() {
        let message = "refusing non-HTTPS calendar URL: ftp://example.com/very/long/path.ics";
        let status = SourceStatus::Error {
            synced_at_utc: NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            message: message.to_string(),
        };
        assert!(full_status_text(&status).contains(message));
    }

    #[test]
    #[serial_test::serial]
    fn pressing_a_source_row_test_marks_that_row_as_testing() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.screen = Screen::Settings;

            // Add a local .ics source and save it.
            let layout = app.settings_layout();
            let (_, add) = layout.add_buttons[0];
            app.handle_touch_tap(add.x + 2, add.y + 2);
            for c in "home".chars() {
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
            let id = app.state.config.sources[0].id.clone();

            // Press the per-row TEST button; the row is immediately marked
            // as testing so the press is visibly acknowledged, before the
            // background worker returns.
            let layout = app.settings_layout();
            let test = layout.source_rows[0].test;
            app.handle_touch_tap(test.x + 2, test.y + 2);
            assert_eq!(app.testing_source_id.as_deref(), Some(id.as_str()));

            // Once the worker's result is applied, the indicator clears.
            for _ in 0..300 {
                app.poll_background();
                if app.testing_source_id.is_none() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(app.testing_source_id.is_none());
        });
    }

    #[test]
    #[serial_test::serial]
    fn reorder_buttons_swap_source_order() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.screen = Screen::Settings;

            // Add two local .ics sources ("first" then "second").
            for name in ["first", "second"] {
                let layout = app.settings_layout();
                let (_, add) = layout.add_buttons[0];
                app.handle_touch_tap(add.x + 2, add.y + 2);
                for c in name.chars() {
                    app.handle_vkb(c as i32);
                }
                app.editor.as_mut().unwrap().handle_key(VkbKey::Tab);
                for c in format!("/tmp/{name}.ics").chars() {
                    app.handle_vkb(c as i32);
                }
                let layout = app.settings_layout();
                let save = layout.save_button.unwrap();
                app.handle_touch_tap(save.x + 2, save.y + 2);
            }
            assert_eq!(app.state.config.sources.len(), 2);
            assert_eq!(app.state.config.sources[0].label, "first");

            // Move the first source down: order becomes second, first.
            let layout = app.settings_layout();
            let down = layout.source_rows[0].move_down;
            app.handle_touch_tap(down.x + 2, down.y + 2);
            assert_eq!(app.state.config.sources[0].label, "second");
            assert_eq!(app.state.config.sources[1].label, "first");

            // Move it back up.
            let layout = app.settings_layout();
            let up = layout.source_rows[1].move_up;
            app.handle_touch_tap(up.x + 2, up.y + 2);
            assert_eq!(app.state.config.sources[0].label, "first");

            // At the ends, the move is a no-op (does not panic or wrap).
            let layout = app.settings_layout();
            let up_first = layout.source_rows[0].move_up;
            app.handle_touch_tap(up_first.x + 2, up_first.y + 2);
            assert_eq!(app.state.config.sources[0].label, "first");
        });
    }

    #[test]
    #[serial_test::serial]
    fn editor_test_reports_the_full_error_for_a_missing_local_ics() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.screen = Screen::Settings;

            let layout = app.settings_layout();
            let (kind, rect) = layout.add_buttons[0];
            assert_eq!(kind, SourceKindChoice::LocalIcs);
            app.handle_touch_tap(rect.x + 2, rect.y + 2);
            for c in "home".chars() {
                app.handle_vkb(c as i32);
            }
            app.editor.as_mut().unwrap().handle_key(VkbKey::Tab);
            for c in "/no/such/calendar-notes-test.ics".chars() {
                app.handle_vkb(c as i32);
            }

            let layout = app.settings_layout();
            let test = layout.editor_test_button.expect("editor has a TEST button");
            app.handle_touch_tap(test.x + 2, test.y + 2);
            assert!(app.editor_test_result.is_some());

            for _ in 0..300 {
                app.poll_background();
                if app.editor_test_rx.is_none() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let result = app
                .editor_test_result
                .clone()
                .expect("a test result is shown");
            assert!(result.starts_with("Error:"), "got: {result}");
        });
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
            // The eraser now shows a faint (light-grey), non-ink feedback
            // trail under the pen, rather than drawing nothing.
            let feedback = app.pen_move(410, 720, 1.0).unwrap();
            assert_eq!(feedback.gray, calnotes_core::render::LIGHT_GRAY);
            assert!(!feedback.dashed);
            assert!(app.pen_up());
            assert!(app
                .state
                .ink
                .strokes_for(app.state.config.anchor_date)
                .is_empty());
            // Undo brings the erased stroke back.
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

            // The lasso outline is drawn dashed and grey, and lassoing is
            // undoable just like erasing.
            app.ink_tool = InkTool::Lasso;
            app.pen_down(700, 1000, 1.0);
            let feedback = app.pen_move(760, 1100, 1.0).unwrap();
            assert!(feedback.dashed);
            assert_eq!(feedback.gray, calnotes_core::render::GRAY);
            app.pen_up();
        });
    }

    #[test]
    #[serial_test::serial]
    fn the_pen_operates_toolbar_buttons_and_writes_below_them() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Month);
            // A pen press on the DAY view button switches views, exactly as a
            // finger tap would.
            let (mode, rect) = app
                .view_buttons()
                .into_iter()
                .find(|(m, _)| *m == ViewMode::Day)
                .unwrap();
            assert_eq!(mode, ViewMode::Day);
            assert_eq!(
                app.handle_pen_tap(rect.x + rect.w / 2, rect.y + rect.h / 2),
                Some(true)
            );
            assert_eq!(app.state.config.view_mode, ViewMode::Day);

            // A pen press on the writing surface below the toolbar is not a
            // UI tap — it should begin an ink stroke.
            assert_eq!(app.handle_pen_tap(400, 900), None);
        });
    }

    #[test]
    #[serial_test::serial]
    fn undo_reverses_the_last_edit_even_on_a_non_anchor_cell() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Month);
            let cells = app.grid_cells();
            // Two different in-focus cells, neither necessarily the anchor.
            let a = cells.iter().find(|c| c.in_focus_period).unwrap().rect;
            let b = cells
                .iter()
                .filter(|c| c.in_focus_period)
                .nth(10)
                .unwrap()
                .rect;
            let stroke_in = |app: &mut App, r: view::Rect| {
                app.pen_down(r.x + r.w / 3, r.y + r.h / 2, 1.0);
                app.pen_move(r.x + 2 * r.w / 3, r.y + r.h / 2, 1.0).unwrap();
                app.pen_up();
            };
            stroke_in(&mut app, a);
            stroke_in(&mut app, b);
            let total = |app: &App| {
                app.state
                    .ink
                    .days
                    .values()
                    .map(|d| d.strokes.len())
                    .sum::<usize>()
            };
            assert_eq!(total(&app), 2);
            // Undo removes the last stroke (on cell B), then the first (A) —
            // regardless of the anchor date.
            app.undo_current_day();
            assert_eq!(total(&app), 1);
            app.undo_current_day();
            assert_eq!(total(&app), 0);
        });
    }

    #[test]
    fn month_is_the_default_view() {
        assert_eq!(
            calnotes_core::model::AppConfig::default().view_mode,
            ViewMode::Month
        );
    }

    #[test]
    fn normalize_https_url_trims_and_defaults_to_https() {
        // A pasted URL with a stray space still validates.
        assert_eq!(
            normalize_https_url("  https://a.com/c.ics "),
            "https://a.com/c.ics"
        );
        // A scheme-less address gets HTTPS (the common case that used to be
        // rejected).
        assert_eq!(
            normalize_https_url("www.officeholidays.com/ics/netherlands"),
            "https://www.officeholidays.com/ics/netherlands"
        );
        // An explicit http:// URL is preserved (and refused later at fetch).
        assert_eq!(normalize_https_url("http://a.com"), "http://a.com");
        assert_eq!(normalize_https_url("   "), "");
    }

    #[test]
    #[serial_test::serial]
    fn https_source_editor_normalizes_the_url_on_save() {
        with_temp_data_dir(|| {
            let mut editor = SourceEditor::new_for_add(SourceKindChoice::HttpsIcs);
            editor.url = TextField::new(" www.officeholidays.com/ics/netherlands ");
            let source = editor.build_source("src-1".to_string());
            match source.kind {
                SourceKind::HttpsIcs { url } => {
                    assert_eq!(url, "https://www.officeholidays.com/ics/netherlands");
                }
                other => panic!("unexpected kind: {other:?}"),
            }
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
