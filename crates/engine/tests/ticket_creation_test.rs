use kranz_engine::ticket::Ticket;
use std::sync::{Arc, Barrier};

#[test]
fn concurrent_ticket_creators_preserve_one_complete_body() {
    let repo = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|index| {
            let root = repo.path().to_owned();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let body = Ticket::ticket_template(&format!("Writer {index}"), Some("Goal"), None);
                barrier.wait();
                Ticket::create_markdown(&root, "shared", &body).map(|_| body)
            })
        })
        .collect();
    let winners: Vec<_> = handles
        .into_iter()
        .filter_map(|thread| thread.join().unwrap().ok())
        .collect();
    assert_eq!(winners.len(), 1);
    assert_eq!(
        std::fs::read_to_string(repo.path().join(".kranz/tickets/shared.md")).unwrap(),
        winners[0]
    );
}

#[cfg(unix)]
#[test]
fn ticket_creation_refuses_linked_leaf_and_parent_components() {
    use std::os::unix::fs::symlink;
    for component in ["leaf-missing", "leaf-existing", "tickets", ".kranz"] {
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel.md");
        if component == "leaf-existing" {
            std::fs::write(&sentinel, "keep me").unwrap();
        }
        match component {
            ".kranz" => symlink(outside.path(), repo.path().join(".kranz")).unwrap(),
            "tickets" => {
                std::fs::create_dir(repo.path().join(".kranz")).unwrap();
                symlink(outside.path(), repo.path().join(".kranz/tickets")).unwrap();
            }
            _ => {
                std::fs::create_dir_all(repo.path().join(".kranz/tickets")).unwrap();
                symlink(&sentinel, repo.path().join(".kranz/tickets/example.md")).unwrap();
            }
        }
        assert!(
            Ticket::scaffold(repo.path(), "example", "Example", None, None).is_err(),
            "{component}"
        );
        if component == "leaf-existing" {
            assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "keep me");
        } else {
            assert!(!sentinel.exists());
        }
        assert!(!outside.path().join("example.md").exists());
        assert!(!outside.path().join("tickets").exists());
    }
}

#[test]
fn invalid_ticket_body_does_not_create_a_ticket() {
    let repo = tempfile::tempdir().unwrap();
    assert!(
        Ticket::create_markdown(repo.path(), "example", "---\ntitle: Unclosed frontmatter\n")
            .is_err()
    );
    assert!(!repo.path().join(".kranz").exists());
}
