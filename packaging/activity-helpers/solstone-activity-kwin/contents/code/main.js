// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
//
// KPackage structure follows https://develop.kde.org/docs/plasma/kwin/
// X-Plasma-MainScript is relative to contents/, so this file is contents/code/main.js.

// Window.internalId is a QUuid. https://develop.kde.org/docs/plasma/kwin/api/
// A caption is read only after that id matches. Class and pid are not enough.
function checkActiveWindow() {
    callDBus(
        "org.solpbc.SolstoneLinux",
        "/org/solpbc/SolstoneLinux/Activity",
        "org.solpbc.SolstoneLinux.Activity",
        "AuthorizeRead",
        function (want, generation, wantTitle, expectedClass, expectedPid, expectedWindow) {
            if (!want) {
                return;
            }
            var win = workspace.activeWindow;
            if (!win) {
                return;
            }
            var internalId = String(win.internalId || "");
            var resourceClass = win.resourceClass || "";
            var pid = win.pid || 0;
            if (
                wantTitle &&
                (internalId === "" ||
                    internalId !== String(expectedWindow || "") ||
                    workspace.activeWindow !== win)
            ) {
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
                        pid: 0,
                        internalId: ""
                    })
                );
                return;
            }
            var payload = {
                generation: generation,
                captured_unix_ms: Date.now(),
                resourceClass: resourceClass,
                resourceName: win.resourceName || "",
                desktopFileName: win.desktopFileName || "",
                pid: pid,
                internalId: internalId
            };
            if (wantTitle) {
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
            void expectedClass;
            void expectedPid;
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
