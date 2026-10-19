// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

function checkActiveWindow() {
    callDBus(
        "org.solpbc.SolstoneLinux",
        "/org/solpbc/SolstoneLinux/Activity",
        "org.solpbc.SolstoneLinux.Activity",
        "AuthorizeRead",
        function (want, generation, wantTitle) {
            if (!want) {
                return;
            }
            var win = workspace.activeWindow;
            var payload = {
                generation: generation,
                captured_unix_ms: Date.now(),
                resourceClass: "",
                resourceName: "",
                desktopFileName: "",
                pid: 0
            };
            if (win) {
                payload.resourceClass = win.resourceClass || "";
                payload.resourceName = win.resourceName || "";
                payload.desktopFileName = win.desktopFileName || "";
                payload.pid = win.pid || 0;
                if (wantTitle) {
                    if (workspace.activeWindow !== win) {
                        return;
                    }
                    payload.caption = win.caption || "";
                }
            }
            callDBus(
                "org.solpbc.SolstoneLinux",
                "/org/solpbc/SolstoneLinux/Activity",
                "org.solpbc.SolstoneLinux.Activity",
                "SubmitSnapshot",
                generation,
                JSON.stringify(payload)
            );
        }
    );
}

var watched = null;

function watchWindow(win) {
    if (watched && watched.captionChanged) {
        try {
            watched.captionChanged.disconnect(checkActiveWindow);
        } catch (e) {}
    }
    watched = win;
    if (win && win.captionChanged) {
        win.captionChanged.connect(checkActiveWindow);
    }
}

workspace.windowActivated.connect(function (win) {
    watchWindow(win);
    checkActiveWindow();
});

var refresh = new QTimer();
refresh.interval = 2000;
refresh.timeout.connect(checkActiveWindow);
refresh.start();
checkActiveWindow();
