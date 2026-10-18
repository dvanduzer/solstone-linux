// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use gtk::prelude::*;
use solstone_linux::copy::CopyKey;
use solstone_linux::dbus_client::ControlClient;
use solstone_linux::window_control::SettingsModel;

use crate::controller::{Controller, IntakeCommand, PairingCommand};
use crate::copy_map::{
    ServicePresenceKind, map_intake, map_journal_health, map_open_journal_outcome, map_pairing,
    map_service_start, map_settings_state, validate_pairing_link, validate_settings_inputs,
};
use crate::format::{format_integer, format_timestamp};
use crate::logs::{MAX_PREVIEW_BYTES, process_log_content, save_frozen_log};

pub fn build_ui(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(CopyKey::WindowTitle.as_str())
        .default_width(600)
        .default_height(620)
        .build();

    window.set_size_request(460, 520);

    let notebook = gtk::Notebook::new();

    let controller = Rc::new(RefCell::new(Controller::new()));

    // Page 1: Status
    let (status_page, update_status) = create_status_page(Rc::clone(&controller));
    notebook.append_page(
        &status_page,
        Some(&gtk::Label::new(Some(CopyKey::NavStatus.as_str()))),
    );

    // Page 2: Journal
    let (journal_page, update_journal) = create_journal_page(Rc::clone(&controller));
    notebook.append_page(
        &journal_page,
        Some(&gtk::Label::new(Some(CopyKey::NavJournal.as_str()))),
    );

    // Page 3: Settings
    let (settings_page, update_settings) = create_settings_page(Rc::clone(&controller));
    notebook.append_page(
        &settings_page,
        Some(&gtk::Label::new(Some(CopyKey::NavSettings.as_str()))),
    );

    // Page 4: Help
    let help_page = create_help_page(Rc::clone(&controller), &window);
    notebook.append_page(
        &help_page,
        Some(&gtk::Label::new(Some(CopyKey::NavHelp.as_str()))),
    );

    window.set_child(Some(&notebook));
    window.present();

    let update_all: Rc<dyn Fn()> = {
        let update_status = update_status.clone();
        let update_journal = update_journal.clone();
        let update_settings = update_settings.clone();
        Rc::new(move || {
            update_status();
            update_journal();
            update_settings();
        })
    };

    // Kick off async poll loop
    spawn_poll_loop(Rc::clone(&controller), Rc::clone(&update_all));
}

fn spawn_poll_loop(controller: Rc<RefCell<Controller>>, update_all: Rc<dyn Fn()>) {
    let refresh = {
        let controller = Rc::clone(&controller);
        let update_all = Rc::clone(&update_all);
        move || {
            let ctrl = Rc::clone(&controller);
            let upd = Rc::clone(&update_all);
            glib::MainContext::default().spawn_local(async move {
                let client_res = ControlClient::session().await;
                match client_res {
                    Ok(client) => {
                        let owner_res = client.get_owner().await;
                        match owner_res {
                            Ok(owner) => {
                                ctrl.borrow_mut().set_bus_owner(Some(owner.clone()));
                                let id = ctrl.borrow_mut().alloc_id();
                                let state_res = client.get_control_state().await;
                                ctrl.borrow_mut().apply_status_reply(id, &owner, state_res);
                            }
                            Err(_) => {
                                ctrl.borrow_mut().set_bus_owner(None);
                            }
                        }
                    }
                    Err(_) => {
                        ctrl.borrow_mut().set_bus_owner(None);
                    }
                }
                upd();
            });
            glib::ControlFlow::Continue
        }
    };

    // Run initial refresh immediately
    refresh();

    // Schedule every 2 seconds
    glib::timeout_add_local(Duration::from_secs(2), refresh);
}

fn create_status_page(controller: Rc<RefCell<Controller>>) -> (gtk::ScrolledWindow, Rc<dyn Fn()>) {
    let scrolled = gtk::ScrolledWindow::new();
    let container = gtk::Box::new(gtk::Orientation::Vertical, 16);
    container.set_margin_top(24);
    container.set_margin_bottom(24);
    container.set_margin_start(24);
    container.set_margin_end(24);

    let headline_label = gtk::Label::new(None);
    headline_label.set_halign(gtk::Align::Start);
    let detail_label = gtk::Label::new(None);
    detail_label.set_halign(gtk::Align::Start);
    detail_label.set_wrap(true);

    let action_button = gtk::Button::new();
    action_button.set_halign(gtk::Align::Start);
    action_button.set_visible(false);

    let start_button = gtk::Button::new();
    start_button.set_halign(gtk::Align::Start);
    start_button.set_visible(false);

    let start_disclosure = gtk::Label::new(None);
    start_disclosure.set_halign(gtk::Align::Start);
    start_disclosure.set_wrap(true);
    start_disclosure.set_visible(false);

    let stats_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let queue_label = gtk::Label::new(None);
    queue_label.set_halign(gtk::Align::Start);
    let quarantine_label = gtk::Label::new(None);
    quarantine_label.set_halign(gtk::Align::Start);
    let delivery_label = gtk::Label::new(None);
    delivery_label.set_halign(gtk::Align::Start);
    let version_label = gtk::Label::new(None);
    version_label.set_halign(gtk::Align::Start);
    version_label.set_visible(false);

    stats_box.append(&queue_label);
    stats_box.append(&quarantine_label);
    stats_box.append(&delivery_label);
    stats_box.append(&version_label);

    let expander = gtk::Expander::new(Some(CopyKey::StatusDetails.as_str()));
    let fault_label = gtk::Label::new(None);
    fault_label.set_wrap(true);
    fault_label.set_halign(gtk::Align::Start);
    expander.set_child(Some(&fault_label));

    container.append(&headline_label);
    container.append(&detail_label);
    container.append(&action_button);
    container.append(&start_button);
    container.append(&start_disclosure);
    container.append(&stats_box);
    container.append(&expander);
    scrolled.set_child(Some(&container));

    {
        let ctrl = Rc::clone(&controller);
        action_button.connect_clicked(move |_| {
            let (is_pause, owner) = {
                let c = ctrl.borrow();
                let is_p = c.document.capture == "recording" || c.document.capture == "on";
                let o = c.bus_owner.clone();
                (is_p, o)
            };
            if let Some(owner) = owner {
                let cmd = if is_pause {
                    IntakeCommand::Pause(0)
                } else {
                    IntakeCommand::Resume
                };
                let id = ctrl.borrow_mut().start_intake_command(&owner, cmd);
                let c_inner = Rc::clone(&ctrl);
                glib::MainContext::default().spawn_local(async move {
                    if let Ok(client) = ControlClient::session().await {
                        let res = if is_pause {
                            client.pause(0).await
                        } else {
                            client.resume().await
                        };
                        c_inner.borrow_mut().apply_intake_reply(id, &owner, res);
                    }
                });
            }
        });
    }

    let update = {
        let ctrl = Rc::clone(&controller);
        Rc::new(move || {
            let c = ctrl.borrow();
            let intake_keys = map_intake(&c.document);

            if let Some(k) = intake_keys.headline {
                headline_label.set_text(k.as_str());
                headline_label.set_visible(true);
            } else {
                headline_label.set_visible(false);
            }

            if let Some(k) = intake_keys.detail {
                detail_label.set_text(k.as_str());
                detail_label.set_visible(true);
            } else {
                detail_label.set_visible(false);
            }

            if let Some(k) = intake_keys.action {
                action_button.set_label(k.as_str());
                action_button.set_visible(true);
            } else {
                action_button.set_visible(false);
            }

            let start_keys = map_service_start(
                ServicePresenceKind::Active,
                c.start.is_some(),
                c.start_error.is_some(),
                false,
            );
            if let Some(k) = start_keys.action {
                start_button.set_label(k.as_str());
                start_button.set_visible(true);
            } else {
                start_button.set_visible(false);
            }

            if let Some(k) = start_keys.disclosure {
                start_disclosure.set_text(k.as_str());
                start_disclosure.set_visible(true);
            } else {
                start_disclosure.set_visible(false);
            }

            if let Some(q) = c.document.queue_count {
                queue_label.set_text(&format!(
                    "{}: {}",
                    CopyKey::StatusWaitingLabel.as_str(),
                    format_integer(q)
                ));
                queue_label.set_visible(true);
            } else {
                queue_label.set_visible(false);
            }

            if let Some(q) = c.document.quarantine_count {
                if q > 0 {
                    let plural = solstone_linux::copy::attention_count(q as usize);
                    quarantine_label.set_text(&format!("{} {}", format_integer(q), plural));
                    quarantine_label.set_visible(true);
                } else {
                    quarantine_label.set_visible(false);
                }
            } else {
                quarantine_label.set_visible(false);
            }

            if let Some(d) = &c.document.last_delivery {
                let at_str = match d.at {
                    Some(at) => format_timestamp(at),
                    None => solstone_linux::copy::NOT_YET.to_string(),
                };
                delivery_label.set_text(&format!(
                    "{}: {}",
                    CopyKey::StatusLastDelivery.as_str(),
                    at_str
                ));
                delivery_label.set_visible(true);
            } else {
                delivery_label.set_visible(false);
            }

            if let Some(v) = &c.document.daemon_version {
                version_label.set_text(&format!("{}: {}", CopyKey::StatusVersion.as_str(), v));
                version_label.set_visible(true);
            } else {
                version_label.set_visible(false);
            }

            if let Some(f) = &c.document.fault {
                fault_label.set_text(f);
                expander.set_visible(true);
            } else {
                fault_label.set_text("");
                expander.set_visible(false);
            }
        })
    };

    (scrolled, update)
}

fn create_journal_page(controller: Rc<RefCell<Controller>>) -> (gtk::ScrolledWindow, Rc<dyn Fn()>) {
    let scrolled = gtk::ScrolledWindow::new();
    let container = gtk::Box::new(gtk::Orientation::Vertical, 16);
    container.set_margin_top(24);
    container.set_margin_bottom(24);
    container.set_margin_start(24);
    container.set_margin_end(24);

    let status_label = gtk::Label::new(None);
    status_label.set_halign(gtk::Align::Start);

    let detail_label = gtk::Label::new(None);
    detail_label.set_halign(gtk::Align::Start);
    detail_label.set_wrap(true);

    let open_button = gtk::Button::with_label(CopyKey::ActionOpenJournal.as_str());
    open_button.set_halign(gtk::Align::Start);
    open_button.set_visible(false);

    let mark_container = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let pairing_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let link_entry = gtk::Entry::new();
    link_entry.set_placeholder_text(Some(CopyKey::JournalLinkPlaceholder.as_str()));
    let pair_button = gtk::Button::with_label(CopyKey::ActionPair.as_str());
    pair_button.set_halign(gtk::Align::Start);

    let pairing_notice = gtk::Label::new(None);
    pairing_notice.set_halign(gtk::Align::Start);
    pairing_notice.set_wrap(true);
    pairing_notice.set_visible(false);

    let held_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let confirm_btn = gtk::Button::with_label(CopyKey::PairingConfirm.as_str());
    let reject_btn = gtk::Button::with_label(CopyKey::PairingReject.as_str());
    let cancel_btn = gtk::Button::with_label(CopyKey::PairingCancel.as_str());
    held_box.append(&confirm_btn);
    held_box.append(&reject_btn);
    held_box.append(&cancel_btn);
    held_box.set_visible(false);

    pairing_box.append(&link_entry);
    pairing_box.append(&pair_button);
    pairing_box.append(&pairing_notice);
    pairing_box.append(&held_box);

    container.append(&status_label);
    container.append(&detail_label);
    container.append(&open_button);
    container.append(&mark_container);
    container.append(&pairing_box);
    scrolled.set_child(Some(&container));

    {
        let ctrl = Rc::clone(&controller);
        open_button.connect_clicked(move |_| {
            let c_inner = Rc::clone(&ctrl);
            glib::MainContext::default().spawn_local(async move {
                if let Ok(client) = ControlClient::session().await {
                    let res = client.open_journal().await;
                    if let Ok(val) = res
                        && let Some(err_key) = map_open_journal_outcome(&val)
                    {
                        c_inner.borrow_mut().pairing_error = Some(err_key.as_str().into());
                    }
                }
            });
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let link_entry = link_entry.clone();
        let pairing_notice = pairing_notice.clone();
        pair_button.connect_clicked(move |_| {
            let text = link_entry.text();
            match validate_pairing_link(&text) {
                Ok(valid_link) => {
                    pairing_notice.set_visible(false);
                    let owner = ctrl.borrow().bus_owner.clone();
                    if let Some(owner) = owner {
                        let id = ctrl
                            .borrow_mut()
                            .start_pairing(&owner, PairingCommand::Begin(valid_link.clone()));
                        let c_inner = Rc::clone(&ctrl);
                        glib::MainContext::default().spawn_local(async move {
                            if let Ok(client) = ControlClient::session().await {
                                let res = client.begin_pairing(&valid_link).await;
                                c_inner.borrow_mut().apply_pairing_reply(id, &owner, res);
                            }
                        });
                    }
                }
                Err(err_key) => {
                    pairing_notice.set_text(err_key.as_str());
                    pairing_notice.set_visible(true);
                }
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        confirm_btn.connect_clicked(move |_| {
            let (att_id, pair_id, owner) = {
                let c = ctrl.borrow();
                let att = c.document.attempt.clone();
                let o = c.bus_owner.clone();
                (
                    att.as_ref().map(|a| a.id.clone()),
                    att.as_ref().map(|a| a.pairing_id.clone()),
                    o,
                )
            };
            if let (Some(att_id), Some(pair_id), Some(owner)) = (att_id, pair_id, owner) {
                let cmd = PairingCommand::Answer {
                    attempt_id: att_id.clone(),
                    pairing_id: pair_id.clone(),
                    accept: true,
                };
                let id = ctrl.borrow_mut().start_pairing(&owner, cmd);
                let c_inner = Rc::clone(&ctrl);
                glib::MainContext::default().spawn_local(async move {
                    if let Ok(client) = ControlClient::session().await {
                        let res = client.answer_pairing(&att_id, &pair_id, true).await;
                        c_inner.borrow_mut().apply_pairing_reply(id, &owner, res);
                    }
                });
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        reject_btn.connect_clicked(move |_| {
            let (att_id, pair_id, owner) = {
                let c = ctrl.borrow();
                let att = c.document.attempt.clone();
                let o = c.bus_owner.clone();
                (
                    att.as_ref().map(|a| a.id.clone()),
                    att.as_ref().map(|a| a.pairing_id.clone()),
                    o,
                )
            };
            if let (Some(att_id), Some(pair_id), Some(owner)) = (att_id, pair_id, owner) {
                let cmd = PairingCommand::Answer {
                    attempt_id: att_id.clone(),
                    pairing_id: pair_id.clone(),
                    accept: false,
                };
                let id = ctrl.borrow_mut().start_pairing(&owner, cmd);
                let c_inner = Rc::clone(&ctrl);
                glib::MainContext::default().spawn_local(async move {
                    if let Ok(client) = ControlClient::session().await {
                        let res = client.answer_pairing(&att_id, &pair_id, false).await;
                        c_inner.borrow_mut().apply_pairing_reply(id, &owner, res);
                    }
                });
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        cancel_btn.connect_clicked(move |_| {
            let (att_id, owner) = {
                let c = ctrl.borrow();
                let att = c.document.attempt.clone();
                let o = c.bus_owner.clone();
                (att.as_ref().map(|a| a.id.clone()), o)
            };
            if let (Some(att_id), Some(owner)) = (att_id, owner) {
                let cmd = PairingCommand::Cancel(att_id.clone());
                let id = ctrl.borrow_mut().start_pairing(&owner, cmd);
                let c_inner = Rc::clone(&ctrl);
                glib::MainContext::default().spawn_local(async move {
                    if let Ok(client) = ControlClient::session().await {
                        let res = client.cancel_pairing(&att_id).await;
                        c_inner.borrow_mut().apply_pairing_reply(id, &owner, res);
                    }
                });
            }
        });
    }

    let update = {
        let ctrl = Rc::clone(&controller);
        Rc::new(move || {
            let c = ctrl.borrow();
            let journal_keys = map_journal_health(&c.document);

            if let Some(k) = journal_keys.status {
                status_label.set_text(k.as_str());
                status_label.set_visible(true);
            } else {
                status_label.set_visible(false);
            }

            if let Some(k) = journal_keys.detail {
                detail_label.set_text(k.as_str());
                detail_label.set_visible(true);
            } else {
                detail_label.set_visible(false);
            }

            open_button.set_visible(journal_keys.action_open.is_some());

            // Mark chip rendering
            let mark = c
                .document
                .pending_mark
                .clone()
                .or_else(|| c.document.confirmed_mark.clone());
            while let Some(child) = mark_container.first_child() {
                mark_container.remove(&child);
            }
            if let Some(m) = mark {
                let mark_widget = crate::mark_card::create_mark_card_widget(m);
                mark_container.append(&mark_widget);
                mark_container.set_visible(true);
            } else {
                mark_container.set_visible(false);
            }

            let pairing_keys = map_pairing(&c.document, c.pairing_attempt_outcome.as_deref());
            if let Some(k) = pairing_keys.title {
                pairing_notice.set_text(k.as_str());
                pairing_notice.set_visible(true);
            }

            held_box.set_visible(
                pairing_keys.confirm_action.is_some() || pairing_keys.reject_action.is_some(),
            );
            confirm_btn.set_visible(pairing_keys.confirm_action.is_some());
            reject_btn.set_visible(pairing_keys.reject_action.is_some());
            cancel_btn.set_visible(pairing_keys.cancel_action.is_some());
        })
    };

    (scrolled, update)
}

fn create_settings_page(
    controller: Rc<RefCell<Controller>>,
) -> (gtk::ScrolledWindow, Rc<dyn Fn()>) {
    let scrolled = gtk::ScrolledWindow::new();
    let container = gtk::Box::new(gtk::Orientation::Vertical, 16);
    container.set_margin_top(24);
    container.set_margin_bottom(24);
    container.set_margin_start(24);
    container.set_margin_end(24);

    let load_failed_label = gtk::Label::new(Some(CopyKey::SettingsLoadFailed.as_str()));
    load_failed_label.set_halign(gtk::Align::Start);
    let reload_button = gtk::Button::with_label(CopyKey::SettingsReload.as_str());
    reload_button.set_halign(gtk::Align::Start);

    let form_box = gtk::Box::new(gtk::Orientation::Vertical, 12);

    let framerate_entry = gtk::Entry::new();
    let framerate_help = gtk::Label::new(Some(CopyKey::SettingsFrameRateHelp.as_str()));
    framerate_help.set_halign(gtk::Align::Start);

    let cursor_switch = gtk::Switch::new();
    cursor_switch.set_halign(gtk::Align::Start);
    let cursor_label = gtk::Label::new(Some(CopyKey::SettingsCursor.as_str()));
    cursor_label.set_halign(gtk::Align::Start);

    let start_paused_switch = gtk::Switch::new();
    start_paused_switch.set_halign(gtk::Align::Start);
    let start_paused_label = gtk::Label::new(Some(CopyKey::SettingsStartPaused.as_str()));
    start_paused_label.set_halign(gtk::Align::Start);

    let segment_entry = gtk::Entry::new();
    let segment_help = gtk::Label::new(Some(CopyKey::SettingsSegmentHelp.as_str()));
    segment_help.set_halign(gtk::Align::Start);

    let save_button = gtk::Button::with_label(CopyKey::SettingsSave.as_str());
    save_button.set_halign(gtk::Align::Start);

    let saved_banner = gtk::Label::new(Some(CopyKey::SettingsSaved.as_str()));
    saved_banner.set_halign(gtk::Align::Start);
    saved_banner.set_visible(false);

    let restart_needed_label = gtk::Label::new(Some(CopyKey::SettingsRestartNeeded.as_str()));
    restart_needed_label.set_halign(gtk::Align::Start);
    restart_needed_label.set_visible(false);

    let restart_detail_label = gtk::Label::new(Some(CopyKey::SettingsRestartDetail.as_str()));
    restart_detail_label.set_halign(gtk::Align::Start);
    restart_detail_label.set_wrap(true);
    restart_detail_label.set_visible(false);

    let restart_button = gtk::Button::with_label(CopyKey::SettingsRestart.as_str());
    restart_button.set_halign(gtk::Align::Start);
    restart_button.set_visible(false);

    let error_label = gtk::Label::new(None);
    error_label.set_halign(gtk::Align::Start);
    error_label.set_visible(false);

    form_box.append(&gtk::Label::new(Some(CopyKey::SettingsFrameRate.as_str())));
    form_box.append(&framerate_entry);
    form_box.append(&framerate_help);
    form_box.append(&cursor_label);
    form_box.append(&cursor_switch);
    form_box.append(&start_paused_label);
    form_box.append(&start_paused_switch);
    form_box.append(&gtk::Label::new(Some(CopyKey::SettingsSegment.as_str())));
    form_box.append(&segment_entry);
    form_box.append(&segment_help);
    form_box.append(&save_button);
    form_box.append(&saved_banner);
    form_box.append(&restart_needed_label);
    form_box.append(&restart_detail_label);
    form_box.append(&restart_button);
    form_box.append(&error_label);

    container.append(&load_failed_label);
    container.append(&reload_button);
    container.append(&form_box);
    scrolled.set_child(Some(&container));

    {
        let ctrl = Rc::clone(&controller);
        framerate_entry.connect_changed(move |e| {
            if let Ok(v) = e.text().trim().parse::<i64>() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.framerate = Some(v);
                c.settings_draft.framerate_dirty =
                    c.document.saved_available && v != c.document.settings.capture_framerate;
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        segment_entry.connect_changed(move |e| {
            if let Ok(v) = e.text().trim().parse::<i64>() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.segment = Some(v);
                c.settings_draft.segment_dirty =
                    c.document.saved_available && v != c.document.settings.segment_interval;
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        cursor_switch.connect_state_set(move |_, state| {
            let mut c = ctrl.borrow_mut();
            c.settings_draft.cursor = Some(state);
            c.settings_draft.cursor_dirty =
                c.document.saved_available && state != c.document.settings.draw_cursor;
            glib::Propagation::Proceed
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        start_paused_switch.connect_state_set(move |_, state| {
            let mut c = ctrl.borrow_mut();
            c.settings_draft.start_paused = Some(state);
            c.settings_draft.start_paused_dirty =
                c.document.saved_available && state != c.document.settings.start_paused;
            glib::Propagation::Proceed
        });
    }

    let update: Rc<dyn Fn()> = {
        let ctrl = Rc::clone(&controller);
        let load_failed_label = load_failed_label.clone();
        let reload_button = reload_button.clone();
        let form_box = form_box.clone();
        let framerate_entry = framerate_entry.clone();
        let cursor_switch = cursor_switch.clone();
        let start_paused_switch = start_paused_switch.clone();
        let segment_entry = segment_entry.clone();
        let saved_banner = saved_banner.clone();
        let restart_needed_label = restart_needed_label.clone();
        let restart_detail_label = restart_detail_label.clone();
        let restart_button = restart_button.clone();
        let error_label = error_label.clone();
        Rc::new(move || {
            let (
                keys,
                framerate_dirty,
                cursor_dirty,
                start_paused_dirty,
                segment_dirty,
                saved_framerate,
                saved_cursor,
                saved_start_paused,
                saved_segment,
                write_failed,
            ) = {
                let c = ctrl.borrow();
                let keys = map_settings_state(
                    c.document.saved_available,
                    &c.document.settings,
                    c.document.effective.as_ref(),
                    c.settings_draft.saved_verified,
                    c.bus_owner.is_some(),
                    false,
                    false,
                );
                (
                    keys,
                    c.settings_draft.framerate_dirty,
                    c.settings_draft.cursor_dirty,
                    c.settings_draft.start_paused_dirty,
                    c.settings_draft.segment_dirty,
                    c.document.settings.capture_framerate,
                    c.document.settings.draw_cursor,
                    c.document.settings.start_paused,
                    c.document.settings.segment_interval,
                    c.settings_draft.write_failed,
                )
            };

            if keys.load_failed.is_some() {
                load_failed_label.set_visible(true);
                reload_button.set_visible(true);
                form_box.set_visible(false);
            } else {
                load_failed_label.set_visible(false);
                reload_button.set_visible(false);
                form_box.set_visible(true);

                // GTK must not replace a dirty or focused settings buffer
                if !framerate_dirty && !framerate_entry.has_focus() {
                    framerate_entry.set_text(&saved_framerate.to_string());
                }
                if !cursor_dirty && !cursor_switch.has_focus() {
                    cursor_switch.set_state(saved_cursor);
                }
                if !start_paused_dirty && !start_paused_switch.has_focus() {
                    start_paused_switch.set_state(saved_start_paused);
                }
                if !segment_dirty && !segment_entry.has_focus() {
                    segment_entry.set_text(&saved_segment.to_string());
                }

                saved_banner.set_visible(keys.saved_banner.is_some());
                restart_needed_label.set_visible(keys.restart_needed.is_some());
                restart_detail_label.set_visible(keys.restart_detail.is_some());
                restart_button.set_visible(keys.restart_action.is_some());

                if write_failed {
                    error_label.set_text(CopyKey::SettingsSaveFailed.as_str());
                    error_label.set_visible(true);
                }
            }
        })
    };

    {
        let ctrl = Rc::clone(&controller);
        let framerate_entry = framerate_entry.clone();
        let segment_entry = segment_entry.clone();
        let cursor_switch = cursor_switch.clone();
        let start_paused_switch = start_paused_switch.clone();
        let error_label = error_label.clone();
        let update_fn = Rc::clone(&update);
        save_button.connect_clicked(move |_| {
            let res = validate_settings_inputs(&framerate_entry.text(), &segment_entry.text());
            match res {
                Ok((fr, seg)) => {
                    error_label.set_visible(false);
                    let writes = ctrl.borrow().settings_draft.changed_writes();
                    if writes.is_empty() {
                        return;
                    }
                    let settings = SettingsModel {
                        capture_framerate: fr,
                        draw_cursor: cursor_switch.state(),
                        start_paused: start_paused_switch.state(),
                        segment_interval: seg,
                    };
                    let owner = ctrl.borrow().bus_owner.clone();
                    if let Some(owner) = owner {
                        let id = ctrl.borrow_mut().start_settings_save(&owner, settings);
                        let c_inner = Rc::clone(&ctrl);
                        let upd = Rc::clone(&update_fn);
                        glib::MainContext::default().spawn_local(async move {
                            match ControlClient::session().await {
                                Ok(client) => {
                                    let mut write_res = Ok("ok".to_string());
                                    for (field, value) in writes {
                                        match client.write_setting(field, &value).await {
                                            Ok(res) => {
                                                write_res = Ok(res);
                                            }
                                            Err(e) => {
                                                write_res = Err(e);
                                                break;
                                            }
                                        }
                                    }
                                    c_inner
                                        .borrow_mut()
                                        .apply_settings_write_reply(id, &owner, write_res);
                                    upd();
                                }
                                Err(e) => {
                                    c_inner.borrow_mut().apply_settings_write_reply(
                                        id,
                                        &owner,
                                        Err(e),
                                    );
                                    upd();
                                }
                            }
                        });
                    }
                }
                Err(err_key) => {
                    error_label.set_text(err_key.as_str());
                    error_label.set_visible(true);
                }
            }
        });
    }

    (scrolled, update)
}

fn create_help_page(
    controller: Rc<RefCell<Controller>>,
    parent_window: &gtk::ApplicationWindow,
) -> gtk::ScrolledWindow {
    let scrolled = gtk::ScrolledWindow::new();
    let container = gtk::Box::new(gtk::Orientation::Vertical, 16);
    container.set_margin_top(24);
    container.set_margin_bottom(24);
    container.set_margin_start(24);
    container.set_margin_end(24);

    let heading = gtk::Label::new(Some(CopyKey::HelpHeading.as_str()));
    heading.set_halign(gtk::Align::Start);
    let intro = gtk::Label::new(Some(CopyKey::HelpIntro.as_str()));
    intro.set_halign(gtk::Align::Start);
    intro.set_wrap(true);

    let open_button = gtk::Button::with_label(CopyKey::HelpOpen.as_str());
    open_button.set_halign(gtk::Align::Start);
    open_button.connect_clicked(|_| {
        let _ = gio::AppInfo::launch_default_for_uri(
            solstone_linux::support::HELP_URL,
            None::<&gio::AppLaunchContext>,
        );
    });

    let email_button = gtk::Button::with_label(CopyKey::HelpEmail.as_str());
    email_button.set_halign(gtk::Align::Start);
    email_button.connect_clicked(|_| {
        let _ = gio::AppInfo::launch_default_for_uri(
            "mailto:support@solstone.app",
            None::<&gio::AppLaunchContext>,
        );
    });

    let logs_title = gtk::Label::new(Some(CopyKey::LogsTitle.as_str()));
    logs_title.set_halign(gtk::Align::Start);
    let logs_intro = gtk::Label::new(Some(CopyKey::LogsIntro.as_str()));
    logs_intro.set_halign(gtk::Align::Start);
    logs_intro.set_wrap(true);

    let read_logs_btn = gtk::Button::with_label(CopyKey::LogsRead.as_str());
    read_logs_btn.set_halign(gtk::Align::Start);

    let logs_view = gtk::TextView::new();
    logs_view.set_editable(false);
    logs_view.set_wrap_mode(gtk::WrapMode::WordChar);
    let logs_scrolled = gtk::ScrolledWindow::new();
    logs_scrolled.set_min_content_height(140);
    logs_scrolled.set_child(Some(&logs_view));

    let save_logs_btn = gtk::Button::with_label(CopyKey::LogsSave.as_str());
    save_logs_btn.set_halign(gtk::Align::Start);

    let save_help = gtk::Label::new(Some(CopyKey::LogsSaveHelp.as_str()));
    save_help.set_halign(gtk::Align::Start);

    container.append(&heading);
    container.append(&intro);
    container.append(&open_button);
    container.append(&email_button);
    container.append(&logs_title);
    container.append(&logs_intro);
    container.append(&read_logs_btn);
    container.append(&logs_scrolled);
    container.append(&save_logs_btn);
    container.append(&save_help);
    scrolled.set_child(Some(&container));

    {
        let ctrl = Rc::clone(&controller);
        let logs_view = logs_view.clone();
        read_logs_btn.connect_clicked(move |_| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let output = std::process::Command::new("journalctl")
                    .args(["--user", "-u", "solstone-linux.service", "--no-pager"])
                    .output();
                let raw_res = match output {
                    Ok(out) => Ok(Some(String::from_utf8_lossy(&out.stdout).to_string())),
                    Err(e) => Err(e.kind()),
                };
                let frozen = process_log_content(raw_res, MAX_PREVIEW_BYTES);
                let _ = tx.send(frozen);
            });
            let ctrl_inner = Rc::clone(&ctrl);
            let logs_view_inner = logs_view.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || {
                if let Ok(frozen) = rx.try_recv() {
                    let buffer = logs_view_inner.buffer();
                    buffer.set_text(&frozen.content);
                    ctrl_inner.borrow_mut().logs.frozen = Some(frozen);
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let parent = parent_window.clone();
        save_logs_btn.connect_clicked(move |_| {
            let frozen = ctrl.borrow().logs.frozen.clone();
            if let Some(frozen) = frozen {
                let dialog = gtk::FileChooserNative::new(
                    Some(CopyKey::LogsSave.as_str()),
                    Some(&parent),
                    gtk::FileChooserAction::Save,
                    Some("Save"),
                    Some("Cancel"),
                );
                dialog.set_current_name(CopyKey::LogsFilename.as_str());
                dialog.connect_response(move |d, response| {
                    if response == gtk::ResponseType::Accept
                        && let Some(file) = d.file()
                        && let Some(path) = file.path()
                    {
                        let _ = save_frozen_log(&frozen, &path);
                    }
                });
                dialog.show();
            }
        });
    }

    scrolled
}
