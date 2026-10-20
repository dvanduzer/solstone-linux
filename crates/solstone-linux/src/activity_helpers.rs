// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::activity_writer::sanitize_url;
use crate::activity_writer::{MAX_AGE_MS, MAX_BODY_BYTES, MAX_STRING_BYTES};

pub const GNOME_SOCKET_NAME: &str = "activity-helper.sock";
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(2);
pub const RECONNECT_THROTTLE: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub app: String,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub pid: Option<u32>,
    pub window_id: u32,
    pub wm_class: String,
    /// KWin `internalId`. Empty when the route has no separate window token.
    pub window_token: String,
    pub captured_unix_ms: u64,
    pub generation: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GnomeSnapshotRequest {
    pub v: u32,
    pub op: String,
    pub generation: u64,
    pub want_title: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_window_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_wm_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_pid: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GnomeSnapshotReply {
    pub v: u32,
    pub generation: u64,
    pub captured_unix_ms: u64,
    pub app: String,
    pub app_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub wm_class: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub window_id: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct KWinSnapshotPayload {
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default, rename = "resourceClass")]
    pub resource_class: Option<String>,
    #[serde(default, rename = "resourceName")]
    pub resource_name: Option<String>,
    #[serde(default, rename = "desktopFileName")]
    pub desktop_file_name: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    /// KWin 6 `Window.internalId`. Class and pid are not a window identity.
    #[serde(default, rename = "internalId")]
    pub internal_id: Option<String>,
    pub captured_unix_ms: u64,
    pub generation: u64,
}

pub fn verify_socket_and_peer(
    endpoint: &Path,
    stream: &UnixStream,
    expected_uid: u32,
) -> io::Result<()> {
    let directory = endpoint.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::PermissionDenied, "missing parent directory")
    })?;
    let dir_meta = fs::symlink_metadata(directory)?;
    if !dir_meta.is_dir() || dir_meta.uid() != expected_uid || (dir_meta.mode() & 0o777 != 0o700) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory untrusted",
        ));
    }

    let sock_meta = fs::symlink_metadata(endpoint)?;
    if !sock_meta.file_type().is_socket() || sock_meta.uid() != expected_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket untrusted",
        ));
    }

    match rustix::net::sockopt::socket_peercred(stream) {
        Ok(cred) if cred.uid.as_raw() == expected_uid => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "peer uid untrusted",
        )),
    }
}

pub fn check_age(captured_unix_ms: u64, now_unix_ms: u64) -> bool {
    if captured_unix_ms > now_unix_ms {
        // slight clock skew or monotonic drift: accept within bound
        now_unix_ms + 1000 >= captured_unix_ms
    } else {
        now_unix_ms - captured_unix_ms <= MAX_AGE_MS
    }
}

pub fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn parse_kwin_snapshot(
    json: &str,
    current_generation: u64,
    now_unix_ms: u64,
) -> Result<WindowSnapshot, &'static str> {
    if json.len() > MAX_BODY_BYTES {
        return Err("payload-too-large");
    }

    let payload: KWinSnapshotPayload = serde_json::from_str(json).map_err(|_| "invalid-json")?;

    if payload.generation != current_generation {
        return Err("stale-generation");
    }

    if !check_age(payload.captured_unix_ms, now_unix_ms) {
        return Err("snapshot-expired");
    }

    let desktop_file = payload.desktop_file_name.unwrap_or_default();
    let resource_class = payload.resource_class.unwrap_or_default();

    let wm_class = resource_class.clone();
    let (app, app_id) = if !desktop_file.is_empty() {
        let id = if !resource_class.is_empty() {
            Some(resource_class)
        } else {
            None
        };
        (desktop_file, id)
    } else if !resource_class.is_empty() {
        (resource_class, None)
    } else {
        return Err("missing-app");
    };

    if app.len() > MAX_STRING_BYTES {
        return Err("app-too-long");
    }

    let title = match payload.caption {
        Some(t) => {
            if t.len() > MAX_STRING_BYTES {
                return Err("title-too-long");
            }
            Some(t)
        }
        None => None,
    };

    Ok(WindowSnapshot {
        app,
        app_id,
        title,
        pid: payload.pid,
        window_id: 0,
        wm_class,
        window_token: payload.internal_id.unwrap_or_default(),
        captured_unix_ms: payload.captured_unix_ms,
        generation: payload.generation,
    })
}

pub fn parse_gnome_reply(
    json: &str,
    current_generation: u64,
    now_unix_ms: u64,
) -> Result<WindowSnapshot, &'static str> {
    if json.len() > MAX_BODY_BYTES {
        return Err("payload-too-large");
    }

    let reply: GnomeSnapshotReply = serde_json::from_str(json).map_err(|_| "invalid-json")?;

    if reply.generation != current_generation {
        return Err("stale-generation");
    }

    if !check_age(reply.captured_unix_ms, now_unix_ms) {
        return Err("snapshot-expired");
    }

    if reply.app.is_empty() || reply.app.len() > MAX_STRING_BYTES {
        return Err("invalid-app");
    }

    let title = match reply.title {
        Some(t) => {
            if t.len() > MAX_STRING_BYTES {
                return Err("title-too-long");
            }
            Some(t)
        }
        None => None,
    };

    let window_id = match reply.window_id {
        Some(id) => u32::try_from(id).map_err(|_| "invalid-window")?,
        None => 0,
    };
    Ok(WindowSnapshot {
        app: reply.app,
        app_id: reply.app_id,
        title,
        pid: reply.pid,
        window_id,
        wm_class: reply.wm_class.unwrap_or_default(),
        window_token: String::new(),
        captured_unix_ms: reply.captured_unix_ms,
        generation: reply.generation,
    })
}

pub fn withhold_for_title_exclusion(title: Option<&str>, title_exclusions: &[String]) -> bool {
    !title_exclusions.is_empty() && title.is_none()
}

// KWin D-Bus interface caller validator
pub fn verify_caller_uid(caller_uid: u32, expected_uid: u32) -> Result<(), &'static str> {
    if caller_uid == expected_uid {
        Ok(())
    } else {
        Err("caller-unauthorized")
    }
}

pub const SWAY_IPC_MAGIC: &[u8; 6] = b"i3-ipc";
pub const SWAY_MAX_PAYLOAD: usize = 1_048_576;
pub const SWAY_GET_TREE_TYPE: u32 = 4;

pub fn encode_sway_command(msg_type: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(14 + payload.len());
    buf.extend_from_slice(SWAY_IPC_MAGIC);
    buf.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    buf.extend_from_slice(&msg_type.to_ne_bytes());
    buf.extend_from_slice(payload);
    buf
}

pub fn read_sway_frame<R: std::io::Read>(reader: &mut R) -> Result<(u32, Vec<u8>), &'static str> {
    let mut header = [0u8; 14];
    reader
        .read_exact(&mut header)
        .map_err(|_| "truncated-header")?;
    if &header[0..6] != SWAY_IPC_MAGIC {
        return Err("bad-magic");
    }
    let length = u32::from_ne_bytes(header[6..10].try_into().unwrap()) as usize;
    let msg_type = u32::from_ne_bytes(header[10..14].try_into().unwrap());
    if length > SWAY_MAX_PAYLOAD {
        return Err("payload-too-large");
    }
    let mut payload = vec![0u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|_| "truncated-payload")?;
    Ok((msg_type, payload))
}

#[derive(Deserialize, Debug)]
pub struct SwayWindowProperties {
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

// Sway IPC tree parsing
#[derive(Deserialize, Debug)]
pub struct SwayNode {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub window_properties: Option<SwayWindowProperties>,
    #[serde(default)]
    pub nodes: Vec<SwayNode>,
    #[serde(default)]
    pub floating_nodes: Vec<SwayNode>,
}

fn collect_focused_sway_nodes<'a>(node: &'a SwayNode, acc: &mut Vec<&'a SwayNode>) {
    if node.focused {
        acc.push(node);
    }
    for child in &node.nodes {
        collect_focused_sway_nodes(child, acc);
    }
    for child in &node.floating_nodes {
        collect_focused_sway_nodes(child, acc);
    }
}

pub fn parse_sway_tree_response(
    json: &str,
    generation: u64,
    now_unix_ms: u64,
) -> Result<WindowSnapshot, &'static str> {
    if json.len() > SWAY_MAX_PAYLOAD {
        return Err("payload-too-large");
    }
    let root: SwayNode = serde_json::from_str(json).map_err(|_| "invalid-json")?;
    let mut focused_nodes = Vec::new();
    collect_focused_sway_nodes(&root, &mut focused_nodes);
    if focused_nodes.is_empty() {
        return Err("no-focused-window");
    }
    if focused_nodes.len() > 1 {
        return Err("ambiguous-focus");
    }
    let focused = focused_nodes[0];

    let Some(id) = focused.id.filter(|&id| id != 0) else {
        return Err("missing-id");
    };
    let window_token = id.to_string();

    let (app, app_id, wm_class) = if focused.shell.as_deref() == Some("xwayland") {
        let Some(class) = focused
            .window_properties
            .as_ref()
            .and_then(|p| p.class.clone())
            .filter(|c| !c.is_empty())
        else {
            return Err("missing-class");
        };
        if class.len() > MAX_STRING_BYTES {
            return Err("app-too-long");
        }
        (class.clone(), None, class)
    } else {
        let Some(ref raw_app_id) = focused.app_id else {
            return Err("null-app-id");
        };
        if raw_app_id.is_empty() || raw_app_id.len() > MAX_STRING_BYTES {
            return Err("invalid-app");
        }
        (raw_app_id.clone(), Some(raw_app_id.clone()), String::new())
    };

    let title = match focused.name {
        Some(ref t) => {
            if t.len() > MAX_STRING_BYTES {
                return Err("title-too-long");
            }
            Some(t.clone())
        }
        None => None,
    };

    Ok(WindowSnapshot {
        app,
        app_id,
        title,
        pid: focused.pid,
        window_id: 0,
        wm_class,
        window_token,
        captured_unix_ms: now_unix_ms,
        generation,
    })
}

// X11 EWMH reader trait
pub trait X11PropertyReader {
    fn read_active_window(&self) -> io::Result<Option<u32>>;
    fn read_net_wm_name(&self, window: u32) -> io::Result<Option<String>>;
    fn read_wm_name(&self, window: u32) -> io::Result<Option<String>>;
    fn read_wm_class(&self, window: u32) -> io::Result<Option<(String, String)>>; // (instance, class)
    /// `_NET_WM_PID`. A missing value is not a title and does not fail the window.
    fn read_net_wm_pid(&self, _window: u32) -> io::Result<Option<u32>> {
        Ok(None)
    }
}

pub fn read_x11_ewmh_snapshot(
    reader: &dyn X11PropertyReader,
    generation: u64,
    want_title: bool,
    now_unix_ms: u64,
    expected: Option<(u32, &str)>,
) -> Result<WindowSnapshot, &'static str> {
    let active_window = reader
        .read_active_window()
        .map_err(|_| "io-error")?
        .ok_or("no-active-window")?;

    if active_window == 0 {
        return Err("no-active-window");
    }

    let (instance, class) = reader
        .read_wm_class(active_window)
        .map_err(|_| "io-error")?
        .ok_or("missing-wm-class")?;

    if class.is_empty() || class.len() > MAX_STRING_BYTES {
        return Err("invalid-class");
    }

    let second_active = reader
        .read_active_window()
        .map_err(|_| "io-error")?
        .ok_or("focus-changed")?;

    if second_active != active_window {
        return Err("focus-changed");
    }

    let app = class.clone();
    let app_id = if !instance.is_empty() && instance != class && instance.len() <= MAX_STRING_BYTES
    {
        Some(instance)
    } else {
        None
    };

    if want_title {
        let Some((expected_window, expected_class)) = expected else {
            return Err("focus-changed");
        };
        if active_window != expected_window || class != expected_class {
            return Err("focus-changed");
        }
    }

    let pid = reader.read_net_wm_pid(active_window).ok().flatten();

    let title = if want_title {
        let name = reader
            .read_net_wm_name(active_window)
            .map_err(|_| "io-error")?
            .or_else(|| reader.read_wm_name(active_window).ok().flatten());
        match name {
            Some(t) => {
                if t.len() > MAX_STRING_BYTES {
                    return Err("title-too-long");
                }
                Some(t)
            }
            None => None,
        }
    } else {
        None
    };

    Ok(WindowSnapshot {
        app,
        app_id,
        title,
        pid,
        window_id: active_window,
        wm_class: class,
        window_token: String::new(),
        captured_unix_ms: now_unix_ms,
        generation,
    })
}

use x11rb::protocol::xproto::ConnectionExt as _;

struct LiveEwmh {
    connection: x11rb::rust_connection::RustConnection,
    root: u32,
    net_active_window: u32,
    net_wm_name: u32,
    net_wm_pid: u32,
    wm_name: u32,
    wm_class: u32,
}

impl X11PropertyReader for LiveEwmh {
    fn read_active_window(&self) -> io::Result<Option<u32>> {
        let reply = self
            .connection
            .get_property(
                false,
                self.root,
                self.net_active_window,
                x11rb::protocol::xproto::AtomEnum::ANY,
                0,
                1,
            )
            .map_err(|error| io::Error::other(error.to_string()))?
            .reply()
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(reply.value32().and_then(|values| values.into_iter().next()))
    }

    fn read_net_wm_name(&self, window: u32) -> io::Result<Option<String>> {
        read_x_text(&self.connection, window, self.net_wm_name, true)
    }

    fn read_wm_name(&self, window: u32) -> io::Result<Option<String>> {
        read_x_text(&self.connection, window, self.wm_name, false)
    }

    fn read_net_wm_pid(&self, window: u32) -> io::Result<Option<u32>> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                self.net_wm_pid,
                x11rb::protocol::xproto::AtomEnum::CARDINAL,
                0,
                1,
            )
            .map_err(|error| io::Error::other(error.to_string()))?
            .reply()
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(reply.value32().and_then(|values| values.into_iter().next()))
    }

    fn read_wm_class(&self, window: u32) -> io::Result<Option<(String, String)>> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                self.wm_class,
                x11rb::protocol::xproto::AtomEnum::ANY,
                0,
                (MAX_STRING_BYTES as u32 / 4) + 2,
            )
            .map_err(|error| io::Error::other(error.to_string()))?
            .reply()
            .map_err(|error| io::Error::other(error.to_string()))?;
        if reply.value.is_empty() {
            return Ok(None);
        }
        if reply.bytes_after > 0 || reply.value.len() > MAX_STRING_BYTES {
            return Ok(Some(("".into(), "x".repeat(MAX_STRING_BYTES + 1))));
        }
        let mut parts = reply.value.split(|byte| *byte == 0);
        let instance = latin1_string(parts.next().unwrap_or_default());
        let class = latin1_string(parts.next().unwrap_or_default());
        if class.is_empty() {
            return Ok(None);
        }
        Ok(Some((instance, class)))
    }
}

fn latin1_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

fn read_x_text(
    connection: &x11rb::rust_connection::RustConnection,
    window: u32,
    atom: u32,
    utf8: bool,
) -> io::Result<Option<String>> {
    let reply = connection
        .get_property(
            false,
            window,
            atom,
            x11rb::protocol::xproto::AtomEnum::ANY,
            0,
            (MAX_STRING_BYTES as u32 / 4) + 2,
        )
        .map_err(|error| io::Error::other(error.to_string()))?
        .reply()
        .map_err(|error| io::Error::other(error.to_string()))?;
    if reply.value.is_empty() && reply.bytes_after == 0 {
        return Ok(None);
    }
    if reply.bytes_after > 0 || reply.value.len() > MAX_STRING_BYTES {
        return Ok(Some("x".repeat(MAX_STRING_BYTES + 1)));
    }
    if utf8 {
        return Ok(String::from_utf8(reply.value).ok());
    }
    Ok(Some(latin1_string(&reply.value)))
}

fn intern_atom(
    connection: &x11rb::rust_connection::RustConnection,
    name: &[u8],
) -> io::Result<u32> {
    let reply = connection
        .intern_atom(false, name)
        .map_err(|error| io::Error::other(error.to_string()))?
        .reply()
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(reply.atom)
}

fn connect_ewmh(display: &str) -> Option<LiveEwmh> {
    use x11rb::connection::Connection;
    let (connection, screen) = x11rb::connect(Some(display)).ok()?;
    let root = connection.setup().roots.get(screen)?.root;
    let net_active_window = intern_atom(&connection, b"_NET_ACTIVE_WINDOW").ok()?;
    let net_wm_name = intern_atom(&connection, b"_NET_WM_NAME").ok()?;
    let wm_name = intern_atom(&connection, b"WM_NAME").ok()?;
    let net_wm_pid = intern_atom(&connection, b"_NET_WM_PID").ok()?;
    let wm_class = intern_atom(&connection, b"WM_CLASS").ok()?;
    Some(LiveEwmh {
        connection,
        root,
        net_active_window,
        net_wm_name,
        net_wm_pid,
        wm_name,
        wm_class,
    })
}

/// Real X11 EWMH read. Callers gate this on an X11 session. A failure is unavailable.
/// Connect and every property reply share one deadline. A hung reply does not
/// stay on the caller.
pub fn read_live_ewmh(
    display: &str,
    generation: u64,
    want_title: bool,
    now_unix_ms: u64,
    expected: Option<(u32, String)>,
) -> Result<WindowSnapshot, &'static str> {
    let display = display.to_string();
    crate::desktop_session::within(HELPER_TIMEOUT, move || {
        let reader = connect_ewmh(&display).ok_or("connect-failed")?;
        let expected = expected
            .as_ref()
            .map(|(window, class)| (*window, class.as_str()));
        read_x11_ewmh_snapshot(&reader, generation, want_title, now_unix_ms, expected)
    })
    .ok_or("timeout")?
}

/// The whole property read, not only the connect, runs inside `limit`.
/// `still_current` false drops a late window after the read returns.
pub fn read_x11_ewmh_bounded<R>(
    reader: R,
    generation: u64,
    want_title: bool,
    now_unix_ms: u64,
    expected: Option<(u32, String)>,
    limit: Duration,
    still_current: impl FnOnce() -> bool + Send + 'static,
) -> Result<WindowSnapshot, &'static str>
where
    R: X11PropertyReader + Send + 'static,
{
    let snapshot = crate::desktop_session::within(limit, move || {
        let expected = expected
            .as_ref()
            .map(|(window, class)| (*window, class.as_str()));
        read_x11_ewmh_snapshot(&reader, generation, want_title, now_unix_ms, expected)
    })
    .ok_or("timeout")??;
    if !still_current() {
        return Err("stale");
    }
    Ok(snapshot)
}

// AT-SPI tree walker. Production does not call this. URL stays unavailable
// until a committed page address can be proved before the value getter.
#[cfg(test)]
pub trait AtSpiNode {
    fn role(&self) -> &'static str;
    fn is_browser_chrome(&self) -> bool;
    fn is_page_input(&self) -> bool;
    fn is_protected(&self) -> bool;
    fn is_focused(&self) -> bool;
    fn is_editing(&self) -> bool;
    fn is_dirty(&self) -> bool;
    fn is_unsubmitted(&self) -> bool;
    fn is_committed(&self) -> bool;
    fn children(&self) -> Vec<Box<dyn AtSpiNode>>;
    fn value(&self) -> Option<String>;
}

#[cfg(test)]
const SUPPORTED_BROWSER_TOKENS: &[&str] = &["firefox", "chromium", "chrome", "brave", "edge"];

#[cfg(test)]
pub fn is_supported_browser(app_id: &str) -> bool {
    let lower = app_id.to_lowercase();
    for token in SUPPORTED_BROWSER_TOKENS {
        if lower == *token {
            return true;
        }
        if let Some(stripped) = lower.strip_suffix(".desktop")
            && stripped == *token
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
pub fn walk_at_spi_url(
    app_id: &str,
    root: &dyn AtSpiNode,
    want_urls: bool,
    mut generation_check: impl FnMut() -> bool,
) -> Option<String> {
    if !want_urls || !is_supported_browser(app_id) {
        return None;
    }

    let mut visited_count = 0;
    fn walk(
        node: &dyn AtSpiNode,
        depth: usize,
        has_chrome_ancestor: bool,
        visited_count: &mut usize,
        generation_check: &mut dyn FnMut() -> bool,
    ) -> Option<String> {
        if depth > 6 || *visited_count >= 64 {
            return None;
        }
        *visited_count += 1;

        if !generation_check() {
            return None;
        }

        let is_chrome = has_chrome_ancestor || node.is_browser_chrome();

        if is_chrome && !node.is_page_input() {
            // Check gating flags before value getter
            if !node.is_protected()
                && !node.is_focused()
                && !node.is_editing()
                && !node.is_dirty()
                && !node.is_unsubmitted()
            {
                // In production, is_committed() is false, so value() is NEVER called!
                if node.is_committed()
                    && let Some(val) = node.value()
                {
                    if !generation_check() {
                        return None;
                    }
                    if val.len() <= MAX_STRING_BYTES {
                        return sanitize_url(&val);
                    }
                }
            }
        }

        for child in node.children() {
            if let Some(url) = walk(
                child.as_ref(),
                depth + 1,
                is_chrome,
                visited_count,
                generation_check,
            ) {
                return Some(url);
            }
        }
        None
    }

    walk(root, 0, false, &mut visited_count, &mut generation_check)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn test_kwin_snapshot_parsing() {
        let json = r#"{
            "desktopFileName": "org.mozilla.firefox",
            "resourceClass": "firefox",
            "resourceName": "Navigator",
            "caption": "Mozilla Firefox",
            "pid": 1234,
            "captured_unix_ms": 10000,
            "generation": 42
        }"#;

        let snap = parse_kwin_snapshot(json, 42, 12000).unwrap();
        assert_eq!(snap.app, "org.mozilla.firefox");
        assert_eq!(snap.app_id, Some("firefox".into()));
        assert_eq!(snap.title, Some("Mozilla Firefox".into()));
        assert_eq!(snap.pid, Some(1234));
        assert_eq!(snap.generation, 42);

        // Stale generation rejected
        assert_eq!(
            parse_kwin_snapshot(json, 41, 12000),
            Err("stale-generation")
        );

        // Age > 5000ms rejected
        assert_eq!(
            parse_kwin_snapshot(json, 42, 16000),
            Err("snapshot-expired")
        );

        // Oversized body rejected
        let large_json = format!("{{\"desktopFileName\":\"{}\"}}", "x".repeat(70000));
        assert_eq!(
            parse_kwin_snapshot(&large_json, 42, 10000),
            Err("payload-too-large")
        );
    }

    #[test]
    fn test_gnome_reply_parsing() {
        let json = r#"{
            "v": 1,
            "generation": 5,
            "captured_unix_ms": 20000,
            "app": "Files",
            "app_id": "org.gnome.Nautilus",
            "title": "Home",
            "pid": 4321
        }"#;

        let snap = parse_gnome_reply(json, 5, 21000).unwrap();
        assert_eq!(snap.app, "Files");
        assert_eq!(snap.app_id, Some("org.gnome.Nautilus".into()));
        assert_eq!(snap.title, Some("Home".into()));
        assert_eq!(snap.pid, Some(4321));

        // Age > 5000 rejected
        assert_eq!(parse_gnome_reply(json, 5, 26000), Err("snapshot-expired"));
    }

    #[test]
    fn test_caller_uid_verification() {
        assert!(verify_caller_uid(1000, 1000).is_ok());
        assert_eq!(verify_caller_uid(1001, 1000), Err("caller-unauthorized"));
    }

    #[test]
    fn test_sway_tree_parsing_fixtures() {
        let native_json = include_str!("fixtures/sway-native.json");
        let native_snap = parse_sway_tree_response(native_json, 1, 1000).unwrap();
        assert_eq!(native_snap.app, "org.solpbc.SyntheticSwayFixture");
        assert_eq!(
            native_snap.app_id,
            Some("org.solpbc.SyntheticSwayFixture".into())
        );
        assert_eq!(native_snap.window_token, "5");
        assert_eq!(native_snap.window_id, 0);
        assert_eq!(native_snap.pid, Some(42771));
        assert_eq!(
            native_snap.title,
            Some("synthetic sway native fixture".into())
        );

        let native_frame = include_bytes!("fixtures/sway-native.frame");
        let mut reader = &native_frame[..];
        let (msg_type, payload) = read_sway_frame(&mut reader).unwrap();
        assert_eq!(msg_type, 4);
        assert_eq!(payload, native_json.as_bytes());

        let xwayland_json = include_str!("fixtures/sway-xwayland.json");
        let xwayland_snap = parse_sway_tree_response(xwayland_json, 1, 1000).unwrap();
        assert_eq!(xwayland_snap.app, "XTerm");
        assert_eq!(xwayland_snap.app_id, None);
        assert_eq!(xwayland_snap.window_token, "6");
        assert_eq!(xwayland_snap.window_id, 0);
        assert_eq!(xwayland_snap.pid, Some(42832));
        assert_eq!(
            xwayland_snap.title,
            Some("synthetic sway Xwayland fixture".into())
        );

        let xwayland_frame = include_bytes!("fixtures/sway-xwayland.frame");
        let mut reader = &xwayland_frame[..];
        let (msg_type, payload) = read_sway_frame(&mut reader).unwrap();
        assert_eq!(msg_type, 4);
        assert_eq!(payload, xwayland_json.as_bytes());
    }

    #[test]
    fn test_sway_framing_encode_and_negatives() {
        let cmd = encode_sway_command(SWAY_GET_TREE_TYPE, &[]);
        assert_eq!(cmd.len(), 14);
        assert_eq!(&cmd[0..6], SWAY_IPC_MAGIC);
        assert_eq!(u32::from_ne_bytes(cmd[6..10].try_into().unwrap()), 0);
        assert_eq!(u32::from_ne_bytes(cmd[10..14].try_into().unwrap()), 4);

        // Bad magic
        let mut bad_magic = cmd.clone();
        bad_magic[0] = b'x';
        assert_eq!(read_sway_frame(&mut &bad_magic[..]), Err("bad-magic"));

        // Truncated header
        assert_eq!(read_sway_frame(&mut &cmd[..10]), Err("truncated-header"));

        // Payload cap exceeded
        let mut overlong_hdr = Vec::new();
        overlong_hdr.extend_from_slice(SWAY_IPC_MAGIC);
        overlong_hdr.extend_from_slice(&((SWAY_MAX_PAYLOAD + 1) as u32).to_ne_bytes());
        overlong_hdr.extend_from_slice(&4u32.to_ne_bytes());
        assert_eq!(
            read_sway_frame(&mut &overlong_hdr[..]),
            Err("payload-too-large")
        );

        // Truncated payload
        let mut truncated_frame = Vec::new();
        truncated_frame.extend_from_slice(SWAY_IPC_MAGIC);
        truncated_frame.extend_from_slice(&100u32.to_ne_bytes());
        truncated_frame.extend_from_slice(&4u32.to_ne_bytes());
        truncated_frame.extend_from_slice(b"short");
        assert_eq!(
            read_sway_frame(&mut &truncated_frame[..]),
            Err("truncated-payload")
        );
    }

    #[test]
    fn test_sway_tree_negative_cases() {
        let native_json = include_str!("fixtures/sway-native.json");

        // Missing native app_id
        let without_app_id = native_json.replace("\"org.solpbc.SyntheticSwayFixture\"", "null");
        assert_eq!(
            parse_sway_tree_response(&without_app_id, 1, 1000),
            Err("null-app-id")
        );

        // Missing id
        let without_id = native_json.replace("\"id\": 5,", "\"id\": 0,");
        assert_eq!(
            parse_sway_tree_response(&without_id, 1, 1000),
            Err("missing-id")
        );

        // No focused window
        let no_focus = native_json.replace("\"focused\": true", "\"focused\": false");
        assert_eq!(
            parse_sway_tree_response(&no_focus, 1, 1000),
            Err("no-focused-window")
        );

        // Ambiguous focus (duplicate focused: true)
        let dup_focus = native_json.replacen("\"focused\": false", "\"focused\": true", 1);
        assert_eq!(
            parse_sway_tree_response(&dup_focus, 1, 1000),
            Err("ambiguous-focus")
        );

        // Body over cap
        let overlong_json = " ".repeat(SWAY_MAX_PAYLOAD + 1);
        assert_eq!(
            parse_sway_tree_response(&overlong_json, 1, 1000),
            Err("payload-too-large")
        );
    }

    struct MockX11Reader {
        active: Vec<Option<u32>>,
        active_call_idx: Cell<usize>,
        class: Option<(String, String)>,
        net_name: Option<String>,
        wm_name: Option<String>,
        name_call_count: Cell<usize>,
    }

    impl MockX11Reader {
        fn single_active(
            active: Option<u32>,
            class: Option<(String, String)>,
            net_name: Option<String>,
            wm_name: Option<String>,
        ) -> Self {
            Self {
                active: vec![active, active],
                active_call_idx: Cell::new(0),
                class,
                net_name,
                wm_name,
                name_call_count: Cell::new(0),
            }
        }
    }

    impl X11PropertyReader for MockX11Reader {
        fn read_active_window(&self) -> io::Result<Option<u32>> {
            let idx = self.active_call_idx.get();
            self.active_call_idx.set(idx + 1);
            if idx < self.active.len() {
                Ok(self.active[idx])
            } else {
                Ok(self.active.last().copied().flatten())
            }
        }
        fn read_net_wm_name(&self, _w: u32) -> io::Result<Option<String>> {
            self.name_call_count.set(self.name_call_count.get() + 1);
            Ok(self.net_name.clone())
        }
        fn read_wm_name(&self, _w: u32) -> io::Result<Option<String>> {
            self.name_call_count.set(self.name_call_count.get() + 1);
            Ok(self.wm_name.clone())
        }
        fn read_wm_class(&self, _w: u32) -> io::Result<Option<(String, String)>> {
            Ok(self.class.clone())
        }
    }

    #[test]
    fn test_x11_ewmh_reading() {
        let reader = MockX11Reader::single_active(
            Some(0x123),
            Some(("gimp-2.10".into(), "Gimp".into())),
            Some("GNU Image Manipulation Program".into()),
            None,
        );

        let snap = read_x11_ewmh_snapshot(&reader, 1, true, 5000, Some((0x123, "Gimp"))).unwrap();
        assert_eq!(snap.app, "Gimp");
        assert_eq!(snap.app_id, Some("gimp-2.10".into()));
        assert_eq!(snap.title, Some("GNU Image Manipulation Program".into()));
        assert_eq!(reader.name_call_count.get(), 1);
    }

    #[test]
    fn test_x11_ewmh_focus_changed() {
        let reader = MockX11Reader {
            active: vec![Some(0x123), Some(0x456)],
            active_call_idx: Cell::new(0),
            class: Some(("gimp-2.10".into(), "Gimp".into())),
            net_name: Some("GNU Image Manipulation Program".into()),
            wm_name: None,
            name_call_count: Cell::new(0),
        };

        let res = read_x11_ewmh_snapshot(&reader, 1, true, 5000, Some((0x123, "Gimp")));
        assert_eq!(res, Err("focus-changed"));
        assert_eq!(reader.name_call_count.get(), 0);
    }

    #[test]
    fn test_x11_title_read_is_bound_to_the_earlier_window() {
        let reader = MockX11Reader {
            active: vec![Some(0x222), Some(0x222)],
            active_call_idx: Cell::new(0),
            class: Some(("excluded".into(), "Excluded".into())),
            net_name: Some("SECRET_TITLE".into()),
            wm_name: Some("SECRET_TITLE".into()),
            name_call_count: Cell::new(0),
        };
        let res = read_x11_ewmh_snapshot(&reader, 1, true, 5000, Some((0x111, "Allowed")));
        assert_eq!(res, Err("focus-changed"));
        assert_eq!(reader.name_call_count.get(), 0);
    }

    struct StallProperty {
        entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
        release: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl X11PropertyReader for StallProperty {
        fn read_active_window(&self) -> io::Result<Option<u32>> {
            self.entered
                .store(true, std::sync::atomic::Ordering::Release);
            while !self.release.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(Some(1))
        }
        fn read_net_wm_name(&self, _: u32) -> io::Result<Option<String>> {
            Ok(Some("SECRET_TITLE".into()))
        }
        fn read_wm_name(&self, _: u32) -> io::Result<Option<String>> {
            Ok(Some("SECRET_TITLE".into()))
        }
        fn read_wm_class(&self, _: u32) -> io::Result<Option<(String, String)>> {
            Ok(Some(("secret".into(), "Secret".into())))
        }
    }

    #[test]
    fn stalled_property_reply_does_not_return_a_window() {
        let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = StallProperty {
            entered: std::sync::Arc::clone(&entered),
            release: std::sync::Arc::clone(&release),
        };
        let result = read_x11_ewmh_bounded(
            reader,
            1,
            true,
            0,
            Some((1, "Secret".into())),
            Duration::from_millis(40),
            || true,
        );
        assert_eq!(result, Err("timeout"));
        assert!(entered.load(std::sync::atomic::Ordering::Acquire));
        release.store(true, std::sync::atomic::Ordering::Release);
    }

    #[test]
    fn late_ewmh_window_is_discarded_when_the_epoch_moved() {
        let reader = MockX11Reader::single_active(
            Some(0x123),
            Some(("gimp".into(), "Gimp".into())),
            Some("SECRET_TITLE".into()),
            None,
        );
        let result = read_x11_ewmh_bounded(
            reader,
            1,
            true,
            5_000,
            Some((0x123, "Gimp".into())),
            Duration::from_secs(2),
            || false,
        );
        assert_eq!(result, Err("stale"));
    }

    struct MockAtSpiNode {
        role: &'static str,
        is_chrome: bool,
        is_page: bool,
        is_protected: bool,
        is_focused: bool,
        is_editing: bool,
        is_dirty: bool,
        is_unsubmitted: bool,
        committed: bool,
        value_call_count: Rc<Cell<usize>>,
        value: Option<String>,
        children: Vec<MockAtSpiNode>,
    }

    impl AtSpiNode for MockAtSpiNode {
        fn role(&self) -> &'static str {
            self.role
        }
        fn is_browser_chrome(&self) -> bool {
            self.is_chrome
        }
        fn is_page_input(&self) -> bool {
            self.is_page
        }
        fn is_protected(&self) -> bool {
            self.is_protected
        }
        fn is_focused(&self) -> bool {
            self.is_focused
        }
        fn is_editing(&self) -> bool {
            self.is_editing
        }
        fn is_dirty(&self) -> bool {
            self.is_dirty
        }
        fn is_unsubmitted(&self) -> bool {
            self.is_unsubmitted
        }
        fn is_committed(&self) -> bool {
            self.committed
        }
        fn children(&self) -> Vec<Box<dyn AtSpiNode>> {
            self.children
                .iter()
                .map(|c| {
                    Box::new(MockAtSpiNode {
                        role: c.role,
                        is_chrome: c.is_chrome,
                        is_page: c.is_page,
                        is_protected: c.is_protected,
                        is_focused: c.is_focused,
                        is_editing: c.is_editing,
                        is_dirty: c.is_dirty,
                        is_unsubmitted: c.is_unsubmitted,
                        committed: c.committed,
                        value_call_count: Rc::clone(&c.value_call_count),
                        value: c.value.clone(),
                        children: Vec::new(),
                    }) as Box<dyn AtSpiNode>
                })
                .collect()
        }
        fn value(&self) -> Option<String> {
            self.value_call_count.set(self.value_call_count.get() + 1);
            self.value.clone()
        }
    }

    #[test]
    fn test_at_spi_production_never_calls_value() {
        let calls = Rc::new(Cell::new(0));
        let node = MockAtSpiNode {
            role: "entry",
            is_chrome: true,
            is_page: false,
            is_protected: false,
            is_focused: false,
            is_editing: false,
            is_dirty: false,
            is_unsubmitted: false,
            committed: false, // Production is always false!
            value_call_count: Rc::clone(&calls),
            value: Some("https://example.com/page?query=1".into()),
            children: Vec::new(),
        };

        let result = walk_at_spi_url("firefox", &node, true, || true);
        assert_eq!(result, None);
        assert_eq!(calls.get(), 0); // Zero value-getter calls!
    }

    #[test]
    fn test_at_spi_focused_dirty_unsubmitted_never_call_value() {
        for (focused, dirty, unsubmitted) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let calls = Rc::new(Cell::new(0));
            let node = MockAtSpiNode {
                role: "entry",
                is_chrome: true,
                is_page: false,
                is_protected: false,
                is_focused: focused,
                is_editing: false,
                is_dirty: dirty,
                is_unsubmitted: unsubmitted,
                committed: true, // Even if committed is set true in test!
                value_call_count: Rc::clone(&calls),
                value: Some("https://example.com/page".into()),
                children: Vec::new(),
            };

            let result = walk_at_spi_url("google-chrome", &node, true, || true);
            assert_eq!(result, None);
            assert_eq!(calls.get(), 0); // Assert zero value-getter calls!
        }
    }

    #[test]
    fn test_at_spi_fake_committed_success() {
        let calls = Rc::new(Cell::new(0));
        let node = MockAtSpiNode {
            role: "entry",
            is_chrome: true,
            is_page: false,
            is_protected: false,
            is_focused: false,
            is_editing: false,
            is_dirty: false,
            is_unsubmitted: false,
            committed: true,
            value_call_count: Rc::clone(&calls),
            value: Some("https://example.com/docs?token=secret#section".into()),
            children: Vec::new(),
        };

        let result = walk_at_spi_url("firefox", &node, true, || true);
        assert_eq!(result, Some("https://example.com/docs".into()));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn test_is_supported_browser_exact() {
        assert!(is_supported_browser("firefox"));
        assert!(is_supported_browser("firefox.desktop"));
        assert!(is_supported_browser("chrome"));
        assert!(is_supported_browser("chrome.desktop"));
        assert!(is_supported_browser("brave"));
        assert!(is_supported_browser("edge"));
        assert!(is_supported_browser("chromium"));
        assert!(!is_supported_browser("notchrome"));
        assert!(!is_supported_browser("firefox-bin"));
    }

    #[test]
    fn test_parser_boundaries() {
        // Age boundary: 5000 accepted, 5001 rejected
        assert!(check_age(10000, 15000));
        assert!(!check_age(10000, 15001));

        // Title length boundary: 4096 accepted, 4097 rejected
        let title_4096 = "a".repeat(4096);
        let title_4097 = "a".repeat(4097);
        let json_4096 = format!(
            r#"{{"v":1,"generation":1,"captured_unix_ms":1000,"app":"App","title":"{}"}}"#,
            title_4096
        );
        let json_4097 = format!(
            r#"{{"v":1,"generation":1,"captured_unix_ms":1000,"app":"App","title":"{}"}}"#,
            title_4097
        );
        let snap_4096 = parse_gnome_reply(&json_4096, 1, 1000).unwrap();
        assert_eq!(snap_4096.title, Some(title_4096));
        assert_eq!(
            parse_gnome_reply(&json_4097, 1, 1000),
            Err("title-too-long")
        );

        // Body size boundary: 65536 accepted, 65537 rejected
        let prefix =
            r#"{"v":1,"generation":1,"captured_unix_ms":1000,"app":"A","title":"","_pad":""#;
        let suffix = r#""}"#;
        let p_count = 65536 - prefix.len() - suffix.len();
        let padded_json_65536 = format!("{}{}{}", prefix, "p".repeat(p_count), suffix);
        assert_eq!(padded_json_65536.len(), 65536);
        assert!(parse_gnome_reply(&padded_json_65536, 1, 1000).is_ok());

        let padded_json_65537 = format!("{} ", padded_json_65536);
        assert_eq!(padded_json_65537.len(), 65537);
        assert_eq!(
            parse_gnome_reply(&padded_json_65537, 1, 1000),
            Err("payload-too-large")
        );

        // Non-JSON body length check
        let non_json_65536 = "x".repeat(65536);
        assert_eq!(
            parse_gnome_reply(&non_json_65536, 1, 1000),
            Err("invalid-json")
        );
        let non_json_65537 = "x".repeat(65537);
        assert_eq!(
            parse_gnome_reply(&non_json_65537, 1, 1000),
            Err("payload-too-large")
        );

        // withhold_for_title_exclusion tests
        assert!(withhold_for_title_exclusion(None, &["Secret".into()]));
        assert!(!withhold_for_title_exclusion(None, &[]));
        assert!(!withhold_for_title_exclusion(
            Some("Other"),
            &["Secret".into()]
        ));
    }
}
