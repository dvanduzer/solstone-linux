// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Live desktop routes. Tests inject fakes. A socket or bus opens only when
//! the daemon starts the production foreground reader.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::activity_helpers::{
    GnomeSnapshotRequest, WindowSnapshot, check_age, current_unix_ms, parse_gnome_reply,
    parse_kwin_snapshot,
};
use crate::activity_writer::{MAX_BODY_BYTES, MAX_STRING_BYTES, sanitize_url};

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

fn read_helper_line(stream: &mut UnixStream, request: &str) -> Result<String, &'static str> {
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.flush())
        .map_err(|_| "unavailable")?;
    let mut line = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => return Err("unavailable"),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if line.len() >= MAX_BODY_BYTES {
                    return Err("payload-too-large");
                }
                line.push(byte[0]);
            }
            Err(_) => return Err("unavailable"),
        }
    }
    String::from_utf8(line).map_err(|_| "unavailable")
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

#[derive(Clone, Debug, Default)]
pub struct KwinInterest {
    pub enabled: bool,
    pub generation: u64,
    pub want_title: bool,
    pub expected_class: String,
    pub expected_pid: u32,
    pub latest: Option<WindowSnapshot>,
}

impl KwinInterest {
    /// `(want, generation, wantTitle, expectedClass, expectedPid)`.
    /// An empty class never asks for a caption.
    pub fn authorize(&self) -> (bool, u64, bool, String, u32) {
        if !self.enabled {
            return (false, self.generation, false, String::new(), 0);
        }
        (
            true,
            self.generation,
            self.want_title && !self.expected_class.is_empty(),
            self.expected_class.clone(),
            self.expected_pid,
        )
    }

    pub fn submit(&mut self, generation: u64, json: &str) -> Result<(), &'static str> {
        if !self.enabled || generation != self.generation {
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
            let class_ok =
                snapshot.wm_class == self.expected_class || snapshot.app == self.expected_class;
            if self.expected_class.is_empty() || !class_ok {
                self.latest = None;
                return Err("focus-changed");
            }
            if self.expected_pid != 0 && snapshot.pid != Some(self.expected_pid) {
                self.latest = None;
                return Err("focus-changed");
            }
        } else if snapshot.title.is_some() {
            self.latest = None;
            return Err("unexpected-title");
        }
        if !check_age(snapshot.captured_unix_ms, current_unix_ms()) {
            return Err("snapshot-expired");
        }
        self.latest = Some(snapshot);
        Ok(())
    }
}

pub struct LiveDesktop {
    helper: Box<dyn HelperBus>,
    pub kwin: Arc<Mutex<KwinInterest>>,
    documents: Box<dyn DocumentBus>,
}

pub trait DocumentBus: Send {
    fn firefox_doc_url(&mut self, pid: u32) -> Option<String>;
}

#[cfg(test)]
struct NoDocument;

#[cfg(test)]
impl DocumentBus for NoDocument {
    fn firefox_doc_url(&mut self, _pid: u32) -> Option<String> {
        None
    }
}

struct ZbusDocument;

impl DocumentBus for ZbusDocument {
    fn firefox_doc_url(&mut self, pid: u32) -> Option<String> {
        firefox_doc_url_from_bus(pid).and_then(|raw| sanitize_url(&raw))
    }
}

impl LiveDesktop {
    /// Binds the GNOME helper socket and the KWin bus name. Only daemon startup calls this.
    pub fn start() -> Self {
        let kwin = Arc::new(Mutex::new(KwinInterest::default()));
        spawn_kwin_bus(Arc::clone(&kwin));
        Self {
            helper: Box::new(GnomeEndpoint::bind()),
            kwin,
            documents: Box::new(ZbusDocument),
        }
    }

    #[cfg(test)]
    fn with_helper(helper: Box<dyn HelperBus>) -> Self {
        Self {
            helper,
            kwin: Arc::new(Mutex::new(KwinInterest::default())),
            documents: Box::new(NoDocument),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        let Ok(mut gate) = self.kwin.lock() else {
            return;
        };
        gate.enabled = enabled;
        if !enabled {
            gate.want_title = false;
            gate.expected_class.clear();
            gate.expected_pid = 0;
            gate.latest = None;
        }
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

    pub fn firefox_doc_url(&mut self, pid: u32) -> Option<String> {
        self.documents.firefox_doc_url(pid)
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
    fn authorize_read(&self) -> (bool, u64, bool, String, u32) {
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

fn firefox_doc_url_from_bus(pid: u32) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let session = zbus::blocking::connection::Builder::session()
        .ok()?
        .method_timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let address = session
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )
        .ok()?;
    let address: String = address.body().deserialize().ok()?;
    let bus = zbus::blocking::connection::Builder::address(address.as_str())
        .ok()?
        .method_timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let root = AtSpiObject {
        bus: &bus,
        destination: "org.a11y.atspi.Registry".into(),
        path: "/org/a11y/atspi/accessible/root".into(),
        deadline,
    };
    let apps = root.children()?;
    let mut matched = None;
    for app in apps.into_iter().take(MAX_NODES) {
        if Instant::now() >= deadline {
            return None;
        }
        let Some(app_pid) = app.unix_pid() else {
            continue;
        };
        if app_pid == pid {
            if matched.is_some() {
                return None;
            }
            matched = Some(app);
        }
    }
    let app = matched?;
    let node = BusNode { object: app };
    prove_firefox_document(&node)
}

struct AtSpiObject<'a> {
    bus: &'a zbus::blocking::Connection,
    destination: String,
    path: String,
    deadline: Instant,
}

impl AtSpiObject<'_> {
    fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    fn call<T: serde::de::DeserializeOwned + zbus::zvariant::Type>(
        &self,
        interface: &str,
        method: &str,
        body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
    ) -> Option<T> {
        if self.expired() {
            return None;
        }
        let reply = self
            .bus
            .call_method(
                Some(self.destination.as_str()),
                self.path.as_str(),
                Some(interface),
                method,
                body,
            )
            .ok()?;
        reply.body().deserialize().ok()
    }

    fn children(&self) -> Option<Vec<AtSpiObject<'_>>> {
        if self.expired() {
            return None;
        }
        let count: i32 = self.call("org.a11y.atspi.Accessible", "GetChildCount", &())?;
        let count = count.max(0) as usize;
        let mut out = Vec::new();
        for index in 0..count.min(MAX_NODES) {
            if self.expired() {
                return None;
            }
            let (destination, path): (String, zbus::zvariant::OwnedObjectPath) = self.call(
                "org.a11y.atspi.Accessible",
                "GetChildAtIndex",
                &(index as i32),
            )?;
            out.push(AtSpiObject {
                bus: self.bus,
                destination,
                path: path.as_str().to_string(),
                deadline: self.deadline,
            });
        }
        Some(out)
    }

    fn unix_pid(&self) -> Option<u32> {
        if self.expired() {
            return None;
        }
        let reply = self
            .bus
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetConnectionUnixProcessID",
                &self.destination,
            )
            .ok()?;
        reply.body().deserialize().ok()
    }

    fn role(&self) -> u32 {
        self.call("org.a11y.atspi.Accessible", "GetRole", &())
            .unwrap_or(0)
    }

    fn states(&self) -> u64 {
        let words: Vec<u32> = self
            .call("org.a11y.atspi.Accessible", "GetState", &())
            .unwrap_or_default();
        let low = u64::from(words.first().copied().unwrap_or(0));
        let high = u64::from(words.get(1).copied().unwrap_or(0));
        low | (high << 32)
    }
}

struct BusNode<'a> {
    object: AtSpiObject<'a>,
}

impl A11yNode for BusNode<'_> {
    fn role(&self) -> u32 {
        self.object.role()
    }
    fn states(&self) -> u64 {
        self.object.states()
    }
    fn children(&self) -> Vec<Box<dyn A11yNode + '_>> {
        self.object
            .children()
            .unwrap_or_default()
            .into_iter()
            .map(|object| Box::new(BusNode { object }) as Box<dyn A11yNode + '_>)
            .collect()
    }
    fn doc_url(&self) -> Option<String> {
        self.object
            .call("org.a11y.atspi.Document", "GetAttributeValue", &"DocURL")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

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

    #[test]
    fn kwin_title_requires_the_previous_window() {
        let mut gate = KwinInterest {
            enabled: true,
            generation: 4,
            want_title: true,
            expected_class: "Allowed".into(),
            expected_pid: 9,
            latest: None,
        };
        let (want, _, want_title, class, pid) = gate.authorize();
        assert!(want);
        assert!(want_title);
        assert_eq!(class, "Allowed");
        assert_eq!(pid, 9);
        let now = current_unix_ms();
        let json = format!(
            r#"{{"resourceClass":"Excluded","pid":9,"caption":"SECRET","captured_unix_ms":{now},"generation":4}}"#
        );
        assert_eq!(gate.submit(4, &json), Err("focus-changed"));
        assert!(gate.latest.is_none());
        gate.expected_class.clear();
        let (_, _, want_title, _, _) = gate.authorize();
        assert!(!want_title);
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
