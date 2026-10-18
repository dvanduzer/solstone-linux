// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod controller;
mod copy_map;
mod format;
mod logs;
mod mark_card;
mod ui;

use gtk::gio::prelude::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("solstone-linux-window {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let app = gtk::Application::builder()
        .application_id("org.solpbc.solstone.Window")
        .build();

    app.connect_activate(ui::build_ui);
    app.run();
}
