// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
//
// KPackage structure follows https://develop.kde.org/docs/plasma/kwin/
// X-Plasma-MainScript is relative to contents/, so this file is contents/code/main.js.

function checkActiveWindow() {
    callDBus(
        "org.solpbc.SolstoneLinux",
        "/org/solpbc/SolstoneLinux/Activity",
        "org.solpbc.SolstoneLinux.Activity",
        "AuthorizeRead",
        function (want, generation, wantTitle, expectedClass, expectedPid) {
            if (!want) {
                return;
            }
            var win = workspace.activeWindow;
            if (!win) {
                return;
            }
            var resourceClass = win.resourceClass || "";
            var pid = win.pid || 0;
            var payload = {
                generation: generation,
                captured_unix_ms: Date.now(),
                resourceClass: resourceClass,
                resourceName: win.resourceName || "",
                desktopFileName: win.desktopFileName || "",
                pid: pid
            };
            if (wantTitle) {
                if (!expectedClass || resourceClass !== expectedClass || pid !== expectedPid || workspace.activeWindow !== win) {
                    callDBus(
                        "org.solpbc.SolstoneLinux",
                        "/org/solpbc/SolstoneLinux/Activity",
                        "org.solpbc.SolstoneLinux.Activity",
                        "SubmitSnapshot",
                        generation,
                        JSON.stringify({
                            generation: generation,
                            captured_unix_ms: Date.now(),
                            resourceClass: "",
                            resourceName: "",
                            desktopFileName: "",
                            pid: 0
                        })
                    );
                    return;
                }
                payload.caption = win.caption || "";
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
