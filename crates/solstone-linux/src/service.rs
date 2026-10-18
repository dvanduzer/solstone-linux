// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::session_env::{Output, Runner};
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

const UNIT_TEMPLATE: &str = include_str!("solstone-linux.service.in");
const FALLBACK_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(15);
const DESKTOP_ENTRY: &str = "[Desktop Entry]\n\
Version=1.2\n\
Type=Application\n\
Name=solstone app\n\
Comment=the solstone app takes in what you share with it, and all of it goes into your journal\n\
Exec=/bin/sh -c 'systemctl --user import-environment DISPLAY XAUTHORITY XDG_SESSION_TYPE 2>/dev/null; systemctl --user start solstone-linux.service'\n\
Icon=solstone-observer\n\
StartupNotify=false\n\
X-GNOME-Autostart-enabled=true\n\
Hidden=false\n";

pub struct ServicePaths {
    pub home: PathBuf,
    pub binary: PathBuf,
    pub path: Option<String>,
}

impl ServicePaths {
    pub fn production() -> io::Result<Self> {
        Self::from_environment(
            env::var_os("HOME"),
            env::current_exe()?,
            env::var("PATH").ok(),
        )
    }
    fn from_environment(
        home: Option<OsString>,
        binary: PathBuf,
        path: Option<String>,
    ) -> io::Result<Self> {
        let home = home
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "HOME is not set; cannot locate user service files",
                )
            })?;
        Ok(Self { home, binary, path })
    }
    fn unit(&self) -> PathBuf {
        self.home
            .join(".config/systemd/user/solstone-linux.service")
    }
    fn desktop(&self) -> PathBuf {
        self.home.join(".config/autostart/solstone-linux.desktop")
    }
}

fn service_path(binary: &Path, raw: Option<&str>) -> io::Result<String> {
    let binary_dir = binary
        .parent()
        .ok_or_else(|| io::Error::other("current executable has no parent directory"))?
        .to_string_lossy();
    let raw = raw
        .filter(|value| !value.is_empty())
        .unwrap_or(FALLBACK_PATH);
    let mut seen = HashSet::new();
    Ok(std::iter::once(binary_dir.as_ref())
        .chain(raw.split(':'))
        .filter(|entry| seen.insert((*entry).to_owned()))
        .collect::<Vec<_>>()
        .join(":"))
}

fn systemctl(runner: &dyn Runner, args: &[&str]) -> io::Result<Output> {
    let program = runner
        .which("systemctl")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "systemctl not found"))?;
    runner.run(&program, args, SYSTEMCTL_TIMEOUT, &HashMap::new())
}

fn systemctl_nonfatal(
    runner: &dyn Runner,
    args: &[&str],
    operation: &str,
    output: &mut dyn io::Write,
) {
    match systemctl(runner, args) {
        Ok(result) if result.success => {}
        Ok(_) => {
            let _ = writeln!(
                output,
                "Warning: systemctl {operation} failed; run 'systemctl --user {operation}' to inspect the error"
            );
        }
        Err(error) => {
            let _ = writeln!(
                output,
                "Warning: systemctl {operation} failed: {error}; run 'systemctl --user {operation}' after systemd is available"
            );
        }
    }
}

fn service_is_active(runner: &dyn Runner) -> bool {
    matches!(
        systemctl(
            runner,
            &["--user", "is-active", "--quiet", "solstone-linux.service"],
        ),
        Ok(result) if result.success
    )
}

pub fn install(paths: &ServicePaths, runner: &dyn Runner, output: &mut dyn io::Write) -> i32 {
    // Preserve update semantics without racing a freshly-created service: an existing
    // owner is stopped before its replacement starts, while a first install reaches
    // `enable --now` exactly once.
    let was_active = service_is_active(runner);
    let path = match service_path(&paths.binary, paths.path.as_deref()) {
        Ok(path) => path,
        Err(error) => {
            let _ = writeln!(output, "Error: {error}");
            return 1;
        }
    };
    let unit = UNIT_TEMPLATE
        .replace("{BINARY}", &paths.binary.to_string_lossy())
        .replace("{PATH}", &path);
    let unit_path = paths.unit();
    let desktop_path = paths.desktop();
    let write_result = (|| -> io::Result<()> {
        fs::create_dir_all(unit_path.parent().unwrap_or(Path::new(".")))?;
        fs::write(&unit_path, unit)?;
        writeln!(output, "Wrote {}", unit_path.display())?;
        fs::create_dir_all(desktop_path.parent().unwrap_or(Path::new(".")))?;
        // Icon matches tray.rs::KsniTray::id(); no native icon files are installed.
        fs::write(&desktop_path, DESKTOP_ENTRY)?;
        writeln!(output, "Wrote {}", desktop_path.display())?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = writeln!(output, "Error writing service files: {error}");
        return 1;
    }
    if crate::browser::ENABLED
        && let Err(error) = crate::browser::registration::write_all(
            &paths.home,
            &crate::browser::registration::stable_binary(&paths.binary),
            crate::browser::DEV_ENABLED,
        )
    {
        let _ = writeln!(
            output,
            "Warning: could not write every browser registration: {error}"
        );
    }
    systemctl_nonfatal(
        runner,
        &["--user", "daemon-reload"],
        "daemon-reload",
        output,
    );
    if was_active {
        systemctl_nonfatal(
            runner,
            &["--user", "stop", "solstone-linux.service"],
            "stop solstone-linux.service",
            output,
        );
    }
    for (args, operation) in [
        (
            &["--user", "enable", "--now", "solstone-linux.service"][..],
            "enable --now solstone-linux.service",
        ),
        (
            &["--user", "--no-pager", "status", "solstone-linux.service"][..],
            "--no-pager status solstone-linux.service",
        ),
    ] {
        systemctl_nonfatal(runner, args, operation, output);
    }
    0
}

pub fn uninstall(paths: &ServicePaths, runner: &dyn Runner, output: &mut dyn io::Write) -> i32 {
    for (args, operation) in [
        (
            &["--user", "stop", "solstone-linux.service"][..],
            "stop solstone-linux.service",
        ),
        (
            &["--user", "disable", "solstone-linux.service"][..],
            "disable solstone-linux.service",
        ),
    ] {
        systemctl_nonfatal(runner, args, operation, output);
    }
    for path in [paths.unit(), paths.desktop()] {
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            let _ = writeln!(output, "Error removing {}: {error}", path.display());
            return 1;
        }
    }
    if crate::browser::ENABLED
        && let Err(error) = crate::browser::registration::remove_all(&paths.home)
    {
        let _ = writeln!(
            output,
            "Warning: could not remove every browser registration: {error}"
        );
    }
    systemctl_nonfatal(
        runner,
        &["--user", "daemon-reload"],
        "daemon-reload",
        output,
    );
    0
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ServicePresence {
    Absent,
    Inactive,
    Active,
    Uncertain,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum WindowStart {
    Started,
    AlreadyActive,
    Uncertain,
    Failed,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MatchingRestart {
    Restarted,
    NotMatching,
    Uncertain,
    Failed,
}

pub fn classify_user_service(runner: &dyn Runner) -> ServicePresence {
    let out = match systemctl(
        runner,
        &[
            "--user",
            "show",
            "-p",
            "LoadState",
            "-p",
            "ActiveState",
            "--no-pager",
            "solstone-linux.service",
        ],
    ) {
        Ok(out) if out.success => out,
        _ => return ServicePresence::Uncertain,
    };

    let mut load_state = None;
    let mut active_state = None;
    for line in out.stdout.lines() {
        if let Some(val) = line.strip_prefix("LoadState=") {
            load_state = Some(val.trim());
        } else if let Some(val) = line.strip_prefix("ActiveState=") {
            active_state = Some(val.trim());
        }
    }

    match (load_state, active_state) {
        (Some("not-found"), _) => ServicePresence::Absent,
        (Some("loaded"), Some("inactive" | "failed")) => ServicePresence::Inactive,
        (Some("loaded"), Some("active" | "activating" | "deactivating" | "reloading")) => {
            ServicePresence::Active
        }
        _ => ServicePresence::Uncertain,
    }
}

pub fn start_for_window(
    paths: &ServicePaths,
    runner: &dyn Runner,
    output: &mut dyn io::Write,
) -> WindowStart {
    match classify_user_service(runner) {
        ServicePresence::Active => WindowStart::AlreadyActive,
        ServicePresence::Uncertain => WindowStart::Uncertain,
        ServicePresence::Inactive => {
            match systemctl(runner, &["--user", "start", "solstone-linux.service"]) {
                Ok(out) if out.success => WindowStart::Started,
                Ok(_) => {
                    let _ = writeln!(
                        output,
                        "Warning: systemctl start failed; run 'systemctl --user start solstone-linux.service' to inspect the error"
                    );
                    WindowStart::Failed
                }
                Err(error) => {
                    let _ = writeln!(
                        output,
                        "Warning: systemctl start failed: {error}; run 'systemctl --user start solstone-linux.service' after systemd is available"
                    );
                    WindowStart::Uncertain
                }
            }
        }
        ServicePresence::Absent => {
            let path = match service_path(&paths.binary, paths.path.as_deref()) {
                Ok(path) => path,
                Err(error) => {
                    let _ = writeln!(output, "Error: {error}");
                    return WindowStart::Failed;
                }
            };
            let unit = UNIT_TEMPLATE
                .replace("{BINARY}", &paths.binary.to_string_lossy())
                .replace("{PATH}", &path);
            let unit_path = paths.unit();
            let desktop_path = paths.desktop();
            let write_result = (|| -> io::Result<()> {
                fs::create_dir_all(unit_path.parent().unwrap_or(Path::new(".")))?;
                fs::write(&unit_path, unit)?;
                writeln!(output, "Wrote {}", unit_path.display())?;
                fs::create_dir_all(desktop_path.parent().unwrap_or(Path::new(".")))?;
                fs::write(&desktop_path, DESKTOP_ENTRY)?;
                writeln!(output, "Wrote {}", desktop_path.display())?;
                Ok(())
            })();
            if let Err(error) = write_result {
                let _ = writeln!(output, "Error writing service files: {error}");
                return WindowStart::Failed;
            }
            if crate::browser::ENABLED
                && let Err(error) = crate::browser::registration::write_all(
                    &paths.home,
                    &crate::browser::registration::stable_binary(&paths.binary),
                    crate::browser::DEV_ENABLED,
                )
            {
                let _ = writeln!(
                    output,
                    "Warning: could not write every browser registration: {error}"
                );
            }
            systemctl_nonfatal(
                runner,
                &["--user", "daemon-reload"],
                "daemon-reload",
                output,
            );
            match systemctl(
                runner,
                &["--user", "enable", "--now", "solstone-linux.service"],
            ) {
                Ok(out) if out.success => WindowStart::Started,
                Ok(_) => {
                    let _ = writeln!(
                        output,
                        "Warning: systemctl enable --now failed; run 'systemctl --user enable --now solstone-linux.service' to inspect the error"
                    );
                    WindowStart::Failed
                }
                Err(error) => {
                    let _ = writeln!(
                        output,
                        "Warning: systemctl enable --now failed: {error}; run 'systemctl --user enable --now solstone-linux.service' after systemd is available"
                    );
                    WindowStart::Uncertain
                }
            }
        }
    }
}

pub fn restart_matching_pid(
    runner: &dyn Runner,
    expected_pid: u32,
    output: &mut dyn io::Write,
) -> MatchingRestart {
    let out = match systemctl(
        runner,
        &[
            "--user",
            "show",
            "-p",
            "MainPID",
            "--no-pager",
            "solstone-linux.service",
        ],
    ) {
        Ok(out) if out.success => out,
        _ => return MatchingRestart::Uncertain,
    };

    let mut main_pid = None;
    for line in out.stdout.lines() {
        if let Some(val) = line.strip_prefix("MainPID=") {
            let trimmed = val.trim();
            if let Ok(pid) = trimmed.parse::<u32>() {
                main_pid = Some(pid);
            } else {
                return MatchingRestart::Uncertain;
            }
        }
    }

    match main_pid {
        None => MatchingRestart::NotMatching,
        Some(0) => MatchingRestart::NotMatching,
        Some(pid) if pid != expected_pid => MatchingRestart::NotMatching,
        Some(_) => match systemctl(runner, &["--user", "restart", "solstone-linux.service"]) {
            Ok(res) if res.success => MatchingRestart::Restarted,
            Ok(_) => {
                let _ = writeln!(
                    output,
                    "Warning: systemctl restart failed; run 'systemctl --user restart solstone-linux.service' to inspect the error"
                );
                MatchingRestart::Failed
            }
            Err(error) => {
                let _ = writeln!(
                    output,
                    "Warning: systemctl restart failed: {error}; run 'systemctl --user restart solstone-linux.service' after systemd is available"
                );
                MatchingRestart::Uncertain
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, os::unix::fs::PermissionsExt};

    struct FakeRunner {
        found: bool,
        success: bool,
        active: bool,
        stdout: String,
        error: bool,
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl FakeRunner {
        fn ok() -> Self {
            Self {
                found: true,
                success: true,
                active: true,
                stdout: String::new(),
                error: false,
                calls: RefCell::new(Vec::new()),
            }
        }
    }
    impl Runner for FakeRunner {
        fn which(&self, _: &str) -> Option<String> {
            self.found.then(|| "/usr/bin/systemctl".into())
        }
        fn run(
            &self,
            _: &str,
            args: &[&str],
            _: Duration,
            _: &HashMap<String, String>,
        ) -> io::Result<Output> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|value| (*value).into()).collect());
            if self.error {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "fake timeout"));
            }
            let active_query = args == ["--user", "is-active", "--quiet", "solstone-linux.service"];
            let is_show = args.contains(&"show");
            Ok(Output {
                success: if active_query {
                    self.active
                } else if is_show && !self.stdout.is_empty() {
                    true
                } else {
                    self.success
                },
                stdout: self.stdout.clone(),
            })
        }
    }
    fn fixture(t: &tempfile::TempDir, path: Option<&str>) -> ServicePaths {
        let binary = t.path().join("bin/solstone-linux");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, b"binary").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        ServicePaths {
            home: t.path().join("home"),
            binary,
            path: path.map(str::to_owned),
        }
    }

    // tests/test_cli.py::test_cmd_install_service_uses_environment_path
    #[test]
    fn environment_path_binary_first_and_deduplicated() {
        let t = tempfile::tempdir().unwrap();
        let paths = fixture(&t, Some("/usr/local/bin:/tmp/.x/bin:/usr/bin:/tmp/.x/bin"));
        install(&paths, &FakeRunner::ok(), &mut Vec::new());
        let unit = fs::read_to_string(paths.unit()).unwrap();
        assert!(unit.contains(&format!(
            "Environment=PATH={}:{}",
            paths.binary.parent().unwrap().display(),
            "/usr/local/bin:/tmp/.x/bin:/usr/bin"
        )));
    }
    // tests/test_cli.py::test_cmd_install_service_uses_default_path_when_missing
    // tests/test_cli.py::test_cmd_install_service_uses_default_path_when_empty
    #[test]
    fn unset_or_empty_path_uses_fallback() {
        for raw in [None, Some("")] {
            let t = tempfile::tempdir().unwrap();
            let paths = fixture(&t, raw);
            install(&paths, &FakeRunner::ok(), &mut Vec::new());
            assert!(fs::read_to_string(paths.unit()).unwrap().contains(&format!(
                "Environment=PATH={}:{FALLBACK_PATH}",
                paths.binary.parent().unwrap().display()
            )));
        }
    }

    #[test]
    fn installed_unit_has_a_bounded_cleanup_backstop() {
        let t = tempfile::tempdir().unwrap();
        let paths = fixture(&t, None);
        install(&paths, &FakeRunner::ok(), &mut Vec::new());

        let unit = fs::read_to_string(paths.unit()).unwrap();
        assert!(unit.contains("TimeoutStopSec=90"));
    }
    // tests/test_cli.py::test_cmd_install_service_always_rewrites
    #[test]
    fn always_rewrites_and_repeats_rust_sequence() {
        let t = tempfile::tempdir().unwrap();
        let paths = fixture(&t, None);
        let runner = FakeRunner::ok();
        assert_eq!(install(&paths, &runner, &mut Vec::new()), 0);
        fs::write(paths.unit(), b"changed").unwrap();
        assert_eq!(install(&paths, &runner, &mut Vec::new()), 0);
        assert!(
            fs::read_to_string(paths.unit())
                .unwrap()
                .starts_with("[Unit]")
        );
        // An existing owner is stopped before its replacement starts. A restart after
        // `enable --now` would race a newly-created service during desktop initialization.
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 10);
        assert_eq!(
            calls[0],
            ["--user", "is-active", "--quiet", "solstone-linux.service"],
        );
        assert_eq!(calls[1], ["--user", "daemon-reload"],);
        assert_eq!(calls[2], ["--user", "stop", "solstone-linux.service"]);
        assert_eq!(
            calls[3],
            &["--user", "enable", "--now", "solstone-linux.service"],
        );
        assert_eq!(
            calls[4],
            &["--user", "--no-pager", "status", "solstone-linux.service"],
        );
        assert_eq!(&calls[..5], &calls[5..]);
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|argument| argument == "restart"))
        );
    }

    #[test]
    fn first_install_starts_once_without_stopping_or_restarting() {
        let t = tempfile::tempdir().unwrap();
        let paths = fixture(&t, None);
        let runner = FakeRunner {
            found: true,
            success: true,
            active: false,
            stdout: String::new(),
            error: false,
            calls: RefCell::new(Vec::new()),
        };

        assert_eq!(install(&paths, &runner, &mut Vec::new()), 0);

        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls[0],
            ["--user", "is-active", "--quiet", "solstone-linux.service"],
        );
        assert_eq!(calls[1], ["--user", "daemon-reload"]);
        assert_eq!(
            calls[2],
            ["--user", "enable", "--now", "solstone-linux.service"],
        );
        assert_eq!(
            calls[3],
            ["--user", "--no-pager", "status", "solstone-linux.service"],
        );
        assert!(calls.iter().all(|call| {
            !call
                .iter()
                .any(|argument| argument == "stop" || argument == "restart")
        }));
    }
    // tests/test_cli.py::test_cmd_install_service_writes_autostart_entry
    #[test]
    fn writes_reference_autostart_entry() {
        let t = tempfile::tempdir().unwrap();
        let paths = fixture(&t, None);
        install(&paths, &FakeRunner::ok(), &mut Vec::new());
        let value = fs::read_to_string(paths.desktop()).unwrap();
        for expected in [
            "Type=Application",
            "solstone-linux.service",
            "import-environment",
            "DISPLAY",
            "XAUTHORITY",
            "XDG_SESSION_TYPE",
            "Icon=solstone-observer",
        ] {
            assert!(value.contains(expected));
        }
    }
    // missing systemctl is non-fatal after files are written.
    #[test]
    fn systemctl_missing_is_nonfatal() {
        let t = tempfile::tempdir().unwrap();
        let paths = fixture(&t, None);
        let runner = FakeRunner {
            found: false,
            success: true,
            active: false,
            stdout: String::new(),
            error: false,
            calls: RefCell::new(Vec::new()),
        };
        let mut output = Vec::new();
        assert_eq!(install(&paths, &runner, &mut output), 0);
        assert!(paths.unit().exists() && paths.desktop().exists());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("systemctl daemon-reload failed"));
        assert!(output.contains("after systemd is available"));
        assert!(runner.calls.borrow().is_empty());
    }
    // present but failing systemctl is non-fatal after files are written.
    #[test]
    fn systemctl_nonzero_is_nonfatal() {
        let t = tempfile::tempdir().unwrap();
        let paths = fixture(&t, None);
        let runner = FakeRunner {
            found: true,
            success: false,
            active: false,
            stdout: String::new(),
            error: false,
            calls: RefCell::new(Vec::new()),
        };
        let mut output = Vec::new();
        assert_eq!(install(&paths, &runner, &mut output), 0);
        assert!(paths.unit().exists() && paths.desktop().exists());
        assert_eq!(runner.calls.borrow().len(), 4);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("systemctl enable --now solstone-linux.service failed"));
        assert!(output.contains("to inspect the error"));
    }
    // missing HOME cannot redirect service writes or removals into the current directory.
    #[test]
    fn production_paths_require_home() {
        let error =
            ServicePaths::from_environment(None, PathBuf::from("/usr/bin/solstone-linux"), None)
                .err()
                .expect("missing HOME must fail");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("HOME is not set"));
    }
    // uninstall is ordered, idempotent, and preserves all owner data.
    #[test]
    fn uninstall_fake_tree_preserves_owner_data() {
        let t = tempfile::tempdir().unwrap();
        let paths = fixture(&t, None);
        fs::create_dir_all(paths.unit().parent().unwrap()).unwrap();
        fs::create_dir_all(paths.desktop().parent().unwrap()).unwrap();
        fs::write(paths.unit(), b"unit").unwrap();
        fs::write(paths.desktop(), b"desktop").unwrap();
        let sentinels = [
            ".config/solstone-linux/config.json",
            ".config/solstone-linux/restore_token",
            ".local/share/solstone-linux/state/facts",
            ".local/share/solstone-linux/captures/day/seg/file",
        ];
        for path in sentinels {
            let path = paths.home.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, path.to_string_lossy().as_bytes()).unwrap();
        }
        let runner = FakeRunner::ok();
        for _ in 0..2 {
            assert_eq!(uninstall(&paths, &runner, &mut Vec::new()), 0);
        }
        assert!(!paths.unit().exists() && !paths.desktop().exists());
        for path in sentinels {
            let path = paths.home.join(path);
            assert_eq!(fs::read(&path).unwrap(), path.to_string_lossy().as_bytes());
        }
        let calls = runner.calls.borrow();
        assert_eq!(&calls[0][1..], &["stop", "solstone-linux.service"]);
        assert_eq!(&calls[1][1..], &["disable", "solstone-linux.service"]);
        assert_eq!(&calls[2][1..], &["daemon-reload"]);
    }

    #[test]
    fn start_for_window_absent_flow() {
        let t = tempfile::tempdir().unwrap();
        let paths = fixture(&t, None);
        let runner = FakeRunner {
            found: true,
            success: true,
            active: false,
            stdout: "LoadState=not-found\nActiveState=inactive\n".into(),
            error: false,
            calls: RefCell::new(Vec::new()),
        };
        let mut out = Vec::new();
        assert_eq!(
            start_for_window(&paths, &runner, &mut out),
            WindowStart::Started
        );
        let calls = runner.calls.borrow();
        assert!(
            calls
                .iter()
                .all(|c| !c.iter().any(|arg| arg == "stop" || arg == "restart"))
        );
        let unit_content = fs::read_to_string(paths.unit()).unwrap();
        assert!(unit_content.contains(&format!("ExecStart={} run", paths.binary.display())));
    }

    #[test]
    fn start_for_window_inactive_and_failed_is_start_only() {
        for state in ["inactive", "failed"] {
            let t = tempfile::tempdir().unwrap();
            let paths = fixture(&t, None);
            let runner = FakeRunner {
                found: true,
                success: true,
                active: false,
                stdout: format!("LoadState=loaded\nActiveState={state}\n"),
                error: false,
                calls: RefCell::new(Vec::new()),
            };
            let mut out = Vec::new();
            assert_eq!(
                start_for_window(&paths, &runner, &mut out),
                WindowStart::Started
            );
            let calls = runner.calls.borrow();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[1], ["--user", "start", "solstone-linux.service"]);
            assert!(
                calls
                    .iter()
                    .all(|c| !c.iter().any(|arg| arg == "stop" || arg == "restart"))
            );
        }
    }

    #[test]
    fn start_for_window_active_and_uncertain_do_not_start_stop_or_restart() {
        for (load, active, expected) in [
            ("loaded", "active", WindowStart::AlreadyActive),
            ("loaded", "activating", WindowStart::AlreadyActive),
            ("loaded", "deactivating", WindowStart::AlreadyActive),
            ("loaded", "reloading", WindowStart::AlreadyActive),
            ("something-else", "active", WindowStart::Uncertain),
            ("not-found-bad", "unknown", WindowStart::Uncertain),
        ] {
            let t = tempfile::tempdir().unwrap();
            let paths = fixture(&t, None);
            let runner = FakeRunner {
                found: true,
                success: true,
                active: true,
                stdout: format!("LoadState={load}\nActiveState={active}\n"),
                error: false,
                calls: RefCell::new(Vec::new()),
            };
            let mut out = Vec::new();
            assert_eq!(start_for_window(&paths, &runner, &mut out), expected);
            let calls = runner.calls.borrow();
            assert_eq!(calls.len(), 1);
            assert!(calls.iter().all(|c| {
                !c.iter()
                    .any(|arg| arg == "start" || arg == "stop" || arg == "restart")
            }));
        }
    }

    #[test]
    fn restart_matching_pid_requires_exact_nonzero_match() {
        let runner = FakeRunner {
            found: true,
            success: true,
            active: true,
            stdout: "MainPID=1234\n".into(),
            error: false,
            calls: RefCell::new(Vec::new()),
        };
        let mut out = Vec::new();
        assert_eq!(
            restart_matching_pid(&runner, 1234, &mut out),
            MatchingRestart::Restarted
        );
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], ["--user", "restart", "solstone-linux.service"]);

        let runner = FakeRunner {
            found: true,
            success: true,
            active: true,
            stdout: "MainPID=5678\n".into(),
            error: false,
            calls: RefCell::new(Vec::new()),
        };
        let mut out = Vec::new();
        assert_eq!(
            restart_matching_pid(&runner, 1234, &mut out),
            MatchingRestart::NotMatching
        );
        assert_eq!(runner.calls.borrow().len(), 1);

        let runner = FakeRunner {
            found: true,
            success: true,
            active: true,
            stdout: "MainPID=0\n".into(),
            error: false,
            calls: RefCell::new(Vec::new()),
        };
        let mut out = Vec::new();
        assert_eq!(
            restart_matching_pid(&runner, 0, &mut out),
            MatchingRestart::NotMatching
        );
        assert_eq!(runner.calls.borrow().len(), 1);

        let runner = FakeRunner {
            found: true,
            success: false,
            active: true,
            stdout: "MainPID=1234\n".into(),
            error: false,
            calls: RefCell::new(Vec::new()),
        };
        let mut out = Vec::new();
        assert_eq!(
            restart_matching_pid(&runner, 1234, &mut out),
            MatchingRestart::Failed
        );
        assert_eq!(runner.calls.borrow().len(), 2);

        let runner = FakeRunner {
            found: true,
            success: true,
            active: true,
            stdout: "MainPID=1234\n".into(),
            error: true,
            calls: RefCell::new(Vec::new()),
        };
        let mut out = Vec::new();
        assert_eq!(
            restart_matching_pid(&runner, 1234, &mut out),
            MatchingRestart::Uncertain
        );
        assert_eq!(runner.calls.borrow().len(), 1);
    }
}
