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

    if gtk::major_version() < 4 || (gtk::major_version() == 4 && gtk::minor_version() < 6) {
        std::process::exit(78);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(70);
        });
    let _runtime_context = runtime.enter();

    let app = gtk::Application::builder()
        .application_id("org.solpbc.solstone.Window")
        .build();

    app.connect_activate(ui::build_ui);
    std::process::exit(app.run().into());
}
