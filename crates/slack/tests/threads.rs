//! Thread-map persistence round-trip: a saved map reloads identically and
//! preserves both-direction lookups, so a restarted bridge re-threads.

use kranz_slack::threads::ThreadMap;
use tempfile::TempDir;

#[test]
fn save_then_load_round_trips() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path();

    let mut map = ThreadMap::default();
    map.set("m-01", "1700000000.000100");
    map.set("m-02", "1700000500.000700");
    map.save(repo).unwrap();

    // The file lands where the bridge expects it.
    assert!(ThreadMap::path(repo).is_file());

    let reloaded = ThreadMap::load(repo).unwrap();
    assert_eq!(reloaded, map);
    assert_eq!(reloaded.thread_ts("m-01"), Some("1700000000.000100"));
    assert_eq!(
        reloaded.mission_for_thread("1700000500.000700"),
        Some("m-02")
    );
    assert!(reloaded.contains("m-02"));
    assert!(!reloaded.contains("m-99"));
}

#[test]
fn load_missing_is_empty_map() {
    let tmp = TempDir::new().unwrap();
    let map = ThreadMap::load(tmp.path()).unwrap();
    assert_eq!(map, ThreadMap::default());
    assert!(map.thread_ts("anything").is_none());
}

#[test]
fn load_corrupt_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let path = ThreadMap::path(tmp.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{not json").unwrap();
    assert!(ThreadMap::load(tmp.path()).is_err());
}

#[test]
fn overwrite_persists_new_root() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path();

    let mut map = ThreadMap::default();
    map.set("m-01", "ts-old");
    map.save(repo).unwrap();

    let mut reloaded = ThreadMap::load(repo).unwrap();
    reloaded.set("m-01", "ts-new");
    reloaded.save(repo).unwrap();

    assert_eq!(
        ThreadMap::load(repo).unwrap().thread_ts("m-01"),
        Some("ts-new")
    );
}
