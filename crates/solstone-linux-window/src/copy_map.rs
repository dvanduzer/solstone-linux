// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use solstone_linux::copy::CopyKey;
use solstone_linux::window_control::{ControlModel, SettingsModel, adapt_graphical_link};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IntakeKeys {
    pub headline: Option<CopyKey>,
    pub detail: Option<CopyKey>,
    pub action: Option<CopyKey>,
}

pub fn map_intake(model: &ControlModel) -> IntakeKeys {
    if model.capture_attention {
        return IntakeKeys {
            headline: Some(CopyKey::StatusAttention),
            detail: None,
            action: None,
        };
    }
    if model.durable == "confirmed" && model.fault.as_deref() == Some("owner-stop-failed") {
        return IntakeKeys {
            headline: Some(CopyKey::StatusAttention),
            detail: None,
            action: None,
        };
    }
    if model.capture == "recording" || model.capture == "on" {
        return IntakeKeys {
            headline: Some(CopyKey::StatusOn),
            detail: Some(CopyKey::StatusOnDetail),
            action: Some(CopyKey::ActionPause),
        };
    }
    if model.capture == "paused" {
        return IntakeKeys {
            headline: Some(CopyKey::StatusPaused),
            detail: if model.pause_bounded == Some(false) {
                Some(CopyKey::StatusPausedDetail)
            } else {
                None
            },
            action: Some(CopyKey::ActionResume),
        };
    }
    IntakeKeys::default()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalHealthKeys {
    pub status: Option<CopyKey>,
    pub detail: Option<CopyKey>,
    pub action_open: Option<CopyKey>,
}

pub fn map_journal_health(model: &ControlModel) -> JournalHealthKeys {
    let action_open = if model.durable == "confirmed" {
        Some(CopyKey::ActionOpenJournal)
    } else {
        None
    };

    if model.durable == "confirmed" {
        if model.health == "connected" {
            return JournalHealthKeys {
                status: Some(CopyKey::JournalConnected),
                detail: None,
                action_open,
            };
        }
        if model.health == "offline" || model.health == "transport-unavailable" {
            return JournalHealthKeys {
                status: Some(CopyKey::JournalOffline),
                detail: Some(CopyKey::JournalOfflineDetail),
                action_open,
            };
        }
    } else if model.durable == "unpaired" {
        return JournalHealthKeys {
            status: Some(CopyKey::JournalNotPaired),
            detail: None,
            action_open: None,
        };
    }

    JournalHealthKeys {
        status: None,
        detail: None,
        action_open,
    }
}

pub fn map_open_journal_outcome(result: &str) -> Option<CopyKey> {
    if result == "unavailable" {
        Some(CopyKey::BrowserOpenFailed)
    } else {
        None
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairingKeys {
    pub title: Option<CopyKey>,
    pub detail: Option<CopyKey>,
    pub secondary_title: Option<CopyKey>,
    pub secondary_detail: Option<CopyKey>,
    pub confirm_action: Option<CopyKey>,
    pub reject_action: Option<CopyKey>,
    pub cancel_action: Option<CopyKey>,
    pub check_status: Option<CopyKey>,
}

pub fn validate_pairing_link(link: &str) -> Result<String, CopyKey> {
    let trimmed = link.trim();
    if trimmed.is_empty() {
        return Err(CopyKey::PairingEmpty);
    }
    adapt_graphical_link(trimmed).map_err(|_| CopyKey::PairingBadLink)
}

pub fn map_pairing(model: &ControlModel, attempt_outcome: Option<&str>) -> PairingKeys {
    let is_replacement = model.durable == "confirmed"
        || (model.confirmed_mark.is_some() && model.pending_mark.is_some());

    if let Some(outcome) = attempt_outcome {
        match outcome {
            "stale-answer" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingStale),
                    ..Default::default()
                };
            }
            "commit-outcome-unknown" | "outcome-unknown" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingOutcomeUnavailable),
                    check_status: Some(CopyKey::PairingCheckStatus),
                    ..Default::default()
                };
            }
            "owner-start-failed" if model.durable == "confirmed" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingConfirmedUnavailable),
                    ..Default::default()
                };
            }
            "commit-failed" if model.durable != "held" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingSaveFailed),
                    ..Default::default()
                };
            }
            _ => {}
        }
    }

    if model.durable == "held" || model.attempt.as_ref().map(|a| a.state.as_str()) == Some("held") {
        let has_verifiable_mark = model.pending_mark.is_some();
        if has_verifiable_mark {
            return PairingKeys {
                title: Some(CopyKey::PairingHeld),
                detail: if is_replacement {
                    Some(CopyKey::PairingReplacementHeldDetail)
                } else {
                    Some(CopyKey::PairingHeldDetail)
                },
                secondary_title: Some(CopyKey::PairingQuestion),
                secondary_detail: Some(CopyKey::PairingQuestionDetail),
                confirm_action: Some(CopyKey::PairingConfirm),
                reject_action: Some(CopyKey::PairingReject),
                cancel_action: Some(CopyKey::PairingCancel),
                check_status: None,
            };
        } else {
            return PairingKeys {
                title: Some(CopyKey::PairingUnavailableTitle),
                detail: if is_replacement {
                    Some(CopyKey::PairingReplacementUnavailableDetail)
                } else {
                    Some(CopyKey::PairingUnavailableDetail)
                },
                secondary_title: None,
                secondary_detail: None,
                confirm_action: None,
                reject_action: Some(CopyKey::PairingReject),
                cancel_action: Some(CopyKey::PairingCancel),
                check_status: None,
            };
        }
    }

    if let Some(att) = &model.attempt {
        match att.state.as_str() {
            "rejected" => {
                return PairingKeys {
                    title: if is_replacement {
                        Some(CopyKey::PairingReplacementRejectedTitle)
                    } else {
                        Some(CopyKey::PairingRejectedTitle)
                    },
                    detail: if is_replacement {
                        Some(CopyKey::PairingReplacementRejectedDetail)
                    } else {
                        Some(CopyKey::PairingRejectedDetail)
                    },
                    ..Default::default()
                };
            }
            "cancelled" => {
                return PairingKeys {
                    title: if is_replacement {
                        Some(CopyKey::PairingReplacementCancelled)
                    } else {
                        Some(CopyKey::PairingCancelled)
                    },
                    ..Default::default()
                };
            }
            "expired" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingExpired),
                    ..Default::default()
                };
            }
            "failed" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingFailed),
                    ..Default::default()
                };
            }
            _ => {}
        }
    }

    PairingKeys::default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ServicePresenceKind {
    Absent,
    Inactive,
    Failed,
    Active,
    Uncertain,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceStartKeys {
    pub action: Option<CopyKey>,
    pub disclosure: Option<CopyKey>,
    pub status: Option<CopyKey>,
    pub check_status: Option<CopyKey>,
}

pub fn map_service_start(
    presence: ServicePresenceKind,
    in_flight: bool,
    definite_failure: bool,
    deadline_hit: bool,
) -> ServiceStartKeys {
    if in_flight {
        return ServiceStartKeys {
            action: Some(CopyKey::ActionStarting),
            disclosure: None,
            status: Some(CopyKey::StatusStartWaiting),
            check_status: None,
        };
    }
    if definite_failure {
        return ServiceStartKeys {
            action: Some(CopyKey::ActionStart),
            disclosure: None,
            status: Some(CopyKey::StatusStartFailed),
            check_status: None,
        };
    }
    if deadline_hit {
        return ServiceStartKeys {
            action: Some(CopyKey::ActionStart),
            disclosure: None,
            status: Some(CopyKey::StatusStartUnavailable),
            check_status: Some(CopyKey::ActionCheckStatus),
        };
    }

    match presence {
        ServicePresenceKind::Absent => ServiceStartKeys {
            action: Some(CopyKey::ActionStart),
            disclosure: Some(CopyKey::StatusStartDisclosure),
            status: None,
            check_status: None,
        },
        ServicePresenceKind::Inactive | ServicePresenceKind::Failed => ServiceStartKeys {
            action: Some(CopyKey::ActionStart),
            disclosure: None,
            status: None,
            check_status: None,
        },
        ServicePresenceKind::Active | ServicePresenceKind::Uncertain => ServiceStartKeys::default(),
    }
}

pub fn validate_settings_inputs(
    framerate_str: &str,
    segment_str: &str,
) -> Result<(i64, i64), CopyKey> {
    let framerate: i64 = framerate_str
        .trim()
        .parse()
        .map_err(|_| CopyKey::SettingsFrameInvalid)?;
    if !(1..=10).contains(&framerate) {
        return Err(CopyKey::SettingsFrameInvalid);
    }

    let segment: i64 = segment_str
        .trim()
        .parse()
        .map_err(|_| CopyKey::SettingsSegmentInvalid)?;
    if segment <= 0 {
        return Err(CopyKey::SettingsSegmentInvalid);
    }

    Ok((framerate, segment))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsKeys {
    pub load_failed: Option<CopyKey>,
    pub reload: Option<CopyKey>,
    pub save_action: Option<CopyKey>,
    pub saved_banner: Option<CopyKey>,
    pub restart_needed: Option<CopyKey>,
    pub restart_detail: Option<CopyKey>,
    pub restart_action: Option<CopyKey>,
    pub restart_failed: Option<CopyKey>,
}

pub fn map_settings_state(
    saved_available: bool,
    saved_settings: &SettingsModel,
    effective_settings: Option<&SettingsModel>,
    saved_verified: bool,
    latest_owner_matches: bool,
    verified_save_restart_failed: bool,
    recovery_failed_without_save: bool,
) -> SettingsKeys {
    if !saved_available {
        return SettingsKeys {
            load_failed: Some(CopyKey::SettingsLoadFailed),
            reload: Some(CopyKey::SettingsReload),
            save_action: None,
            ..Default::default()
        };
    }

    let saved_banner = if saved_verified {
        Some(CopyKey::SettingsSaved)
    } else {
        None
    };

    let differ = match effective_settings {
        Some(eff) => eff != saved_settings,
        None => false,
    };

    let (restart_needed, restart_detail, restart_action) = if differ {
        (
            Some(CopyKey::SettingsRestartNeeded),
            Some(CopyKey::SettingsRestartDetail),
            if latest_owner_matches {
                Some(CopyKey::SettingsRestart)
            } else {
                None
            },
        )
    } else {
        (None, None, None)
    };

    let restart_failed = if verified_save_restart_failed {
        Some(CopyKey::SettingsRestartFailed)
    } else if recovery_failed_without_save {
        Some(CopyKey::SettingsRestartRecoveryFailed)
    } else {
        None
    };

    SettingsKeys {
        load_failed: None,
        reload: None,
        save_action: Some(CopyKey::SettingsSave),
        saved_banner,
        restart_needed,
        restart_detail,
        restart_action,
        restart_failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_journal_connected_and_offline() {
        let mut model = ControlModel::unknown();
        model.durable = "confirmed".into();
        model.health = "connected".into();
        let k = map_journal_health(&model);
        assert_eq!(k.status, Some(CopyKey::JournalConnected));
        assert_eq!(k.detail, None);
        assert_eq!(k.action_open, Some(CopyKey::ActionOpenJournal));

        model.health = "offline".into();
        let k = map_journal_health(&model);
        assert_eq!(k.status, Some(CopyKey::JournalOffline));
        assert_eq!(k.detail, Some(CopyKey::JournalOfflineDetail));

        model.health = "transport-unavailable".into();
        let k = map_journal_health(&model);
        assert_eq!(k.status, Some(CopyKey::JournalOffline));
        assert_eq!(k.detail, Some(CopyKey::JournalOfflineDetail));

        for h in ["listener-ready", "connecting", "syncing", "not-reported"] {
            model.health = h.into();
            let k = map_journal_health(&model);
            assert_ne!(k.status, Some(CopyKey::JournalOffline));
        }

        let mut unconf = ControlModel::unknown();
        unconf.durable = "unpaired".into();
        unconf.health = "offline".into();
        let k = map_journal_health(&unconf);
        assert_eq!(k.status, Some(CopyKey::JournalNotPaired));
        assert_eq!(k.action_open, None);
    }

    #[test]
    fn test_pairing_outcomes() {
        let mut model = ControlModel::unknown();
        model.durable = "confirmed".into();
        let k = map_pairing(&model, Some("owner-start-failed"));
        assert_eq!(k.title, Some(CopyKey::PairingConfirmedUnavailable));

        let mut held_model = ControlModel::unknown();
        held_model.durable = "held".into();
        let k = map_pairing(&held_model, Some("commit-failed"));
        assert_ne!(k.title, Some(CopyKey::PairingSaveFailed));

        let mut unp_model = ControlModel::unknown();
        unp_model.durable = "unpaired".into();
        let k = map_pairing(&unp_model, Some("commit-failed"));
        assert_eq!(k.title, Some(CopyKey::PairingSaveFailed));

        for unk in ["commit-outcome-unknown", "outcome-unknown"] {
            let k = map_pairing(&unp_model, Some(unk));
            assert_eq!(k.title, Some(CopyKey::PairingOutcomeUnavailable));
            assert_eq!(k.check_status, Some(CopyKey::PairingCheckStatus));
        }

        let k = map_pairing(&unp_model, Some("stale-answer"));
        assert_eq!(k.title, Some(CopyKey::PairingStale));
    }

    #[test]
    fn test_intake_attention_and_states() {
        let mut model = ControlModel::unknown();
        model.capture_attention = true;
        let k = map_intake(&model);
        assert_eq!(k.headline, Some(CopyKey::StatusAttention));

        let mut doc = ControlModel::unknown();
        doc.durable = "confirmed".into();
        doc.fault = Some("owner-stop-failed".into());
        let k = map_intake(&doc);
        assert_eq!(k.headline, Some(CopyKey::StatusAttention));

        let mut rec = ControlModel::unknown();
        rec.capture = "recording".into();
        let k = map_intake(&rec);
        assert_eq!(k.headline, Some(CopyKey::StatusOn));
        assert_eq!(k.detail, Some(CopyKey::StatusOnDetail));
        assert_eq!(k.action, Some(CopyKey::ActionPause));

        let mut pau = ControlModel::unknown();
        pau.capture = "paused".into();
        pau.pause_bounded = Some(false);
        let k = map_intake(&pau);
        assert_eq!(k.headline, Some(CopyKey::StatusPaused));
        assert_eq!(k.detail, Some(CopyKey::StatusPausedDetail));
        assert_eq!(k.action, Some(CopyKey::ActionResume));

        pau.pause_bounded = Some(true);
        let k = map_intake(&pau);
        assert_eq!(k.headline, Some(CopyKey::StatusPaused));
        assert_eq!(k.detail, None);
        assert_eq!(k.action, Some(CopyKey::ActionResume));
    }

    #[test]
    fn test_pairing_link_validation() {
        assert_eq!(validate_pairing_link(""), Err(CopyKey::PairingEmpty));
        assert_eq!(validate_pairing_link("   "), Err(CopyKey::PairingEmpty));
        assert_eq!(
            validate_pairing_link("invalid-link"),
            Err(CopyKey::PairingBadLink)
        );
        assert!(validate_pairing_link("https://go.solstone.app/p#PAIR123").is_ok());
    }

    #[test]
    fn test_service_start_mapping() {
        let absent = map_service_start(ServicePresenceKind::Absent, false, false, false);
        assert_eq!(absent.action, Some(CopyKey::ActionStart));
        assert_eq!(absent.disclosure, Some(CopyKey::StatusStartDisclosure));

        let inactive = map_service_start(ServicePresenceKind::Inactive, false, false, false);
        assert_eq!(inactive.action, Some(CopyKey::ActionStart));
        assert_eq!(inactive.disclosure, None);

        let active = map_service_start(ServicePresenceKind::Active, false, false, false);
        assert_eq!(active.action, None);

        let in_flight = map_service_start(ServicePresenceKind::Inactive, true, false, false);
        assert_eq!(in_flight.action, Some(CopyKey::ActionStarting));
        assert_eq!(in_flight.status, Some(CopyKey::StatusStartWaiting));

        let d_failure = map_service_start(ServicePresenceKind::Inactive, false, true, false);
        assert_eq!(d_failure.status, Some(CopyKey::StatusStartFailed));

        let deadline = map_service_start(ServicePresenceKind::Inactive, false, false, true);
        assert_eq!(deadline.status, Some(CopyKey::StatusStartUnavailable));
        assert_eq!(deadline.check_status, Some(CopyKey::ActionCheckStatus));
    }

    #[test]
    fn test_settings_mapping() {
        let saved = SettingsModel {
            capture_framerate: 5,
            draw_cursor: true,
            start_paused: false,
            segment_interval: 10,
        };
        let keys_unavailable = map_settings_state(false, &saved, None, false, true, false, false);
        assert_eq!(
            keys_unavailable.load_failed,
            Some(CopyKey::SettingsLoadFailed)
        );
        assert_eq!(keys_unavailable.reload, Some(CopyKey::SettingsReload));
        assert_eq!(keys_unavailable.save_action, None);

        let eff_same = saved.clone();
        let keys_same = map_settings_state(true, &saved, Some(&eff_same), true, true, false, false);
        assert_eq!(keys_same.saved_banner, Some(CopyKey::SettingsSaved));
        assert_eq!(keys_same.restart_needed, None);

        let eff_diff = SettingsModel {
            capture_framerate: 6,
            ..saved.clone()
        };
        let keys_diff =
            map_settings_state(true, &saved, Some(&eff_diff), false, true, false, false);
        assert_eq!(
            keys_diff.restart_needed,
            Some(CopyKey::SettingsRestartNeeded)
        );
        assert_eq!(keys_diff.restart_action, Some(CopyKey::SettingsRestart));

        assert_eq!(
            validate_settings_inputs("0", "10"),
            Err(CopyKey::SettingsFrameInvalid)
        );
        assert_eq!(
            validate_settings_inputs("11", "10"),
            Err(CopyKey::SettingsFrameInvalid)
        );
        assert_eq!(
            validate_settings_inputs("5", "0"),
            Err(CopyKey::SettingsSegmentInvalid)
        );
        assert_eq!(
            validate_settings_inputs("5", "-1"),
            Err(CopyKey::SettingsSegmentInvalid)
        );
        assert!(validate_settings_inputs("5", "10").is_ok());
    }
}
