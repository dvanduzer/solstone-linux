// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Live desktop routes. Tests inject fakes. A socket or bus opens only when
//! the daemon starts the production foreground reader.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::activity_helpers::{
    GnomeSnapshotRequest, WindowSnapshot, check_age, current_unix_ms, parse_gnome_reply,
    parse_kwin_snapshot,
};
use crate::activity_writer::{MAX_BODY_BYTES, MAX_STRING_BYTES};

const ROLE_FRAME: u32 = 23;
const ROLE_DOCUMENT_FRAME: u32 = 82;
const ROLE_DOCUMENT_WEB: u32 = 95;
const STATE_ACTIVE: u32 = 1;
const STATE_SHOWING: u32 = 25;
const MAX_NODES: usize = 64;
const MAX_DEPTH: usize = 6;

pub trait A11yNode {
    fn role(&self) -> u32;
    fn states(&self) -> u64;
    fn children(&self) -> Vec<Box<dyn A11yNode + '_>>;
    /// Document attribute `DocURL`. Call this only after the tree has exactly one showing document.
    fn doc_url(&self) -> Option<String>;
}

fn state_bit(states: u64, bit: u32) -> bool {
    states & (1u64 << bit) != 0
}

fn is_active_frame(node: &(dyn A11yNode + '_)) -> bool {
    node.role() == ROLE_FRAME
        && state_bit(node.states(), STATE_ACTIVE)
        && state_bit(node.states(), STATE_SHOWING)
}

fn is_showing_document(node: &(dyn A11yNode + '_)) -> bool {
    let role = node.role();
    (role == ROLE_DOCUMENT_FRAME || role == ROLE_DOCUMENT_WEB)
        && state_bit(node.states(), STATE_SHOWING)
}

struct DocScan {
    nodes: usize,
    frames: usize,
    docs: usize,
}

/// One showing document under the one active frame. `doc_url` runs only then.
/// The daemon does not call this. Production does not open an accessibility connection.
pub fn prove_firefox_document(root: &(dyn A11yNode + '_)) -> Option<String> {
    let mut scan = DocScan {
        nodes: 0,
        frames: 0,
        docs: 0,
    };
    count_documents(root, 0, false, &mut scan);
    if scan.frames != 1 || scan.docs != 1 || scan.nodes > MAX_NODES {
        return None;
    }
    let mut nodes = 0usize;
    let mut url = None;
    read_single_document(root, 0, false, &mut nodes, &mut url);
    url.filter(|value| value.len() <= MAX_STRING_BYTES)
}

fn count_documents(node: &(dyn A11yNode + '_), depth: usize, in_frame: bool, scan: &mut DocScan) {
    if depth > MAX_DEPTH || scan.nodes >= MAX_NODES || scan.frames > 1 || scan.docs > 1 {
        return;
    }
    scan.nodes += 1;
    if !in_frame && is_active_frame(node) {
        scan.frames += 1;
        for child in node.children() {
            count_documents(child.as_ref(), depth + 1, true, scan);
        }
        return;
    }
    if in_frame && is_showing_document(node) {
        scan.docs += 1;
        return;
    }
    for child in node.children() {
        count_documents(child.as_ref(), depth + 1, in_frame, scan);
    }
}

fn read_single_document(
    node: &(dyn A11yNode + '_),
    depth: usize,
    in_frame: bool,
    nodes: &mut usize,
    url: &mut Option<String>,
) {
    if url.is_some() || depth > MAX_DEPTH || *nodes >= MAX_NODES {
        return;
    }
    *nodes += 1;
    if !in_frame && is_active_frame(node) {
        for child in node.children() {
            read_single_document(child.as_ref(), depth + 1, true, nodes, url);
        }
        return;
    }
    if in_frame && is_showing_document(node) {
        *url = node.doc_url();
        return;
    }
    for child in node.children() {
        read_single_document(child.as_ref(), depth + 1, in_frame, nodes, url);
    }
}

pub trait HelperBus: Send {
    fn exchange(&mut self, request: &str) -> Result<String, &'static str>;
}

pub struct GnomeEndpoint {
    listener: Option<UnixListener>,
    stream: Option<UnixStream>,
    last_attempt: Option<Instant>,
    socket_path: Option<PathBuf>,
}

impl GnomeEndpoint {
    pub fn bind() -> Self {
        let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
            return Self::empty();
        };
        let dir = PathBuf::from(runtime).join("solstone-linux");
        if fs::create_dir_all(&dir).is_err() {
            return Self::empty();
        }
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        let path = dir.join("activity-helper.sock");
        let _ = fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => {
                let _ = listener.set_nonblocking(true);
                Self {
                    listener: Some(listener),
                    stream: None,
                    last_attempt: None,
                    socket_path: Some(path),
                }
            }
            Err(_) => Self::empty(),
        }
    }

    fn empty() -> Self {
        Self {
            listener: None,
            stream: None,
            last_attempt: None,
            socket_path: None,
        }
    }

    fn ensure_connected(&mut self) -> Result<(), &'static str> {
        if self.stream.is_some() {
            return Ok(());
        }
        let now = Instant::now();
        if self
            .last_attempt
            .is_some_and(|attempt| now.duration_since(attempt) < Duration::from_secs(5))
        {
            return Err("unavailable");
        }
        self.last_attempt = Some(now);
        let listener = self.listener.as_ref().ok_or("unavailable")?;
        let (stream, _) = listener.accept().map_err(|_| "unavailable")?;
        let path = self.socket_path.clone().ok_or("unavailable")?;
        let uid = rustix::process::getuid().as_raw();
        crate::activity_helpers::verify_socket_and_peer(&path, &stream, uid)
            .map_err(|_| "unavailable")?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_nonblocking(false);
        self.stream = Some(stream);
        Ok(())
    }
}

fn arm_deadline(stream: &UnixStream, deadline: Instant) -> Result<(), &'static str> {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return Err("unavailable");
    };
    if remaining.is_zero() {
        return Err("unavailable");
    }
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|_| "unavailable")?;
    stream
        .set_write_timeout(Some(remaining))
        .map_err(|_| "unavailable")?;
    Ok(())
}

/// One deadline covers the whole line. A peer that drips one byte inside each
/// per-read timeout still stops when `deadline` passes.
pub(crate) fn collect_helper_line(
    mut read_byte: impl FnMut() -> io::Result<Option<u8>>,
    deadline: Instant,
    now: impl Fn() -> Instant,
) -> Result<Vec<u8>, &'static str> {
    let mut line = Vec::new();
    loop {
        if now() >= deadline {
            return Err("unavailable");
        }
        match read_byte() {
            Ok(Some(b'\n')) => break,
            Ok(Some(byte)) => {
                if line.len() >= MAX_BODY_BYTES {
                    return Err("payload-too-large");
                }
                line.push(byte);
            }
            Ok(None) | Err(_) => return Err("unavailable"),
        }
    }
    Ok(line)
}

fn read_helper_line(stream: &mut UnixStream, request: &str) -> Result<String, &'static str> {
    let deadline = Instant::now() + Duration::from_secs(2);
    arm_deadline(stream, deadline)?;
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "unavailable")?;
    arm_deadline(stream, deadline)?;
    stream.write_all(b"\n").map_err(|_| "unavailable")?;
    arm_deadline(stream, deadline)?;
    stream.flush().map_err(|_| "unavailable")?;
    let bytes = collect_helper_line(
        || {
            if arm_deadline(stream, deadline).is_err() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "deadline"));
            }
            let mut byte = [0u8; 1];
            match stream.read(&mut byte) {
                Ok(0) => Ok(None),
                Ok(_) => Ok(Some(byte[0])),
                Err(error) => Err(error),
            }
        },
        deadline,
        Instant::now,
    )?;
    String::from_utf8(bytes).map_err(|_| "unavailable")
}

impl HelperBus for GnomeEndpoint {
    fn exchange(&mut self, request: &str) -> Result<String, &'static str> {
        self.ensure_connected()?;
        let outcome = {
            let stream = self.stream.as_mut().ok_or("unavailable")?;
            read_helper_line(stream, request)
        };
        if outcome.is_err() {
            self.stream = None;
        }
        outcome
    }
}

impl Drop for GnomeEndpoint {
    fn drop(&mut self) {
        self.stream.take();
        self.listener.take();
        if let Some(path) = self.socket_path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[derive(Clone, Default)]
pub struct KwinInterest {
    // Only the in-module parser tests opt in. Production construction uses
    // Default=false; no setting or installation can qualify this route.
    qualified: bool,
    pub prefs: Option<std::sync::Arc<crate::activity_prefs::ActivityPrefs>>,
    pub paused: bool,
    pub session_live: bool,
    pub seen_generation: u64,
    pub generation: u64,
    pub want_title: bool,
    pub expected_window: String,
    pub expected_class: String,
    pub expected_pid: u32,
    pub latest: Option<WindowSnapshot>,
}

impl KwinInterest {
    /// Source, pause, and session are read on every authorization. A saved
    /// enabled flag is not admission. Class and pid are not the window.
    pub fn admitted(&self) -> bool {
        let source = self
            .prefs
            .as_ref()
            .is_some_and(|prefs| prefs.snapshot().source);
        // No producer/helper qualification has been completed for KWin. Neither
        // installing its assets nor saving source=true grants it authorization.
        self.qualified && source && !self.paused && self.session_live
    }

    pub fn refresh_admission(&mut self) {
        let generation = self
            .prefs
            .as_ref()
            .map(|prefs| prefs.snapshot().generation)
            .unwrap_or(0);
        if !self.admitted() || generation != self.seen_generation {
            self.latest = None;
            self.want_title = false;
            self.expected_window.clear();
            self.expected_class.clear();
            self.expected_pid = 0;
        }
        self.seen_generation = generation;
    }

    /// `(want, generation, wantTitle, expectedClass, expectedPid, expectedWindow)`.
    /// `expectedWindow` is KWin `internalId`. An empty id never asks for a caption.
    pub fn authorize(&mut self) -> (bool, u64, bool, String, u32, String) {
        self.refresh_admission();
        if !self.admitted() {
            return (
                false,
                self.generation,
                false,
                String::new(),
                0,
                String::new(),
            );
        }
        (
            true,
            self.generation,
            self.want_title && !self.expected_window.is_empty(),
            self.expected_class.clone(),
            self.expected_pid,
            self.expected_window.clone(),
        )
    }

    pub fn submit(&mut self, generation: u64, json: &str) -> Result<(), &'static str> {
        self.refresh_admission();
        if !self.admitted() || generation != self.generation {
            self.latest = None;
            return Err("stale-generation");
        }
        let snapshot = match parse_kwin_snapshot(json, generation, current_unix_ms()) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.latest = None;
                return Err(error);
            }
        };
        if self.want_title {
            if self.expected_window.is_empty() || snapshot.window_token != self.expected_window {
                self.latest = None;
                return Err("focus-changed");
            }
        } else if snapshot.title.is_some() {
            self.latest = None;
            return Err("unexpected-title");
        }
        if !check_age(snapshot.captured_unix_ms, current_unix_ms()) {
            self.latest = None;
            return Err("snapshot-expired");
        }
        self.latest = Some(snapshot);
        Ok(())
    }
}

pub struct LiveDesktop {
    helper: Box<dyn HelperBus>,
    pub kwin: Arc<Mutex<KwinInterest>>,
}

impl LiveDesktop {
    /// Binds the GNOME helper socket and the KWin bus name. Only daemon startup calls this.
    pub fn start() -> Self {
        let kwin = Arc::new(Mutex::new(KwinInterest::default()));
        spawn_kwin_bus(Arc::clone(&kwin));
        Self {
            helper: Box::new(GnomeEndpoint::bind()),
            kwin,
        }
    }

    #[cfg(test)]
    fn with_helper(helper: Box<dyn HelperBus>) -> Self {
        Self {
            helper,
            kwin: Arc::new(Mutex::new(KwinInterest::default())),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        let Ok(mut gate) = self.kwin.lock() else {
            return;
        };
        gate.paused = paused;
        if paused {
            gate.latest = None;
            gate.want_title = false;
            gate.expected_window.clear();
        }
    }

    pub fn bind_prefs(&self, prefs: std::sync::Arc<crate::activity_prefs::ActivityPrefs>) {
        if let Ok(mut gate) = self.kwin.lock() {
            gate.seen_generation = prefs.snapshot().generation;
            gate.prefs = Some(prefs);
        }
    }

    pub fn note_route(&self, route: &str, live: bool, epoch: u64) {
        let Ok(mut gate) = self.kwin.lock() else {
            return;
        };
        let admit = route == "kwin" && live;
        if !admit || epoch != gate.generation {
            gate.latest = None;
            gate.want_title = false;
            gate.expected_window.clear();
        }
        gate.session_live = admit;
        gate.generation = epoch;
    }

    pub fn set_enabled(&self, enabled: bool) {
        if enabled {
            return;
        }
        let Ok(mut gate) = self.kwin.lock() else {
            return;
        };
        gate.want_title = false;
        gate.expected_window.clear();
        gate.expected_class.clear();
        gate.expected_pid = 0;
        gate.latest = None;
    }

    pub fn gnome_snapshot(
        &mut self,
        generation: u64,
        want_title: bool,
        expected: Option<(u64, String, u32)>,
    ) -> Result<WindowSnapshot, &'static str> {
        if want_title && expected.is_none() {
            return Err("focus-changed");
        }
        let request = GnomeSnapshotRequest {
            v: 1,
            op: "snapshot".into(),
            generation,
            want_title,
            expected_window_id: expected.as_ref().map(|(id, _, _)| *id),
            expected_wm_class: expected.as_ref().map(|(_, class, _)| class.clone()),
            expected_pid: expected.as_ref().map(|(_, _, pid)| *pid),
        };
        let line = serde_json::to_string(&request).map_err(|_| "unavailable")?;
        let reply = self.helper.exchange(&line)?;
        let mut snapshot = parse_gnome_reply(&reply, generation, current_unix_ms())?;
        if want_title {
            let (window, class, pid) = expected.ok_or("focus-changed")?;
            if u64::from(snapshot.window_id) != window
                || snapshot.wm_class != class
                || snapshot.pid != Some(pid)
            {
                return Err("focus-changed");
            }
        } else {
            snapshot.title = None;
        }
        Ok(snapshot)
    }
}

fn spawn_kwin_bus(state: Arc<Mutex<KwinInterest>>) {
    let _ = std::thread::Builder::new()
        .name("solstone-kwin-activity".into())
        .spawn(move || {
            let service = KwinService { state };
            let connection = zbus::blocking::connection::Builder::session()
                .and_then(|builder| builder.serve_at("/org/solpbc/SolstoneLinux/Activity", service))
                .and_then(|builder| builder.name("org.solpbc.SolstoneLinux"))
                .and_then(|builder| builder.build());
            let Ok(_connection) = connection else {
                return;
            };
            // The connection's executor thread serves the script. Hold the name until exit.
            loop {
                std::thread::park();
            }
        });
}

struct KwinService {
    state: Arc<Mutex<KwinInterest>>,
}

#[zbus::interface(name = "org.solpbc.SolstoneLinux.Activity")]
impl KwinService {
    #[zbus(name = "AuthorizeRead")]
    fn authorize_read(&mut self) -> (bool, u64, bool, String, u32, String) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .authorize()
    }

    #[zbus(name = "SubmitSnapshot")]
    fn submit_snapshot(&mut self, generation: u64, json: String) {
        if let Ok(mut state) = self.state.lock() {
            let _ = state.submit(generation, &json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    use crate::activity_writer::sanitize_url;

    struct Node {
        role: u32,
        states: u64,
        children: Vec<Node>,
        url: Option<String>,
        reads: Rc<Cell<usize>>,
    }

    impl A11yNode for Node {
        fn role(&self) -> u32 {
            self.role
        }
        fn states(&self) -> u64 {
            self.states
        }
        fn children(&self) -> Vec<Box<dyn A11yNode + '_>> {
            self.children
                .iter()
                .map(|child| {
                    Box::new(Node {
                        role: child.role,
                        states: child.states,
                        children: child.children.clone(),
                        url: child.url.clone(),
                        reads: Rc::clone(&child.reads),
                    }) as Box<dyn A11yNode>
                })
                .collect()
        }
        fn doc_url(&self) -> Option<String> {
            self.reads.set(self.reads.get() + 1);
            self.url.clone()
        }
    }

    impl Clone for Node {
        fn clone(&self) -> Self {
            Self {
                role: self.role,
                states: self.states,
                children: self.children.clone(),
                url: self.url.clone(),
                reads: Rc::clone(&self.reads),
            }
        }
    }

    fn showing(active: bool) -> u64 {
        let mut bits = 1u64 << STATE_SHOWING;
        if active {
            bits |= 1u64 << STATE_ACTIVE;
        }
        bits
    }

    #[test]
    fn firefox_doc_url_reads_only_the_showing_document() {
        let entry_reads = Rc::new(Cell::new(0));
        let doc_reads = Rc::new(Cell::new(0));
        let root = Node {
            role: 75,
            states: showing(false),
            reads: Rc::new(Cell::new(0)),
            url: None,
            children: vec![Node {
                role: ROLE_FRAME,
                states: showing(true),
                reads: Rc::new(Cell::new(0)),
                url: None,
                children: vec![
                    Node {
                        role: 79,
                        states: showing(true) | (1u64 << 12),
                        reads: Rc::clone(&entry_reads),
                        url: Some("https://typed.example/draft".into()),
                        children: vec![],
                    },
                    Node {
                        role: ROLE_DOCUMENT_WEB,
                        states: showing(false),
                        reads: Rc::clone(&doc_reads),
                        url: Some("https://user:pass@example.com/path?q=1#f".into()),
                        children: vec![],
                    },
                ],
            }],
        };
        let raw = prove_firefox_document(&root).unwrap();
        assert_eq!(raw, "https://user:pass@example.com/path?q=1#f");
        assert_eq!(entry_reads.get(), 0);
        assert_eq!(doc_reads.get(), 1);
        assert_eq!(
            sanitize_url(&raw).as_deref(),
            Some("https://example.com/path")
        );
    }

    #[test]
    fn two_showing_documents_do_not_read_a_value() {
        let reads = Rc::new(Cell::new(0));
        let doc = |reads: &Rc<Cell<usize>>| Node {
            role: ROLE_DOCUMENT_WEB,
            states: showing(false),
            reads: Rc::clone(reads),
            url: Some("https://example.com/a".into()),
            children: vec![],
        };
        let root = Node {
            role: ROLE_FRAME,
            states: showing(true),
            reads: Rc::new(Cell::new(0)),
            url: None,
            children: vec![doc(&reads), doc(&reads)],
        };
        assert!(prove_firefox_document(&root).is_none());
        assert_eq!(reads.get(), 0);
    }

    fn admitted_kwin() -> (
        KwinInterest,
        std::sync::Arc<crate::activity_prefs::ActivityPrefs>,
    ) {
        let prefs = std::sync::Arc::new(crate::activity_prefs::ActivityPrefs::new(
            &crate::config::Config {
                activity_source: true,
                ..crate::config::Config::default()
            },
        ));
        let gate = KwinInterest {
            qualified: true,
            prefs: Some(std::sync::Arc::clone(&prefs)),
            paused: false,
            session_live: true,
            seen_generation: prefs.generation(),
            generation: 4,
            want_title: true,
            expected_window: "win-a".into(),
            expected_class: "Allowed".into(),
            expected_pid: 9,
            latest: None,
        };
        (gate, prefs)
    }

    #[test]
    fn unqualified_kwin_never_authorizes_or_accepts_content() {
        let (mut gate, _prefs) = admitted_kwin();
        gate.qualified = false;
        assert!(!gate.authorize().0);
        assert!(!gate.want_title);
        assert!(gate.submit(4, r#"{"caption":"SECRET"}"#).is_err());
        assert!(gate.latest.is_none());
    }

    #[test]
    fn kwin_title_requires_the_previous_window() {
        let (mut gate, _prefs) = admitted_kwin();
        let (want, _, want_title, class, pid, window) = gate.authorize();
        assert!(want);
        assert!(want_title);
        assert_eq!(class, "Allowed");
        assert_eq!(pid, 9);
        assert_eq!(window, "win-a");
        let now = current_unix_ms();
        let same_app = format!(
            r#"{{"resourceClass":"Allowed","pid":9,"internalId":"win-b","caption":"SECRET","captured_unix_ms":{now},"generation":4}}"#
        );
        assert_eq!(gate.submit(4, &same_app), Err("focus-changed"));
        assert!(gate.latest.is_none());
        let matched = format!(
            r#"{{"resourceClass":"Allowed","pid":9,"internalId":"win-a","caption":"Page","captured_unix_ms":{now},"generation":4}}"#
        );
        assert!(gate.submit(4, &matched).is_ok());
        assert_eq!(
            gate.latest
                .as_ref()
                .and_then(|snapshot| snapshot.title.clone()),
            Some("Page".into())
        );
        gate.latest = None;
        gate.expected_window.clear();
        let (_, _, want_title, _, _, _) = gate.authorize();
        assert!(!want_title);
    }

    #[test]
    fn kwin_authorization_follows_pause_and_source() {
        let (mut gate, prefs) = admitted_kwin();
        assert!(gate.authorize().0);
        gate.paused = true;
        let (want, _, _, _, _, _) = gate.authorize();
        assert!(!want);
        assert!(gate.latest.is_none());
        gate.paused = false;
        gate.expected_window = "win-a".into();
        gate.want_title = true;
        gate.seen_generation = prefs.generation();
        prefs.apply_policy(crate::activity_prefs::ActivityPolicy {
            activity_source: false,
            activity_titles: false,
            activity_urls: false,
            activity_exclude_apps: Vec::new(),
            activity_exclude_titles: Vec::new(),
        });
        let (want, _, _, _, _, _) = gate.authorize();
        assert!(!want);
        assert!(gate.expected_window.is_empty());
    }

    #[test]
    fn helper_line_stops_on_an_absolute_deadline() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(2);
        let calls = std::cell::Cell::new(0u32);
        let result = collect_helper_line(
            || {
                calls.set(calls.get() + 1);
                Ok(Some(b'x'))
            },
            deadline,
            || start + Duration::from_millis(400 * u64::from(calls.get())),
        );
        assert_eq!(result, Err("unavailable"));
        assert!(calls.get() < 8, "drip ran for {}", calls.get());
        assert!(calls.get() >= 4, "drip stopped too early {}", calls.get());
    }

    struct FakeGnome {
        focus: std::sync::Arc<std::sync::atomic::AtomicU64>,
        title_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl HelperBus for FakeGnome {
        fn exchange(&mut self, request: &str) -> Result<String, &'static str> {
            let req: GnomeSnapshotRequest =
                serde_json::from_str(request).map_err(|_| "unavailable")?;
            let now = current_unix_ms();
            if req.want_title {
                let same = req.expected_window_id == Some(1)
                    && req.expected_wm_class.as_deref() == Some("Allowed")
                    && req.expected_pid == Some(7)
                    && self.focus.load(std::sync::atomic::Ordering::Relaxed) == 1;
                if !same {
                    return Ok(format!(
                        r#"{{"v":1,"generation":{},"captured_unix_ms":{now},"app":"","pid":0,"window_id":0}}"#,
                        req.generation
                    ));
                }
                self.title_reads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Ok(format!(
                    r#"{{"v":1,"generation":{},"captured_unix_ms":{now},"app":"Allowed","wm_class":"Allowed","pid":7,"window_id":1,"title":"Page"}}"#,
                    req.generation
                ));
            }
            Ok(format!(
                r#"{{"v":1,"generation":{},"captured_unix_ms":{now},"app":"Allowed","wm_class":"Allowed","pid":7,"window_id":1,"title":"SHOULD_NOT_KEEP"}}"#,
                req.generation
            ))
        }
    }

    #[test]
    fn gnome_title_request_names_the_earlier_window() {
        let focus = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1));
        let title_reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut live = LiveDesktop::with_helper(Box::new(FakeGnome {
            focus: std::sync::Arc::clone(&focus),
            title_reads: std::sync::Arc::clone(&title_reads),
        }));
        let identity = live.gnome_snapshot(3, false, None).unwrap();
        assert_eq!(identity.app, "Allowed");
        assert!(identity.title.is_none());
        assert_eq!(identity.window_id, 1);
        focus.store(2, std::sync::atomic::Ordering::Relaxed);
        assert!(
            live.gnome_snapshot(3, true, Some((1, "Allowed".into(), 7)))
                .is_err()
        );
        assert_eq!(title_reads.load(std::sync::atomic::Ordering::Relaxed), 0);
        focus.store(1, std::sync::atomic::Ordering::Relaxed);
        let titled = live
            .gnome_snapshot(3, true, Some((1, "Allowed".into(), 7)))
            .unwrap();
        assert_eq!(titled.title.as_deref(), Some("Page"));
        assert_eq!(title_reads.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
