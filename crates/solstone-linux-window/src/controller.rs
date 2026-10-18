// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use solstone_linux::dbus_client::ControlClientError;
use solstone_linux::window_control::{
    ControlModel, SessionView, SettingsModel, decode_control_str,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InFlight<T> {
    pub id: u64,
    pub bus_owner: String,
    pub payload: T,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntakeCommand {
    Pause(i32),
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingCommand {
    Begin(String),
    Answer {
        attempt_id: String,
        pairing_id: String,
        accept: bool,
    },
    Cancel(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsDraft {
    pub framerate: Option<i64>,
    pub framerate_dirty: bool,
    pub cursor: Option<bool>,
    pub cursor_dirty: bool,
    pub start_paused: Option<bool>,
    pub start_paused_dirty: bool,
    pub segment: Option<i64>,
    pub segment_dirty: bool,
    pub in_flight: Option<InFlight<SettingsModel>>,
    pub saved_verified: bool,
    pub write_failed: bool,
    pub waiting_confirming_read: bool,
}

impl SettingsDraft {
    pub fn is_dirty(&self) -> bool {
        self.framerate_dirty || self.cursor_dirty || self.start_paused_dirty || self.segment_dirty
    }

    pub fn changed_writes(&self) -> Vec<(&'static str, String)> {
        let mut writes = Vec::new();
        if self.framerate_dirty
            && let Some(val) = self.framerate
        {
            writes.push(("capture_framerate", val.to_string()));
        }
        if self.cursor_dirty
            && let Some(val) = self.cursor
        {
            writes.push(("draw_cursor", val.to_string()));
        }
        if self.start_paused_dirty
            && let Some(val) = self.start_paused
        {
            writes.push(("start_paused", val.to_string()));
        }
        if self.segment_dirty
            && let Some(val) = self.segment
        {
            writes.push(("segment_interval", val.to_string()));
        }
        writes
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogSlot {
    pub in_flight: Option<InFlight<()>>,
    pub frozen: Option<crate::logs::FrozenLog>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum StartActionKind {
    Start,
}

#[allow(dead_code)]
pub struct Controller {
    next_id: u64,
    pub bus_owner: Option<String>,
    pub document: ControlModel,
    pub intake_command: Option<InFlight<IntakeCommand>>,
    pub intake_error: Option<String>,
    pub pairing: Option<InFlight<PairingCommand>>,
    pub pairing_attempt_outcome: Option<String>,
    pub pairing_error: Option<String>,
    pub settings_draft: SettingsDraft,
    pub logs: LogSlot,
    pub start: Option<InFlight<StartActionKind>>,
    pub start_error: Option<String>,
    pub restart: Option<InFlight<()>>,
    pub restart_error: Option<String>,
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl Controller {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            bus_owner: None,
            document: ControlModel::unknown(),
            intake_command: None,
            intake_error: None,
            pairing: None,
            pairing_attempt_outcome: None,
            pairing_error: None,
            settings_draft: SettingsDraft::default(),
            logs: LogSlot::default(),
            start: None,
            start_error: None,
            restart: None,
            restart_error: None,
        }
    }

    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn set_bus_owner(&mut self, owner: Option<String>) {
        if self.bus_owner != owner {
            self.bus_owner = owner;
        }
    }

    pub fn start_intake_command(&mut self, bus_owner: &str, command: IntakeCommand) -> u64 {
        let id = self.alloc_id();
        self.intake_command = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: command,
        });
        self.intake_error = None;
        id
    }

    pub fn start_pairing(&mut self, bus_owner: &str, command: PairingCommand) -> u64 {
        let id = self.alloc_id();
        self.pairing = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: command,
        });
        self.pairing_error = None;
        id
    }

    pub fn start_settings_save(&mut self, bus_owner: &str, settings: SettingsModel) -> u64 {
        let id = self.alloc_id();
        self.settings_draft.in_flight = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: settings,
        });
        self.settings_draft.saved_verified = false;
        self.settings_draft.write_failed = false;
        self.settings_draft.waiting_confirming_read = true;
        id
    }

    pub fn start_logs_request(&mut self, bus_owner: &str) -> u64 {
        let id = self.alloc_id();
        self.logs.in_flight = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: (),
        });
        id
    }

    pub fn start_service_start(&mut self, bus_owner: &str) -> u64 {
        let id = self.alloc_id();
        self.start = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: StartActionKind::Start,
        });
        self.start_error = None;
        id
    }

    pub fn start_service_restart(&mut self, bus_owner: &str) -> u64 {
        let id = self.alloc_id();
        self.restart = Some(InFlight {
            id,
            bus_owner: bus_owner.to_string(),
            payload: (),
        });
        self.restart_error = None;
        id
    }

    pub fn apply_status_reply(
        &mut self,
        id: u64,
        reply_owner: &str,
        reply: Result<String, ControlClientError>,
    ) {
        if Some(reply_owner) != self.bus_owner.as_deref() {
            return;
        }

        let model = match reply {
            Ok(json_str) => match decode_control_str(&json_str) {
                Ok(m) if m.view != SessionView::Unknown => m,
                _ => ControlModel::unknown(),
            },
            Err(_) => ControlModel::unknown(),
        };

        self.document = model;

        // Dirty settings fields stay dirty until a fresh read shows that field's requested value
        if self.document.saved_available {
            if self.settings_draft.framerate_dirty
                && let Some(val) = self.settings_draft.framerate
                && val == self.document.settings.capture_framerate
            {
                self.settings_draft.framerate_dirty = false;
            }
            if self.settings_draft.cursor_dirty
                && let Some(val) = self.settings_draft.cursor
                && val == self.document.settings.draw_cursor
            {
                self.settings_draft.cursor_dirty = false;
            }
            if self.settings_draft.start_paused_dirty
                && let Some(val) = self.settings_draft.start_paused
                && val == self.document.settings.start_paused
            {
                self.settings_draft.start_paused_dirty = false;
            }
            if self.settings_draft.segment_dirty
                && let Some(val) = self.settings_draft.segment
                && val == self.document.settings.segment_interval
            {
                self.settings_draft.segment_dirty = false;
            }
        }

        if self.settings_draft.waiting_confirming_read
            && self.document.saved_available
            && self.settings_draft.in_flight.is_none()
            && !self.settings_draft.write_failed
            && !self.settings_draft.is_dirty()
        {
            self.settings_draft.saved_verified = true;
            self.settings_draft.waiting_confirming_read = false;
        }

        // Completion of Pause/Resume: later document whose pause facts match the request
        if let Some(in_flight) = &self.intake_command
            && in_flight.id <= id
        {
            match in_flight.payload {
                IntakeCommand::Pause(_) => {
                    if self.document.capture == "paused" || self.document.pause_bounded.is_some() {
                        self.intake_command = None;
                    }
                }
                IntakeCommand::Resume => {
                    if self.document.capture != "paused" && self.document.pause_bounded.is_none() {
                        self.intake_command = None;
                    }
                }
            }
        }
    }

    pub fn apply_intake_reply(
        &mut self,
        id: u64,
        reply_owner: &str,
        reply: Result<String, ControlClientError>,
    ) {
        if let Some(in_flight) = &self.intake_command
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
            && let Err(e) = reply
        {
            self.intake_error = Some(e.to_string());
            self.intake_command = None;
        }
        // If Ok("ok"), do not clear intake_command; wait for matching status document
    }

    pub fn apply_pairing_reply(
        &mut self,
        id: u64,
        reply_owner: &str,
        reply: Result<String, ControlClientError>,
    ) {
        if let Some(in_flight) = &self.pairing
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
        {
            match reply {
                Ok(payload) => {
                    self.pairing = None;
                    self.pairing_error = None;
                    if let Ok(model) = decode_control_str(&payload) {
                        self.document = model;
                    }
                }
                Err(e) => {
                    self.pairing_error = Some(e.to_string());
                    self.pairing = None;
                }
            }
        }
    }

    pub fn apply_settings_write_reply(
        &mut self,
        id: u64,
        reply_owner: &str,
        reply: Result<String, ControlClientError>,
    ) {
        if let Some(in_flight) = &self.settings_draft.in_flight
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
        {
            match reply {
                Ok(_) => {
                    self.settings_draft.in_flight = None;
                }
                Err(_) => {
                    self.settings_draft.in_flight = None;
                    self.settings_draft.waiting_confirming_read = false;
                    self.settings_draft.write_failed = true;
                }
            }
        }
    }

    pub fn apply_logs_reply(&mut self, id: u64, reply_owner: &str, frozen: crate::logs::FrozenLog) {
        if let Some(in_flight) = &self.logs.in_flight
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
        {
            self.logs.frozen = Some(frozen);
            self.logs.in_flight = None;
        }
    }

    pub fn apply_start_reply(&mut self, id: u64, reply_owner: &str, res: Result<(), String>) {
        if let Some(in_flight) = &self.start
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
        {
            match res {
                Ok(()) => {
                    self.start = None;
                    self.start_error = None;
                }
                Err(e) => {
                    self.start_error = Some(e);
                    self.start = None;
                }
            }
        }
    }

    pub fn apply_restart_reply(&mut self, id: u64, reply_owner: &str, res: Result<(), String>) {
        if let Some(in_flight) = &self.restart
            && in_flight.id == id
            && in_flight.bus_owner == reply_owner
        {
            match res {
                Ok(()) => {
                    self.restart = None;
                    self.restart_error = None;
                }
                Err(e) => {
                    self.restart_error = Some(e);
                    self.restart = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_reply_does_not_overwrite_settings_draft_or_logs() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        controller.settings_draft.framerate = Some(7);
        controller.settings_draft.framerate_dirty = true;
        controller.logs.frozen = Some(crate::logs::FrozenLog {
            content: "pre-existing log".into(),
            outcome: crate::logs::LogOutcome::Complete,
        });

        let doc_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": ":1.100",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 5,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            },
            "saved_available": true
        }"#;

        controller.apply_status_reply(1, ":1.100", Ok(doc_json.into()));

        // Settings draft is preserved and still dirty because 5 != 7
        assert_eq!(controller.settings_draft.framerate, Some(7));
        assert!(controller.settings_draft.framerate_dirty);

        // Logs are untouched
        assert_eq!(
            controller.logs.frozen.as_ref().unwrap().content,
            "pre-existing log"
        );
    }

    #[test]
    fn test_clean_available_status_read_does_not_set_saved_verified() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        let doc_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": ":1.100",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 5,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            },
            "saved_available": true
        }"#;

        controller.apply_status_reply(1, ":1.100", Ok(doc_json.into()));
        assert!(!controller.settings_draft.saved_verified);
    }

    #[test]
    fn test_dirty_fields_clear_when_read_matches_requested() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        controller.settings_draft.framerate = Some(7);
        controller.settings_draft.framerate_dirty = true;

        let doc_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": ":1.100",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 7,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            },
            "saved_available": true
        }"#;

        controller.apply_status_reply(1, ":1.100", Ok(doc_json.into()));
        assert!(!controller.settings_draft.framerate_dirty);
        assert!(!controller.settings_draft.saved_verified);
    }

    #[test]
    fn test_saved_verified_after_save_write_ok_and_matching_read() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        controller.settings_draft.framerate = Some(7);
        controller.settings_draft.framerate_dirty = true;

        let settings = SettingsModel {
            capture_framerate: 7,
            draw_cursor: true,
            start_paused: false,
            segment_interval: 10,
        };
        let save_id = controller.start_settings_save(":1.100", settings);
        assert!(!controller.settings_draft.saved_verified);
        assert!(controller.settings_draft.waiting_confirming_read);

        controller.apply_settings_write_reply(save_id, ":1.100", Ok("ok".into()));
        assert!(!controller.settings_draft.saved_verified);
        assert!(controller.settings_draft.waiting_confirming_read);

        let doc_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": ":1.100",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 7,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            },
            "saved_available": true
        }"#;

        controller.apply_status_reply(1, ":1.100", Ok(doc_json.into()));
        assert!(!controller.settings_draft.framerate_dirty);
        assert!(controller.settings_draft.saved_verified);
        assert!(!controller.settings_draft.waiting_confirming_read);
    }

    #[test]
    fn test_status_err_after_write_ok_leaves_saved_verified_and_write_failure_false() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        controller.settings_draft.framerate = Some(7);
        controller.settings_draft.framerate_dirty = true;

        let settings = SettingsModel {
            capture_framerate: 7,
            draw_cursor: true,
            start_paused: false,
            segment_interval: 10,
        };
        let save_id = controller.start_settings_save(":1.100", settings);
        controller.apply_settings_write_reply(save_id, ":1.100", Ok("ok".into()));

        controller.apply_status_reply(
            1,
            ":1.100",
            Err(ControlClientError::Call("transport failure".into())),
        );
        assert!(!controller.settings_draft.saved_verified);
        assert!(!controller.settings_draft.write_failed);
        assert!(controller.settings_draft.waiting_confirming_read);
        assert!(controller.settings_draft.framerate_dirty);
    }

    #[test]
    fn test_write_err_sets_write_failure_and_prevents_saved_verified() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        controller.settings_draft.framerate = Some(7);
        controller.settings_draft.framerate_dirty = true;

        let settings = SettingsModel {
            capture_framerate: 7,
            draw_cursor: true,
            start_paused: false,
            segment_interval: 10,
        };
        let save_id = controller.start_settings_save(":1.100", settings);
        controller.apply_settings_write_reply(
            save_id,
            ":1.100",
            Err(ControlClientError::Call("failed write".into())),
        );
        assert!(controller.settings_draft.write_failed);
        assert!(!controller.settings_draft.waiting_confirming_read);

        let doc_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": ":1.100",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 7,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            },
            "saved_available": true
        }"#;

        controller.apply_status_reply(1, ":1.100", Ok(doc_json.into()));
        assert!(!controller.settings_draft.saved_verified);
    }

    #[test]
    fn test_changed_writes_contains_only_dirty_fields() {
        let mut draft = SettingsDraft::default();
        assert!(draft.changed_writes().is_empty());

        draft.framerate = Some(8);
        assert!(draft.changed_writes().is_empty()); // not dirty yet

        draft.framerate_dirty = true;
        assert_eq!(
            draft.changed_writes(),
            vec![("capture_framerate", "8".to_string())]
        );

        draft.cursor = Some(false);
        draft.cursor_dirty = true;
        draft.start_paused = Some(true);
        draft.start_paused_dirty = true;
        draft.segment = Some(15);
        draft.segment_dirty = true;

        assert_eq!(
            draft.changed_writes(),
            vec![
                ("capture_framerate", "8".to_string()),
                ("draw_cursor", "false".to_string()),
                ("start_paused", "true".to_string()),
                ("segment_interval", "15".to_string()),
            ]
        );
    }

    #[test]
    fn test_superseded_or_mismatched_owner_is_ignored() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        let id1 = controller.start_intake_command(":1.100", IntakeCommand::Pause(10));
        let id2 = controller.start_intake_command(":1.100", IntakeCommand::Resume);

        // Reply for old id1 is ignored
        controller.apply_intake_reply(id1, ":1.100", Err(ControlClientError::Timeout));
        assert!(controller.intake_error.is_none());
        assert_eq!(controller.intake_command.as_ref().unwrap().id, id2);

        // Reply for wrong owner is ignored
        controller.apply_intake_reply(id2, ":1.999", Err(ControlClientError::NoOwner));
        assert!(controller.intake_error.is_none());
    }

    #[test]
    fn test_pause_ok_does_not_complete_until_subsequent_status_matches() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        let id = controller.start_intake_command(":1.100", IntakeCommand::Pause(60));
        controller.apply_intake_reply(id, ":1.100", Ok("ok".into()));

        // In-flight command is NOT completed yet
        assert!(controller.intake_command.is_some());

        // Status still says recording
        let rec_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": ":1.100",
            "health": "connected",
            "capture": "recording",
            "settings": {
                "capture_framerate": 5,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            }
        }"#;
        controller.apply_status_reply(id, ":1.100", Ok(rec_json.into()));
        assert!(controller.intake_command.is_some());

        // Now status says paused
        let pau_json = r#"{
            "v": 1,
            "durable": "confirmed",
            "owner": ":1.100",
            "health": "connected",
            "capture": "paused",
            "pause_bounded": true,
            "settings": {
                "capture_framerate": 5,
                "draw_cursor": true,
                "start_paused": false,
                "segment_interval": 10
            }
        }"#;
        controller.apply_status_reply(id, ":1.100", Ok(pau_json.into()));
        assert!(controller.intake_command.is_none());
    }

    #[test]
    fn test_transport_error_decodes_to_unknown() {
        let mut controller = Controller::new();
        controller.set_bus_owner(Some(":1.100".into()));

        controller.apply_status_reply(1, ":1.100", Err(ControlClientError::Timeout));
        assert_eq!(controller.document.view, SessionView::Unknown);
        assert!(!controller.document.saved_available);
    }
}
