// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Desktop activity rows. Idle and lock for media stay in `activity` and `observer::mode`.

use std::sync::Mutex;

use crate::activity::{ActivityFacts, BoundBackends};
use crate::activity_prefs::ActivityPrefsSnapshot;
use crate::activity_writer::DesktopActivityRow;
use crate::desktop_session::DesktopKind;
use crate::observer::ActivityState;

pub const ACTIVITY_FAULT: &str = "activity-write-failed";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityStatus {
    pub published: bool,
    pub route: Option<String>,
    pub foreground: String,
    pub title: String,
    pub url: String,
    pub idle: String,
}

impl Default for CapabilityStatus {
    fn default() -> Self {
        Self {
            published: false,
            route: None,
            foreground: "unavailable".into(),
            title: "unavailable".into(),
            url: "unavailable".into(),
            idle: "unavailable".into(),
        }
    }
}

#[derive(Debug)]
pub struct ActivityFault {
    token: Mutex<Option<String>>,
}

impl ActivityFault {
    pub fn new() -> Self {
        Self {
            token: Mutex::new(None),
        }
    }

    pub fn record_write_error(&self) {
        *self.token.lock().unwrap_or_else(|p| p.into_inner()) = Some(ACTIVITY_FAULT.into());
    }

    pub fn token(&self) -> Option<String> {
        self.token.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Default for ActivityFault {
    fn default() -> Self {
        Self::new()
    }
}

/// App identity, and a title only when the caller asked for one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForegroundWindow {
    pub app: String,
    pub app_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForegroundRead {
    Window(ForegroundWindow),
    Unavailable,
}

pub trait ForegroundProvider {
    /// `want_title` false must not read a title. A second call with `want_title`
    /// true is the title read, and it must describe the same window.
    fn read_foreground(&mut self, want_title: bool, facts: &ActivityFacts) -> ForegroundRead;
}

pub struct NoForeground;

impl ForegroundProvider for NoForeground {
    fn read_foreground(&mut self, _want_title: bool, _facts: &ActivityFacts) -> ForegroundRead {
        ForegroundRead::Unavailable
    }
}

pub struct PollInput {
    pub now_ms: u64,
    pub prefs: ActivityPrefsSnapshot,
    pub activity: ActivityState,
    pub bound: BoundBackends,
    pub paused: bool,
    pub route: &'static str,
}

#[derive(Clone, Debug)]
enum Span {
    Foreground {
        app: String,
        app_id: Option<String>,
        title: Option<String>,
        method: String,
        ts: u64,
    },
    Activity {
        state: String,
        method: String,
        ts: u64,
    },
    Availability {
        capability: String,
        status: String,
        method: String,
        ts: u64,
    },
}

#[derive(Clone, Copy)]
enum SlotKind {
    Foreground,
    Presence,
    Url,
}

pub struct ActivityRuntime {
    pub prefs: std::sync::Arc<crate::activity_prefs::ActivityPrefs>,
    pub sampler: ActivitySampler,
    pub writer: Option<(std::path::PathBuf, crate::activity_writer::ActivityWriter)>,
    pub fault: std::sync::Arc<ActivityFault>,
    pub capability: std::sync::Arc<Mutex<CapabilityStatus>>,
    pub anchor_mono: Option<f64>,
    pub desktop: String,
}

pub struct ActivitySampler {
    generation: u64,
    epoch: u64,
    foreground: Option<Span>,
    presence: Option<Span>,
    url: Option<Span>,
    pending: Vec<DesktopActivityRow>,
    status: CapabilityStatus,
}

impl ActivitySampler {
    pub fn new() -> Self {
        Self {
            generation: 0,
            epoch: 0,
            foreground: None,
            presence: None,
            url: None,
            pending: Vec::new(),
            status: CapabilityStatus::default(),
        }
    }

    pub fn has_open(&self) -> bool {
        self.foreground.is_some() || self.presence.is_some() || self.url.is_some()
    }

    pub fn status(&self) -> CapabilityStatus {
        self.status.clone()
    }

    pub fn take_pending(&mut self) -> Vec<DesktopActivityRow> {
        std::mem::take(&mut self.pending)
    }

    pub fn restore_pending(&mut self, rows: Vec<DesktopActivityRow>) {
        let mut restored = rows;
        restored.append(&mut self.pending);
        self.pending = restored;
    }

    pub fn poll(
        &mut self,
        input: &PollInput,
        provider: &mut dyn ForegroundProvider,
        facts: &ActivityFacts,
    ) {
        if input.prefs.generation != self.generation || input.activity.session_epoch != self.epoch {
            self.seal(input.now_ms);
            self.generation = input.prefs.generation;
            self.epoch = input.activity.session_epoch;
        }
        self.status.published = true;
        self.status.url = "unavailable".into();
        self.observe_presence(input);
        self.observe_url(input);
        self.observe_foreground(input, provider, facts);
    }

    pub fn seal(&mut self, now_ms: u64) {
        if let Some(span) = self.foreground.take() {
            self.push_span(span, now_ms);
        }
        if let Some(span) = self.presence.take() {
            self.push_span(span, now_ms);
        }
        if let Some(span) = self.url.take() {
            self.push_span(span, now_ms);
        }
    }

    fn observe_presence(&mut self, input: &PollInput) {
        if !input.prefs.source || input.paused {
            self.close_slot(SlotKind::Presence, input.now_ms);
            if !input.prefs.source {
                self.status.idle = "unavailable".into();
            }
            return;
        }
        let next = presence_span(&input.activity, &input.bound, input.now_ms);
        self.status.idle = match &next {
            Some(Span::Activity { state, .. }) if state == "active" || state == "idle" => {
                "available".into()
            }
            Some(Span::Activity { state, .. }) if state == "locked" => "available".into(),
            _ => "unavailable".into(),
        };
        self.continue_or_replace(SlotKind::Presence, next, input.now_ms);
    }

    fn observe_url(&mut self, input: &PollInput) {
        if !input.prefs.source || !input.prefs.urls || input.paused {
            self.close_slot(SlotKind::Url, input.now_ms);
            return;
        }
        let next = Some(Span::Availability {
            capability: "url".into(),
            status: "unavailable".into(),
            method: method_or_unavailable(input.route),
            ts: input.now_ms,
        });
        self.continue_or_replace(SlotKind::Url, next, input.now_ms);
    }

    fn observe_foreground(
        &mut self,
        input: &PollInput,
        provider: &mut dyn ForegroundProvider,
        facts: &ActivityFacts,
    ) {
        let suppressed = !input.prefs.source
            || input.paused
            || !input.activity.session.is_live()
            || input.activity.screen_locked;
        if suppressed {
            self.close_slot(SlotKind::Foreground, input.now_ms);
            if !input.prefs.source {
                self.status.route = None;
                self.status.foreground = "unavailable".into();
                self.status.title = "unavailable".into();
            }
            return;
        }

        let identity = provider.read_foreground(false, facts);
        let ForegroundRead::Window(window) = identity else {
            self.status.route = None;
            self.status.foreground = "unavailable".into();
            self.status.title = "unavailable".into();
            self.open_availability_foreground(input);
            return;
        };
        if window.app.is_empty() {
            self.status.route = None;
            self.status.foreground = "unavailable".into();
            self.status.title = "unavailable".into();
            self.open_availability_foreground(input);
            return;
        }
        if app_excluded(&input.prefs, &window.app, window.app_id.as_deref()) {
            self.close_slot(SlotKind::Foreground, input.now_ms);
            return;
        }

        let mut title = None;
        if input.prefs.titles || !input.prefs.exclude_titles.is_empty() {
            match provider.read_foreground(true, facts) {
                ForegroundRead::Window(second)
                    if second.app == window.app && second.app_id == window.app_id =>
                {
                    title = second.title;
                }
                _ => {
                    self.close_slot(SlotKind::Foreground, input.now_ms);
                    return;
                }
            }
        }
        if title.as_ref().is_some_and(|value| value.len() > 4096) {
            title = None;
        }
        if !input.prefs.exclude_titles.is_empty()
            && (title.is_none() || title_excluded(&input.prefs, title.as_deref().unwrap_or("")))
        {
            self.close_slot(SlotKind::Foreground, input.now_ms);
            return;
        }
        let stored_title = if input.prefs.titles { title } else { None };
        let route = if input.route.is_empty() {
            None
        } else {
            Some(input.route.to_string())
        };
        self.status.route = route;
        self.status.foreground = "available".into();
        self.status.title = if stored_title.is_some() {
            "available".into()
        } else {
            "unavailable".into()
        };
        let next = Some(Span::Foreground {
            app: window.app,
            app_id: window.app_id,
            title: stored_title,
            method: method_or_unavailable(input.route),
            ts: input.now_ms,
        });
        self.continue_or_replace(SlotKind::Foreground, next, input.now_ms);
    }

    fn open_availability_foreground(&mut self, input: &PollInput) {
        let next = Some(Span::Availability {
            capability: "foreground".into(),
            status: "unavailable".into(),
            method: method_or_unavailable(input.route),
            ts: input.now_ms,
        });
        self.continue_or_replace(SlotKind::Foreground, next, input.now_ms);
    }

    fn slot(&mut self, kind: SlotKind) -> &mut Option<Span> {
        match kind {
            SlotKind::Foreground => &mut self.foreground,
            SlotKind::Presence => &mut self.presence,
            SlotKind::Url => &mut self.url,
        }
    }

    fn continue_or_replace(&mut self, kind: SlotKind, next: Option<Span>, now_ms: u64) {
        let unchanged = matches!(
            (self.slot(kind).as_ref(), next.as_ref()),
            (Some(open), Some(incoming)) if same_span(open, incoming)
        );
        if unchanged {
            return;
        }
        self.close_slot(kind, now_ms);
        *self.slot(kind) = next.map(|span| stamp_span(span, now_ms));
    }

    fn close_slot(&mut self, kind: SlotKind, now_ms: u64) {
        let span = self.slot(kind).take();
        if let Some(span) = span {
            self.push_span(span, now_ms);
        }
    }

    fn push_span(&mut self, span: Span, now_ms: u64) {
        let row = match span {
            Span::Foreground {
                app,
                app_id,
                title,
                method,
                ts,
            } => {
                if app.is_empty() || now_ms < ts {
                    return;
                }
                DesktopActivityRow::foreground(ts, now_ms, method, app, app_id, title, None)
            }
            Span::Activity { state, method, ts } => {
                if now_ms < ts {
                    return;
                }
                DesktopActivityRow::activity(ts, now_ms, method, state)
            }
            Span::Availability {
                capability,
                status,
                method,
                ts,
            } => {
                if now_ms < ts {
                    return;
                }
                DesktopActivityRow::availability(ts, now_ms, method, capability, status)
            }
        };
        self.pending.push(row);
    }
}

impl Default for ActivitySampler {
    fn default() -> Self {
        Self::new()
    }
}

fn method_or_unavailable(route: &str) -> String {
    if route.is_empty() {
        "unavailable".into()
    } else {
        route.to_string()
    }
}

fn app_excluded(prefs: &ActivityPrefsSnapshot, app: &str, app_id: Option<&str>) -> bool {
    prefs
        .exclude_apps
        .iter()
        .any(|excluded| excluded == app || app_id == Some(excluded.as_str()))
}

fn title_excluded(prefs: &ActivityPrefsSnapshot, title: &str) -> bool {
    prefs
        .exclude_titles
        .iter()
        .any(|excluded| excluded == title)
}

fn stamp_span(span: Span, now_ms: u64) -> Span {
    match span {
        Span::Foreground {
            app,
            app_id,
            title,
            method,
            ..
        } => Span::Foreground {
            app,
            app_id,
            title,
            method,
            ts: now_ms,
        },
        Span::Activity { state, method, .. } => Span::Activity {
            state,
            method,
            ts: now_ms,
        },
        Span::Availability {
            capability,
            status,
            method,
            ..
        } => Span::Availability {
            capability,
            status,
            method,
            ts: now_ms,
        },
    }
}

fn same_span(open: &Span, next: &Span) -> bool {
    match (open, next) {
        (
            Span::Foreground {
                app,
                app_id,
                title,
                method,
                ..
            },
            Span::Foreground {
                app: app2,
                app_id: id2,
                title: title2,
                method: method2,
                ..
            },
        ) => app == app2 && app_id == id2 && title == title2 && method == method2,
        (
            Span::Activity { state, method, .. },
            Span::Activity {
                state: state2,
                method: method2,
                ..
            },
        ) => state == state2 && method == method2,
        (
            Span::Availability {
                capability,
                status,
                method,
                ..
            },
            Span::Availability {
                capability: capability2,
                status: status2,
                method: method2,
                ..
            },
        ) => capability == capability2 && status == status2 && method == method2,
        _ => false,
    }
}

fn presence_span(activity: &ActivityState, bound: &BoundBackends, now_ms: u64) -> Option<Span> {
    if activity.screen_locked
        && let Some(method) = bound.lock_signal()
    {
        return Some(Span::Activity {
            state: "locked".into(),
            method: method.to_string(),
            ts: now_ms,
        });
    }
    if let Some(method) = bound.idle_signal() {
        let state = if activity.user_idle { "idle" } else { "active" };
        return Some(Span::Activity {
            state: state.into(),
            method: method.to_string(),
            ts: now_ms,
        });
    }
    Some(Span::Availability {
        capability: "idle".into(),
        status: "unavailable".into(),
        method: "unavailable".into(),
        ts: now_ms,
    })
}

pub fn route_name(kind: Option<DesktopKind>, desktop: &str) -> &'static str {
    match kind {
        Some(DesktopKind::X11) => "x11-ewmh",
        Some(DesktopKind::Wayland) if crate::activity::desktop_has_token(desktop, "GNOME") => {
            "gnome-shell"
        }
        Some(DesktopKind::Wayland) if crate::activity::desktop_has_token(desktop, "KDE") => "kwin",
        Some(DesktopKind::Wayland) if crate::activity::desktop_has_token(desktop, "sway") => {
            "sway-ipc"
        }
        _ => "",
    }
}

pub fn ewmh_display(kind: Option<DesktopKind>, display: Option<&str>) -> Option<&str> {
    match kind {
        Some(DesktopKind::X11) => display.filter(|value| !value.is_empty()),
        _ => None,
    }
}

/// Live foreground. Missing helpers and a Wayland display report unavailable.
/// URL stays unavailable: no committed page property is qualified.
pub struct ProductionForeground {
    pub desktop: String,
}

impl ForegroundProvider for ProductionForeground {
    fn read_foreground(&mut self, want_title: bool, facts: &ActivityFacts) -> ForegroundRead {
        let route = route_name(facts.kind, &self.desktop);
        if route == "x11-ewmh" {
            let Some(display) = ewmh_display(facts.kind, facts.display.as_deref()) else {
                return ForegroundRead::Unavailable;
            };
            return match crate::activity_helpers::read_live_ewmh(display, 0, want_title, 0) {
                Ok(snapshot) => ForegroundRead::Window(ForegroundWindow {
                    app: snapshot.app,
                    app_id: snapshot.app_id,
                    title: if want_title { snapshot.title } else { None },
                }),
                Err(_) => ForegroundRead::Unavailable,
            };
        }
        // GNOME, KWin, and Sway parsers are tested against their field names.
        // A missing socket or bus is unavailable. This path does not open an X display.
        let _ = (route, want_title);
        ForegroundRead::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;

    use crate::desktop_session::SessionPresence;

    fn prefs(
        source: bool,
        titles: bool,
        urls: bool,
        apps: &[&str],
        title_exclusions: &[&str],
    ) -> ActivityPrefsSnapshot {
        ActivityPrefsSnapshot {
            source,
            titles,
            urls,
            exclude_apps: apps.iter().map(|s| (*s).to_string()).collect(),
            exclude_titles: title_exclusions.iter().map(|s| (*s).to_string()).collect(),
            generation: 1,
        }
    }

    fn live_state() -> ActivityState {
        ActivityState {
            session: SessionPresence::Live,
            ..ActivityState::default()
        }
    }

    struct Scripted {
        identity: VecDeque<ForegroundRead>,
        titled: VecDeque<ForegroundRead>,
        title_reads: Cell<usize>,
    }

    impl Scripted {
        fn window(app: &str, title: Option<&str>) -> ForegroundRead {
            ForegroundRead::Window(ForegroundWindow {
                app: app.into(),
                app_id: None,
                title: title.map(str::to_string),
            })
        }
    }

    impl ForegroundProvider for Scripted {
        fn read_foreground(&mut self, want_title: bool, _facts: &ActivityFacts) -> ForegroundRead {
            if want_title {
                self.title_reads.set(self.title_reads.get() + 1);
                self.titled
                    .pop_front()
                    .unwrap_or(ForegroundRead::Unavailable)
            } else {
                self.identity
                    .pop_front()
                    .unwrap_or(ForegroundRead::Unavailable)
            }
        }
    }

    fn poll_at(
        sampler: &mut ActivitySampler,
        now_ms: u64,
        prefs: ActivityPrefsSnapshot,
        activity: ActivityState,
        bound: BoundBackends,
        provider: &mut Scripted,
    ) {
        sampler.poll(
            &PollInput {
                now_ms,
                prefs,
                activity,
                bound,
                paused: false,
                route: "x11-ewmh",
            },
            provider,
            &ActivityFacts::default(),
        );
    }

    fn foreground_apps(rows: &[DesktopActivityRow]) -> Vec<String> {
        rows.iter()
            .filter_map(|row| match &row.payload {
                crate::activity_writer::DesktopActivityPayload::Foreground { app, .. } => {
                    Some(app.clone())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn source_off_does_not_read_or_write() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("A", None)]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(false, true, true, &[], &[]),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(2_000);
        assert!(sampler.take_pending().is_empty());
        assert_eq!(provider.title_reads.get(), 0);
        assert_eq!(provider.identity.len(), 1);
    }

    #[test]
    fn excluded_app_skips_title_getter_and_is_not_stored() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("SENTINEL_APP", None)]),
            titled: VecDeque::from([Scripted::window("SENTINEL_APP", Some("SENTINEL_TITLE"))]),
            title_reads: Cell::new(0),
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, true, false, &["SENTINEL_APP"], &[]),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        assert_eq!(provider.title_reads.get(), 0);
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(!blob.contains("SENTINEL_APP"));
        assert!(!blob.contains("SENTINEL_TITLE"));
    }

    #[test]
    fn excluded_title_is_not_stored() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("Notes", None)]),
            titled: VecDeque::from([Scripted::window("Notes", Some("SENTINEL_TITLE"))]),
            title_reads: Cell::new(0),
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, true, false, &[], &["SENTINEL_TITLE"]),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(!blob.contains("SENTINEL_TITLE"));
        assert!(!foreground_apps(&rows).iter().any(|app| app == "Notes"));
        assert!(sampler.status().route.is_none());
    }

    #[test]
    fn foreground_gap_is_two_intervals() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([
                Scripted::window("A", None),
                Scripted::window("SENTINEL_APP", None),
                Scripted::window("A", None),
            ]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
        };
        let included = prefs(true, false, false, &["SENTINEL_APP"], &[]);
        poll_at(
            &mut sampler,
            1_000,
            included.clone(),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        poll_at(
            &mut sampler,
            2_000,
            included.clone(),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        poll_at(
            &mut sampler,
            3_000,
            included,
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(4_000);
        let rows = sampler.take_pending();
        let apps = foreground_apps(&rows);
        assert_eq!(apps, vec!["A".to_string(), "A".to_string()]);
        let foregrounds: Vec<_> = rows
            .iter()
            .filter(|row| {
                matches!(
                    row.payload,
                    crate::activity_writer::DesktopActivityPayload::Foreground { .. }
                )
            })
            .collect();
        assert_eq!(foregrounds[0].ts, 1_000);
        assert_eq!(foregrounds[0].end_ts, 2_000);
        assert_eq!(foregrounds[1].ts, 3_000);
        assert_eq!(foregrounds[1].end_ts, 4_000);
        assert!(foregrounds[0].end_ts <= foregrounds[1].ts);
    }

    #[test]
    fn absent_idle_is_availability_not_active() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([ForegroundRead::Unavailable]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
        };
        let mut activity = live_state();
        activity.user_idle = false;
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, false, false, &[], &[]),
            activity,
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(blob.contains("\"capability\":\"idle\""));
        assert!(blob.contains("\"status\":\"unavailable\""));
        assert!(!blob.contains("\"state\":\"active\""));
    }

    #[test]
    fn measured_not_idle_is_active() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([ForegroundRead::Unavailable]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
        };
        let bound = BoundBackends {
            x11_idle: true,
            ..BoundBackends::default()
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, false, false, &[], &[]),
            live_state(),
            bound,
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(blob.contains("\"state\":\"active\""));
        assert!(blob.contains("XScreenSaver"));
    }

    #[test]
    fn lock_suppresses_foreground() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("Secret", Some("hidden"))]),
            titled: VecDeque::from([Scripted::window("Secret", Some("hidden"))]),
            title_reads: Cell::new(0),
        };
        let mut activity = live_state();
        activity.screen_locked = true;
        let bound = BoundBackends {
            gnome_lock: true,
            ..BoundBackends::default()
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, true, false, &[], &[]),
            activity,
            bound,
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        assert_eq!(provider.title_reads.get(), 0);
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(!blob.contains("Secret"));
        assert!(blob.contains("\"state\":\"locked\""));
    }

    #[test]
    fn wayland_display_is_not_an_ewmh_source() {
        assert_eq!(ewmh_display(Some(DesktopKind::Wayland), Some(":0")), None);
        assert_eq!(ewmh_display(Some(DesktopKind::X11), Some(":0")), Some(":0"));
        assert_eq!(ewmh_display(None, Some(":0")), None);
    }

    #[test]
    fn write_error_token_has_no_payload() {
        let fault = ActivityFault::new();
        fault.record_write_error();
        let token = fault.token().unwrap();
        assert_eq!(token, ACTIVITY_FAULT);
        assert!(!token.contains("SENTINEL"));
        assert!(!token.contains('/'));
    }
}
