use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::events::{walk_events, Event, LeaveReason};
use crate::tests::util::Dir;

/// A compact, test-local summary of one yielded item.
#[derive(Debug)]
enum Item {
    Enter(PathBuf),
    Entry(PathBuf),
    Leave(PathBuf, LeaveReason),
    Err(String),
}

fn collect<P: AsRef<Path>>(root: P) -> Vec<Item> {
    let mut items = vec![];
    for result in walk_events(root) {
        match result {
            Ok(Event::Enter { dir }) => {
                items.push(Item::Enter(dir.into_path()))
            }
            Ok(Event::Entry { entry }) => {
                items.push(Item::Entry(entry.into_path()))
            }
            Ok(Event::Leave { dir, reason }) => {
                items.push(Item::Leave(dir.into_path(), reason))
            }
            Err(err) => items.push(Item::Err(err.to_string())),
        }
    }
    items
}

/// Assert the global structural invariants of a completed event stream:
/// every Leave matches the innermost unmatched Enter, and every directory is
/// eventually left with reason `Completed`.
fn assert_balanced_completed(items: &[Item]) {
    let mut stack: Vec<PathBuf> = vec![];
    for item in items {
        match *item {
            Item::Enter(ref p) => stack.push(p.clone()),
            Item::Entry(_) | Item::Err(_) => {}
            Item::Leave(ref p, reason) => {
                assert_eq!(reason, LeaveReason::Completed);
                assert_eq!(stack.pop().as_ref(), Some(p));
            }
        }
    }
    assert!(stack.is_empty(), "unmatched Enter events: {:?}", stack);
}

fn enter_path(item: &Item) -> Option<&Path> {
    match *item {
        Item::Enter(ref p) => Some(p),
        _ => None,
    }
}

fn entry_path(item: &Item) -> Option<&Path> {
    match *item {
        Item::Entry(ref p) => Some(p),
        _ => None,
    }
}

#[test]
fn nested_tree_events() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");
    dir.mkdirp("c");
    dir.touch("top.txt");
    dir.touch("a/a.txt");
    dir.touch("a/b/b.txt");

    let items = collect(dir.path());

    // 4 directories (root, a, a/b, c) and 3 files.
    let enters = items.iter().filter(|i| enter_path(i).is_some()).count();
    let leaves =
        items.iter().filter(|i| matches!(*i, Item::Leave(..))).count();
    let entries = items.iter().filter(|i| entry_path(i).is_some()).count();
    assert_eq!(enters, 4);
    assert_eq!(leaves, 4);
    assert_eq!(entries, 3);

    // The first and last events frame the whole tree.
    assert_eq!(enter_path(&items[0]), Some(dir.path()));
    match items.last().unwrap() {
        Item::Leave(p, LeaveReason::Completed) => assert_eq!(p, dir.path()),
        ref other => panic!("expected final Leave of root, got {:?}", other),
    }

    assert_balanced_completed(&items);

    // Every created path appears exactly in the expected kind of event.
    let all: Vec<PathBuf> = items
        .iter()
        .filter_map(|i| match *i {
            Item::Enter(ref p) | Item::Entry(ref p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    for expected in &["", "a", "a/b", "c", "top.txt", "a/a.txt", "a/b/b.txt"] {
        assert!(all.contains(&dir.join(expected)), "missing {}", expected);
    }

    // Within a/b, its file is visited between its Enter and its Leave, in
    // that order.
    let ab_enter = items
        .iter()
        .position(|i| enter_path(i) == Some(dir.join("a/b").as_path()))
        .unwrap();
    let ab_file = items
        .iter()
        .position(|i| entry_path(i) == Some(dir.join("a/b/b.txt").as_path()))
        .unwrap();
    let ab_leave = items
        .iter()
        .position(|i| match *i {
            Item::Leave(ref p, _) => p == &dir.join("a/b"),
            _ => false,
        })
        .unwrap();
    assert!(ab_enter < ab_file && ab_file < ab_leave);
}

#[test]
fn empty_directory_events() {
    let dir = Dir::tmp();
    let items = collect(dir.path());
    assert_eq!(items.len(), 2);
    match items[0] {
        Item::Enter(ref p) => assert_eq!(p, dir.path()),
        ref other => panic!("{:?}", other),
    }
    match items[1] {
        Item::Leave(ref p, LeaveReason::Completed) => {
            assert_eq!(p, dir.path())
        }
        ref other => panic!("{:?}", other),
    }
}

#[test]
fn root_file_is_entry_only() {
    let dir = Dir::tmp();
    dir.touch("a");
    let items = collect(dir.join("a"));
    assert_eq!(items.len(), 1);
    match items[0] {
        Item::Entry(ref p) => assert_eq!(p, &dir.join("a")),
        ref other => panic!("{:?}", other),
    }
}

#[test]
fn missing_root_does_not_fabricate_enter() {
    let dir = Dir::tmp();
    let missing = dir.join("does-not-exist");
    let items = collect(&missing);
    assert_eq!(items.len(), 1);
    match items[0] {
        Item::Err(ref msg) => {
            assert!(msg.contains(&missing.display().to_string()));
        }
        ref other => panic!("expected a single Err, got {:?}", other),
    }
}

/// A directory that exists (and is entered) but cannot be opened for
/// reading. We arrange this without permissions by removing the directory in
/// a directory filter, which runs after Enter but before the lazy read.
#[test]
fn read_dir_failure_has_failed_leave() {
    let dir = Dir::tmp();
    dir.mkdirp("gone/child");
    dir.touch("gone/keep.txt");

    let root = dir.path().to_path_buf();
    let closure_root = root.clone();
    let it = walk_events(&root)
        .filter_dirs(move |d| {
            if d.path() == closure_root.join("gone") {
                // Rename out of the way rather than removing outright, so
                // the test does not race against cleanup of an existing
                // subtree on any platform.
                let _ =
                    std::fs::rename(d.path(), closure_root.join("gone-away"));
            }
            true
        })
        .build();

    let mut saw_err = false;
    let mut saw_leave = None;
    for item in it {
        match item {
            Ok(Event::Leave { dir, reason }) => {
                if dir.path() == root.join("gone") {
                    saw_leave = Some(reason);
                }
            }
            Err(_) => {
                // The error corresponds to opening "gone".
                saw_err = true;
            }
            _ => {}
        }
    }
    assert!(saw_err, "read_dir error must still be yielded");
    assert_eq!(saw_leave, Some(LeaveReason::ReadFailed));

    // Restore so the temp dir helper can clean up normally.
    let _ = std::fs::rename(root.join("gone-away"), root.join("gone"));
}

#[test]
fn skipped_directory_is_not_read() {
    let dir = Dir::tmp();
    dir.mkdirp("skip/secret");
    dir.touch("skip/secret/file.txt");
    dir.mkdirp("keep");
    dir.touch("keep/file.txt");

    // If "skip" were read despite being skipped, removing its contents
    // before its Leave would surface as errors.
    let mut it = walk_events(dir.path()).build();
    let mut skipped = None;
    let mut items = vec![];
    // `while let` is needed because the body calls `skip_current_dir`,
    // which borrows the iterator mutably.
    #[allow(clippy::while_let_on_iterator)]
    while let Some(item) = it.next() {
        match item.unwrap() {
            Event::Enter { dir: d } => {
                if d.path() == dir.join("skip") {
                    it.skip_current_dir();
                }
                items.push(Item::Enter(d.into_path()));
            }
            Event::Entry { entry } => {
                items.push(Item::Entry(entry.into_path()));
            }
            Event::Leave { dir: d, reason } => {
                if d.path() == dir.join("skip") {
                    skipped = Some(reason);
                }
                items.push(Item::Leave(d.into_path(), reason));
            }
        }
    }
    assert_eq!(skipped, Some(LeaveReason::Skipped));

    // Nothing below "skip" is ever observed...
    for item in &items {
        let p: Option<&Path> = match *item {
            Item::Enter(ref p) | Item::Entry(ref p) => Some(p.as_ref()),
            _ => None,
        };
        if let Some(p) = p {
            assert!(!p.starts_with(dir.join("skip").join("secret")));
        }
    }
    // ...but "skip" itself is balanced, and the sibling is still walked.
    assert!(items
        .iter()
        .any(|i| enter_path(i) == Some(dir.join("keep").as_path())));
}

#[test]
fn dir_and_file_filters_are_independent() {
    let dir = Dir::tmp();
    dir.mkdirp("hide/inner");
    dir.touch("hide/inner/x");
    dir.mkdirp("show");
    dir.touch("show/keep.txt");
    dir.touch("show/drop.txt");

    let items: Vec<_> = walk_events(dir.path())
        .filter_dirs(|d| d.file_name() != std::ffi::OsStr::new("hide"))
        .filter_files(|d| d.file_name() != std::ffi::OsStr::new("drop.txt"))
        .into_iter()
        .collect::<crate::Result<Vec<_>>>()
        .unwrap();

    let paths: Vec<PathBuf> = items
        .iter()
        .filter_map(|e| {
            e.enter()
                .or_else(|| e.as_entry())
                .or_else(|| e.leave())
                .map(|d| d.path().to_path_buf())
        })
        .collect();

    // The filtered directory has no Enter and no Leave...
    assert!(!paths.contains(&dir.join("hide")));
    // ...the filtered file is absent...
    assert!(!paths.contains(&dir.join("show/drop.txt")));
    // ...while the kept file and directories are present.
    assert!(paths.contains(&dir.join("show/keep.txt")));
    assert!(paths.contains(&dir.join("show")));
}

#[test]
fn cancel_closes_open_dirs() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");
    dir.touch("a/b/c.txt");

    let mut it = walk_events(dir.path()).build();

    // Enter root, then enter "a", then cancel while both are open.
    let root_enter = it.next().unwrap().unwrap();
    assert!(root_enter.enter().is_some());
    let a_enter = it.next().unwrap().unwrap();
    match a_enter {
        Event::Enter { dir: d } => assert_eq!(d.path(), dir.join("a")),
        other => panic!("{:?}", other),
    }

    let completion = it.completion();
    assert!(!completion.is_complete());
    it.cancel();

    let inner = it.next().unwrap().unwrap();
    match inner {
        Event::Leave { dir: d, reason } => {
            assert_eq!(d.path(), dir.join("a"));
            assert_eq!(reason, LeaveReason::Cancelled);
        }
        other => panic!("{:?}", other),
    }
    let outer = it.next().unwrap().unwrap();
    match outer {
        Event::Leave { dir: d, reason } => {
            assert_eq!(d.path(), dir.path());
            assert_eq!(reason, LeaveReason::Cancelled);
        }
        other => panic!("{:?}", other),
    }
    // Nothing unvisited was read.
    assert!(it.next().is_none());
    assert!(completion.is_complete());
}

#[test]
fn dropped_midway_is_incomplete_without_extra_events() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");
    dir.touch("a/b/c.txt");

    let completion;
    {
        let mut it = walk_events(dir.path()).build();
        let _ = it.next().unwrap().unwrap(); // Enter root
        let a = it.next().unwrap().unwrap(); // Enter a
        assert!(a.enter().is_some());
        completion = it.completion();
        assert!(!completion.is_complete());
        // Dropped here: a and root are never left.
    }
    assert!(!completion.is_complete());
}

#[test]
fn complete_stream_marks_completion() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    let it = walk_events(dir.path()).build();
    let completion = it.completion();
    for _ in it {}
    assert!(completion.is_complete());
}

#[test]
fn dir_filter_is_called_once_per_directory() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");
    dir.touch("a/b/c.txt");

    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let it = walk_events(dir.path()).filter_dirs(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
        true
    });
    for item in it {
        item.unwrap();
    }
    // Root, a and a/b: exactly three directories, and no retry reads.
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
