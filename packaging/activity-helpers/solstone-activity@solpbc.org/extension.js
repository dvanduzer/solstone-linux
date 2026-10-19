// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';
import Meta from 'gi://Meta';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';

export default class ActivityExtension extends Extension {
    enable() {
        this._running = true;
        this._client = null;
        this._connection = null;
        this._startConnectionLoop();
    }

    disable() {
        this._running = false;
        if (this._connection) {
            try { this._connection.close(null); } catch (e) {}
            this._connection = null;
        }
        this._client = null;
    }

    _getSocketPath() {
        const runtime = GLib.getenv('XDG_RUNTIME_DIR') || `/run/user/${GLib.get_user_name()}`;
        return `${runtime}/solstone-linux/activity-helper.sock`;
    }

    _startConnectionLoop() {
        if (!this._running) return;
        const path = this._getSocketPath();
        if (!GLib.file_test(path, GLib.FileTest.EXISTS)) {
            GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 5, () => {
                if (this._running && !this._connection) {
                    this._startConnectionLoop();
                }
                return GLib.SOURCE_REMOVE;
            });
            return;
        }

        const client = new Gio.SocketClient();
        const address = new Gio.UnixSocketAddress({ path });
        client.connect_async(address, null, (_source, res) => {
            if (!this._running) return;
            try {
                this._connection = client.connect_finish(res);
                this._readRequests();
            } catch (e) {
                this._connection = null;
                GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 5, () => {
                    if (this._running && !this._connection) {
                        this._startConnectionLoop();
                    }
                    return GLib.SOURCE_REMOVE;
                });
            }
        });
    }

    _readRequests() {
        if (!this._connection || !this._running) return;
        const input = new Gio.DataInputStream({
            base_stream: this._connection.get_input_stream()
        });
        this._readLine(input);
    }

    _readLine(input) {
        if (!this._connection || !this._running) return;
        input.read_line_async(GLib.PRIORITY_DEFAULT, null, (_stream, res) => {
            if (!this._running || !this._connection) return;
            try {
                const [line] = input.read_line_finish_utf8(res);
                if (line === null) {
                    this._connection = null;
                    this._startConnectionLoop();
                    return;
                }
                this._handleRequest(line);
                this._readLine(input);
            } catch (e) {
                this._connection = null;
                this._startConnectionLoop();
            }
        });
    }

    _handleRequest(line) {
        let req;
        try {
            req = JSON.parse(line);
        } catch (e) {
            return;
        }
        if (!req || req.v !== 1 || req.op !== 'snapshot') {
            return;
        }
        const generation = req.generation || 0;
        const wantTitle = Boolean(req.want_title);

        const win = global.display.focus_window;
        const now = Date.now();
        let payload = {
            v: 1,
            generation: generation,
            captured_unix_ms: now,
            app: '',
            app_id: '',
            wm_class: '',
            pid: 0
        };

        if (win) {
            const tracker = Shell.WindowTracker.get_default();
            const app = tracker.get_window_app(win);
            payload.app = app ? app.get_name() : '';
            payload.app_id = app ? app.get_id() : '';
            const winId = win.get_id();
            const winWmClass = win.get_wm_class() || '';
            const winPid = win.get_pid() || 0;
            payload.wm_class = winWmClass;
            payload.pid = winPid;

            if (wantTitle) {
                const secondWin = global.display.focus_window;
                if (!secondWin || secondWin.get_id() !== winId || (secondWin.get_wm_class() || '') !== winWmClass || (secondWin.get_pid() || 0) !== winPid) {
                    payload.app = '';
                    payload.app_id = '';
                    payload.wm_class = '';
                    payload.pid = 0;
                } else {
                    payload.title = win.get_title() || '';
                }
            }
        }

        const out = JSON.stringify(payload) + '\n';
        const bytes = new GLib.Bytes(new TextEncoder().encode(out));
        if (this._connection) {
            const output = this._connection.get_output_stream();
            output.write_bytes_async(bytes, GLib.PRIORITY_DEFAULT, null, null);
        }
    }
}
