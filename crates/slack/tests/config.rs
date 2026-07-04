//! `SlackConfig::from_config` resolution: from env, from `~/.kranz/config.json`,
//! env-over-file precedence, and `None` when unconfigured.
//!
//! `from_config` reads process-global state (`HOME`/`USERPROFILE` for the file
//! path, plus the `KRANZ_SLACK_*` env vars). Mutating those is inherently
//! racy under the default parallel test runner, so ALL of it lives in ONE test
//! that drives the scenarios sequentially, saving and restoring every relevant
//! var around each. (An alternative — a static Mutex across several `#[test]`s —
//! still can't stop a *different* integration test binary from touching env, but
//! this crate's other test files don't, and one test keeps it airtight here.)

use kranz_slack::SlackConfig;
use tempfile::TempDir;

/// The env vars this suite manipulates.
const HOME_KEYS: &[&str] = if cfg!(windows) { &["USERPROFILE"] } else { &["HOME"] };
const SLACK_KEYS: &[&str] = &[
    "KRANZ_SLACK_BOT_TOKEN",
    "KRANZ_SLACK_APP_TOKEN",
    "KRANZ_SLACK_CHANNEL",
    "KRANZ_SLACK_DASHBOARD_URL",
    "KRANZ_SLACK_INSTANCE",
];

/// Snapshot the current values of the given keys so they can be restored.
fn snapshot(keys: &[&str]) -> Vec<(String, Option<String>)> {
    keys.iter().map(|k| (k.to_string(), std::env::var(k).ok())).collect()
}

fn restore(saved: &[(String, Option<String>)]) {
    for (k, v) in saved {
        match v {
            Some(val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
        }
    }
}

fn clear(keys: &[&str]) {
    for k in keys {
        std::env::remove_var(k);
    }
}

/// Point HOME (or USERPROFILE) at `dir` so `global_config()` resolves under it.
fn set_home(dir: &std::path::Path) {
    for k in HOME_KEYS {
        std::env::set_var(k, dir);
    }
}

/// Write a `~/.kranz/config.json` under `home` with the given raw JSON.
fn write_global_config(home: &std::path::Path, json: &str) {
    let dir = home.join(".kranz");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), json).unwrap();
}

#[test]
fn from_config_scenarios() {
    // A dummy repo root — tokens are never read from the repo, so its contents
    // don't matter, but from_config takes one.
    let repo = TempDir::new().unwrap();
    let repo_root = repo.path();

    let saved_home = snapshot(HOME_KEYS);
    let saved_slack = snapshot(SLACK_KEYS);
    // Ensure a clean baseline regardless of the outer environment.
    clear(SLACK_KEYS);

    let result = std::panic::catch_unwind(|| {
        // --- 1. Unconfigured: empty HOME, no env → None ------------------
        {
            let home = TempDir::new().unwrap();
            set_home(home.path());
            clear(SLACK_KEYS);
            assert!(
                SlackConfig::from_config(repo_root).unwrap().is_none(),
                "no file, no env → None"
            );
        }

        // --- 2. From env only -------------------------------------------
        {
            let home = TempDir::new().unwrap();
            set_home(home.path()); // empty ~/.kranz — env supplies everything
            clear(SLACK_KEYS);
            std::env::set_var("KRANZ_SLACK_BOT_TOKEN", "xoxb-from-env");
            std::env::set_var("KRANZ_SLACK_APP_TOKEN", "xapp-from-env");
            std::env::set_var("KRANZ_SLACK_CHANNEL", "C-ENV");
            std::env::set_var("KRANZ_SLACK_INSTANCE", "studio-env");

            let cfg = SlackConfig::from_config(repo_root).unwrap().expect("env triple → Some");
            assert_eq!(cfg.bot_token, "xoxb-from-env");
            assert_eq!(cfg.app_token, "xapp-from-env");
            assert_eq!(cfg.channel, "C-ENV");
            assert_eq!(
                cfg.instance_name.as_deref(),
                Some("studio-env"),
                "instance name read from KRANZ_SLACK_INSTANCE"
            );
            // Defaults: all notify classes on.
            assert!(cfg.notify.plan_ready);
            assert!(cfg.notify.blocked);
            assert!(cfg.notify.complete);
        }

        // --- 3. From ~/.kranz/config.json only --------------------------
        {
            let home = TempDir::new().unwrap();
            set_home(home.path());
            clear(SLACK_KEYS);
            write_global_config(
                home.path(),
                r#"{
                    "slack": {
                        "botToken": "xoxb-from-file",
                        "appToken": "xapp-from-file",
                        "channel": "C-FILE",
                        "notify": { "planReady": false, "needsContext": true, "blocked": true, "complete": false }
                    }
                }"#,
            );

            let cfg = SlackConfig::from_config(repo_root).unwrap().expect("file triple → Some");
            assert_eq!(cfg.bot_token, "xoxb-from-file");
            assert_eq!(cfg.app_token, "xapp-from-file");
            assert_eq!(cfg.channel, "C-FILE");
            assert!(!cfg.notify.plan_ready, "file notify override honored");
            assert!(cfg.notify.blocked);
            assert!(!cfg.notify.complete);
            // BACKCOMPAT: a config written before `instanceName` existed (no
            // key in the file, no env var) resolves to None — no labels.
            assert_eq!(cfg.instance_name, None, "absent everywhere → None");
        }

        // --- 4. Env wins over file, per field ---------------------------
        {
            let home = TempDir::new().unwrap();
            set_home(home.path());
            clear(SLACK_KEYS);
            write_global_config(
                home.path(),
                r#"{ "slack": { "botToken": "xoxb-file", "appToken": "xapp-file",
                     "channel": "C-FILE", "instanceName": "laptop-file" } }"#,
            );
            // Env overrides only the channel + instance; tokens from the file.
            std::env::set_var("KRANZ_SLACK_CHANNEL", "C-ENV-WINS");
            std::env::set_var("KRANZ_SLACK_INSTANCE", "laptop-env");

            let cfg = SlackConfig::from_config(repo_root).unwrap().expect("Some");
            assert_eq!(cfg.bot_token, "xoxb-file", "token from file");
            assert_eq!(cfg.channel, "C-ENV-WINS", "channel from env");
            assert_eq!(cfg.instance_name.as_deref(), Some("laptop-env"), "instance from env");
        }

        // --- 5. Partial (missing channel) → None ------------------------
        {
            let home = TempDir::new().unwrap();
            set_home(home.path());
            clear(SLACK_KEYS);
            write_global_config(
                home.path(),
                r#"{ "slack": { "botToken": "xoxb-only", "appToken": "xapp-only" } }"#,
            );
            assert!(
                SlackConfig::from_config(repo_root).unwrap().is_none(),
                "no channel anywhere → None"
            );
        }

        // --- 6. Config file with no `slack` key → None ------------------
        {
            let home = TempDir::new().unwrap();
            set_home(home.path());
            clear(SLACK_KEYS);
            write_global_config(home.path(), r#"{ "worker": { "model": "sonnet" } }"#);
            assert!(SlackConfig::from_config(repo_root).unwrap().is_none());
        }

        // --- 7. Malformed config file → Err -----------------------------
        {
            let home = TempDir::new().unwrap();
            set_home(home.path());
            clear(SLACK_KEYS);
            write_global_config(home.path(), "{ not valid json");
            assert!(
                SlackConfig::from_config(repo_root).is_err(),
                "invalid JSON surfaces an error, not a silent disable"
            );
        }
    });

    // Always restore the environment, then propagate any test panic.
    restore(&saved_slack);
    restore(&saved_home);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
