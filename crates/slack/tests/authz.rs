//! Spend-authorization gate (docs/slack-management.md must-have #1) at the
//! integration boundary — `SlackConfig::is_authorized` as a pure function of
//! the allowlist + the invoking user id. No socket, no network.

use kranz_slack::{NotifyFlags, SlackConfig};

/// A config with the given allowlist and no open-posture acknowledgement; the
/// tokens/channel are irrelevant to the gate but must be present to build a
/// `SlackConfig`.
fn cfg(allow_users: Vec<&str>) -> SlackConfig {
    cfg_with(allow_users, false)
}

/// A config with the given allowlist and `allowAllUsers` acknowledgement.
fn cfg_with(allow_users: Vec<&str>, allow_all_users: bool) -> SlackConfig {
    SlackConfig {
        bot_token: "xoxb".into(),
        app_token: "xapp".into(),
        channel: "C1".into(),
        notify: NotifyFlags::default(),
        allow_users: allow_users.into_iter().map(String::from).collect(),
        allow_all_users,
        dashboard_url: None,
        instance_name: None,
    }
}

#[test]
fn empty_allowlist_fails_closed_without_allow_all_users() {
    let c = cfg(vec![]);
    assert!(
        !c.is_authorized(Some("U-anyone")),
        "empty list without allowAllUsers denies everyone"
    );
    assert!(
        !c.is_authorized(None),
        "empty list without allowAllUsers denies even a missing user id"
    );
}

#[test]
fn empty_allowlist_with_allow_all_users_keeps_the_open_posture() {
    let c = cfg_with(vec![], true);
    assert!(
        c.is_authorized(Some("U-anyone")),
        "allowAllUsers: true deliberately opens spend to anyone"
    );
    assert!(
        c.is_authorized(None),
        "allowAllUsers: true allows even a missing user id"
    );
}

#[test]
fn nonempty_allowlist_authorizes_only_listed_users() {
    let c = cfg(vec!["U123", "U456"]);
    assert!(c.is_authorized(Some("U123")), "listed user authorized");
    assert!(
        c.is_authorized(Some("U456")),
        "other listed user authorized"
    );
    assert!(!c.is_authorized(Some("U999")), "unlisted user denied");
}

#[test]
fn allowlist_denies_missing_or_blank_user_when_set() {
    let c = cfg(vec!["U123"]);
    assert!(
        !c.is_authorized(None),
        "no user id can't slip past a configured gate"
    );
    assert!(!c.is_authorized(Some("   ")), "blank user id denied");
    // Surrounding whitespace on a real id is tolerated (Slack shouldn't send it,
    // but the gate must not reject a genuine operator over stray padding).
    assert!(
        c.is_authorized(Some("  U123 ")),
        "whitespace around a real id tolerated"
    );
}
