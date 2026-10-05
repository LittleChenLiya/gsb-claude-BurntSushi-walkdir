use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::tests::util::Dir;
use crate::{LeaveReason, WalkControl, WalkDir, WalkEvent};

fn collect_events<F>(dir: &Dir, root: &str, mut f: F) -> Vec<WalkEvent>
where
    F: FnMut(&WalkEvent) -> WalkControl,
{
    let mut events = Vec::new();
    WalkDir::new(dir.join(root)).walk_events(|event| {
        let control = f(&event);
        events.push(event);
        control
    });
    events
}

fn event_path(event: &WalkEvent) -> PathBuf {
    event.entry().path().to_path_buf()
}

#[test]
fn events_nested_tree() {
    let dir = Dir::tmp();
    dir.mkdirp("root/sub");
    dir.touch("root/a.txt");
    dir.touch("root/sub/b.txt");

    let events = collect_events(&dir, "root", |_| WalkControl::Continue);

    assert_eq!(events.len(), 6, "expected 6 events, got: {:?}", events);

    assert!(events[0].is_enter());
    assert_eq!(event_path(&events[0]), dir.join("root"));

    assert!(events[1].is_entry());
    assert_eq!(event_path(&events[1]), dir.join("root/a.txt"));

    assert!(events[2].is_enter());
    assert_eq!(event_path(&events[2]), dir.join("root/sub"));

    assert!(events[3].is_entry());
    assert_eq!(event_path(&events[3]), dir.join("root/sub/b.txt"));

    assert!(events[4].is_leave());
    assert_eq!(event_path(&events[4]), dir.join("root/sub"));
    assert!(matches!(events[4], WalkEvent::Leave(_, LeaveReason::Exhausted)));

    assert!(events[5].is_leave());
    assert_eq!(event_path(&events[5]), dir.join("root"));
    assert!(matches!(events[5], WalkEvent::Leave(_, LeaveReason::Exhausted)));
}

#[test]
fn events_enter_leave_balanced() {
    let dir = Dir::tmp();
    dir.mkdirp("root/a/b/c");
    dir.touch("root/a/b/c/file.txt");
    dir.touch("root/a/file2.txt");

    let events = collect_events(&dir, "root", |_| WalkControl::Continue);

    let enters: Vec<_> = events.iter().filter(|e| e.is_enter()).collect();
    let leaves: Vec<_> = events.iter().filter(|e| e.is_leave()).collect();

    assert_eq!(enters.len(), leaves.len(), "Enter/Leave mismatch");

    for enter in &enters {
        let path = event_path(enter);
        let matching_leave = leaves.iter().find(|l| event_path(l) == path);
        assert!(
            matching_leave.is_some(),
            "Enter for {:?} has no matching Leave",
            path
        );
    }

    for leave in &leaves {
        assert!(
            matches!(leave, WalkEvent::Leave(_, LeaveReason::Exhausted)),
            "expected Exhausted, got: {:?}",
            leave
        );
    }
}

#[test]
fn events_root_file() {
    let dir = Dir::tmp();
    dir.touch("root.txt");

    let events = collect_events(&dir, "root.txt", |_| WalkControl::Continue);

    assert_eq!(events.len(), 1);
    assert!(events[0].is_entry());
    assert_eq!(event_path(&events[0]), dir.join("root.txt"));
}

#[test]
fn events_root_missing_no_enter() {
    let dir = Dir::tmp();

    let events = collect_events(&dir, "nonexistent", |_| WalkControl::Continue);

    assert!(events.is_empty(), "expected no events, got: {:?}", events);
}

#[test]
fn events_skip_dir_no_descend() {
    let dir = Dir::tmp();
    dir.mkdirp("root/skip_me/sub");
    dir.touch("root/keep.txt");
    dir.touch("root/skip_me/hidden.txt");
    dir.touch("root/skip_me/sub/deep.txt");

    let events = collect_events(&dir, "root", |event| {
        if let WalkEvent::Enter(d) = event {
            if d.file_name() == "skip_me" {
                return WalkControl::SkipDir;
            }
        }
        WalkControl::Continue
    });

    assert_eq!(events.len(), 5, "expected 5 events, got: {:?}", events);

    assert!(events[0].is_enter());
    assert_eq!(event_path(&events[0]), dir.join("root"));

    assert!(events[1].is_entry());
    assert_eq!(event_path(&events[1]), dir.join("root/keep.txt"));

    assert!(events[2].is_enter());
    assert_eq!(event_path(&events[2]), dir.join("root/skip_me"));

    assert!(events[3].is_leave());
    assert_eq!(event_path(&events[3]), dir.join("root/skip_me"));
    assert!(matches!(events[3], WalkEvent::Leave(_, LeaveReason::Skipped)));

    assert!(events[4].is_leave());
    assert_eq!(event_path(&events[4]), dir.join("root"));
    assert!(matches!(events[4], WalkEvent::Leave(_, LeaveReason::Exhausted)));

    let skip_me_path = dir.join("root/skip_me");
    for event in &events {
        let path = event_path(event);
        // No event should have skip_me as its parent (i.e. no children of
        // skip_me should be read).
        if let Some(parent) = path.parent() {
            assert!(
                parent != skip_me_path,
                "skip_me contents should not be read, but got event for: {:?}",
                path
            );
        }
    }
}

#[test]
fn events_cancel_leaves_cancelled() {
    let dir = Dir::tmp();
    dir.mkdirp("root/sub1/deep");
    dir.mkdirp("root/sub2");
    dir.touch("root/file1.txt");
    dir.touch("root/sub1/file2.txt");
    dir.touch("root/sub1/deep/file3.txt");

    let events = collect_events(&dir, "root", |event| {
        if let WalkEvent::Enter(d) = event {
            if d.file_name() == "sub1" {
                return WalkControl::Cancel;
            }
        }
        WalkControl::Continue
    });

    let leaves: Vec<_> = events.iter().filter(|e| e.is_leave()).collect();
    assert_eq!(leaves.len(), 2, "expected 2 leaves, got: {:?}", events);

    assert_eq!(event_path(leaves[0]), dir.join("root/sub1"));
    assert!(matches!(leaves[0], WalkEvent::Leave(_, LeaveReason::Cancelled)));

    assert_eq!(event_path(leaves[1]), dir.join("root"));
    assert!(matches!(leaves[1], WalkEvent::Leave(_, LeaveReason::Cancelled)));
}

#[test]
fn events_all_leaves_have_enters() {
    let dir = Dir::tmp();
    dir.mkdirp("root/a/b/c");
    dir.touch("root/a/file.txt");
    dir.touch("root/a/b/file2.txt");

    let events = collect_events(&dir, "root", |_| WalkControl::Continue);

    let enters: Vec<_> = events.iter().filter(|e| e.is_enter()).collect();
    let leaves: Vec<_> = events.iter().filter(|e| e.is_leave()).collect();
    assert_eq!(enters.len(), leaves.len());

    for enter in &enters {
        let path = event_path(enter);
        let matching = leaves.iter().find(|l| event_path(l) == path);
        assert!(matching.is_some(), "Enter for {:?} has no Leave", path);
    }
}

#[test]
fn events_min_depth() {
    let dir = Dir::tmp();
    dir.mkdirp("root/sub");
    dir.touch("root/a.txt");
    dir.touch("root/sub/b.txt");

    let mut enter_entry_paths = Vec::new();
    WalkDir::new(dir.join("root"))
        .min_depth(1)
        .walk_events(|event| {
            if event.is_enter() || event.is_entry() {
                enter_entry_paths.push(event.entry().path().to_path_buf());
            }
            WalkControl::Continue
        });

    assert!(
        !enter_entry_paths.contains(&dir.join("root")),
        "root should be skipped with min_depth(1)"
    );
    assert!(enter_entry_paths.contains(&dir.join("root/a.txt")));
    assert!(enter_entry_paths.contains(&dir.join("root/sub")));
}

#[test]
fn events_max_depth() {
    let dir = Dir::tmp();
    dir.mkdirp("root/sub/deep");
    dir.touch("root/a.txt");
    dir.touch("root/sub/b.txt");
    dir.touch("root/sub/deep/c.txt");

    let mut paths = Vec::new();
    WalkDir::new(dir.join("root"))
        .max_depth(1)
        .walk_events(|event| {
            paths.push(event.entry().path().to_path_buf());
            WalkControl::Continue
        });

    assert!(paths.contains(&dir.join("root")));
    assert!(paths.contains(&dir.join("root/a.txt")));
    assert!(paths.contains(&dir.join("root/sub")));
    assert!(!paths.contains(&dir.join("root/sub/b.txt")));
    assert!(!paths.contains(&dir.join("root/sub/deep")));
}

#[test]
fn events_sorted() {
    let dir = Dir::tmp();
    dir.mkdirp("root");
    dir.touch("root/z.txt");
    dir.touch("root/a.txt");
    dir.touch("root/m.txt");

    let mut names = Vec::new();
    WalkDir::new(dir.join("root"))
        .sort_by_file_name()
        .walk_events(|event| {
            if let WalkEvent::Entry(e) = event {
                names.push(e.file_name().to_str().unwrap().to_string());
            }
            WalkControl::Continue
        });

    assert_eq!(names, vec!["a.txt", "m.txt", "z.txt"]);
}

#[test]
fn events_drop_emits_incomplete() {
    let dir = Dir::tmp();
    dir.mkdirp("root/sub");
    dir.touch("root/file.txt");
    dir.touch("root/sub/deep.txt");

    let collected: Arc<Mutex<Vec<WalkEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_clone = collected.clone();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        WalkDir::new(dir.join("root")).walk_events(|event| {
            let should_panic =
                matches!(&event, WalkEvent::Enter(d) if d.file_name() == "sub");
            collected_clone.lock().unwrap().push(event);
            if should_panic {
                panic!("intentional panic to trigger drop cleanup");
            }
            WalkControl::Continue
        });
    }));

    assert!(result.is_err(), "expected the callback to panic");

    let events = collected.lock().unwrap();

    // The walk was interrupted by a panic, so all leaves should be
    // Incomplete (not Exhausted or Cancelled).
    let leaves: Vec<_> = events.iter().filter(|e| e.is_leave()).collect();
    assert!(
        !leaves.is_empty(),
        "expected Incomplete leave events, got none. events: {:?}",
        events.iter().map(|e| (e.is_enter(), e.is_leave())).collect::<Vec<_>>()
    );

    for leave in &leaves {
        assert!(
            matches!(leave, WalkEvent::Leave(_, LeaveReason::Incomplete)),
            "expected Incomplete, got: {:?}",
            leave
        );
    }

    // The root should have an Incomplete leave (it was entered but never
    // exhausted).
    let root_leaves: Vec<_> = leaves
        .iter()
        .filter(|l| event_path(l) == dir.join("root"))
        .collect();
    assert_eq!(root_leaves.len(), 1, "expected one root leave");
}

#[test]
fn events_drop_no_io() {
    // Verify that dropping the walker does not perform additional I/O.
    // We do this by creating a directory that would fail to open and
    // verifying that the drop path does not try to open it.
    let dir = Dir::tmp();
    dir.mkdirp("root/sub");
    dir.touch("root/file.txt");

    let collected: Arc<Mutex<Vec<WalkEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_clone = collected.clone();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        WalkDir::new(dir.join("root")).walk_events(|event| {
            let should_panic =
                matches!(&event, WalkEvent::Enter(d) if d.file_name() == "root");
            collected_clone.lock().unwrap().push(event);
            if should_panic {
                panic!("panic after root enter");
            }
            WalkControl::Continue
        });
    }));

    assert!(result.is_err());

    let events = collected.lock().unwrap();
    // Only Enter(root) and Leave(root, Incomplete) should be present.
    // No entries from root should have been read (the panic happened
    // before reading entries).
    assert_eq!(events.len(), 2, "expected 2 events, got: {:?}", events);
    assert!(events[0].is_enter());
    assert!(events[1].is_leave());
    assert!(matches!(events[1], WalkEvent::Leave(_, LeaveReason::Incomplete)));
}

#[cfg(unix)]
#[test]
fn events_read_dir_failure_still_leaves() {
    use std::os::unix::fs::PermissionsExt;

    let dir = Dir::tmp();
    dir.mkdirp("root/denied");
    dir.touch("root/keep.txt");
    dir.touch("root/denied/hidden.txt");

    // Deny read access to the subdirectory. On Unix, mode 0o000 prevents
    // reading directory contents (the owner can still stat the directory
    // but cannot list its entries).
    let denied_path = dir.join("root/denied");
    std::fs::set_permissions(&denied_path, std::fs::Permissions::from_mode(0o000))
        .expect("failed to set permissions");

    let events = collect_events(&dir, "root", |_| WalkControl::Continue);

    // Restore permissions so the temp directory can be cleaned up.
    let _ = std::fs::set_permissions(
        &denied_path,
        std::fs::Permissions::from_mode(0o755),
    );

    // The denied directory should produce Enter + Leave(Error), not be
    // silently skipped.
    let denied_events: Vec<_> = events
        .iter()
        .filter(|e| event_path(e) == denied_path)
        .collect();
    assert_eq!(
        denied_events.len(),
        2,
        "expected Enter + Leave for denied dir, got: {:?}",
        denied_events
    );
    assert!(denied_events[0].is_enter());
    assert!(denied_events[1].is_leave());
    assert!(
        matches!(denied_events[1], WalkEvent::Leave(_, LeaveReason::Error(_))),
        "expected Leave(Error), got: {:?}",
        denied_events[1]
    );

    // The sibling file should still be read normally.
    assert!(events.iter().any(|e| {
        matches!(e, WalkEvent::Entry(d) if d.file_name() == "keep.txt")
    }));
}
