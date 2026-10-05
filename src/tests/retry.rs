//! Tests for the directory retry policy.
//!
//! These tests use an injected [`DirOpener`] that scripts when opens and
//! reads fail, allowing the retry behavior to be triggered deterministically.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::tests::util::Dir;
use crate::{DirOpener, DirReader, ErrorStage, WalkDir};

/// Events recorded by the mock opener/reader.
///
/// These are used to assert the ordering of handle operations (e.g. that a
/// handle is closed before the next open).
#[derive(Debug, Clone, PartialEq)]
enum Event {
    Open(PathBuf),
    Close(PathBuf),
}

/// A mock opener that scripts when opens and reads fail.
#[derive(Debug)]
struct MockOpener {
    events: Arc<Mutex<Vec<Event>>>,
    script: Mutex<VecDeque<OpenAction>>,
}

/// A scripted action for a single `open` call.
#[derive(Debug)]
enum OpenAction {
    /// Fail the open with this error.
    Fail(io::Error),
    /// Succeed the open with a reader that follows this script.
    Succeed(ReaderScript),
}

/// A script for a single opened reader.
#[derive(Debug)]
struct ReaderScript {
    /// The real directory to read entries from.
    real_dir: PathBuf,
    /// The script of read actions, consumed in order. When exhausted, reads
    /// delegate to the real reader.
    actions: VecDeque<ReadAction>,
}

/// A scripted action for a single `next` call on a reader.
#[derive(Debug)]
enum ReadAction {
    /// Delegate to the real reader.
    Read,
    /// Inject a read failure.
    Fail(io::Error),
}

/// A mock reader that wraps a real [`fs::ReadDir`] and injects failures
/// according to a script.
struct MockReader {
    path: PathBuf,
    real: fs::ReadDir,
    actions: VecDeque<ReadAction>,
    events: Arc<Mutex<Vec<Event>>>,
}

impl Drop for MockReader {
    fn drop(&mut self) {
        self.events
            .lock()
            .unwrap()
            .push(Event::Close(self.path.clone()));
    }
}

impl Iterator for MockReader {
    type Item = io::Result<fs::DirEntry>;

    fn next(&mut self) -> Option<io::Result<fs::DirEntry>> {
        loop {
            match self.actions.pop_front() {
                Some(ReadAction::Fail(err)) => return Some(Err(err)),
                Some(ReadAction::Read) | None => match self.real.next() {
                    Some(result) => return Some(result),
                    None => return None,
                },
            }
        }
    }
}

impl DirReader for MockReader {}

impl DirOpener for MockOpener {
    fn open(&self, path: &Path) -> io::Result<Box<dyn DirReader>> {
        self.events
            .lock()
            .unwrap()
            .push(Event::Open(path.to_path_buf()));
        match self.script.lock().unwrap().pop_front() {
            Some(OpenAction::Fail(err)) => Err(err),
            Some(OpenAction::Succeed(script)) => {
                let real = fs::read_dir(&script.real_dir)?;
                Ok(Box::new(MockReader {
                    path: path.to_path_buf(),
                    real,
                    actions: script.actions,
                    events: self.events.clone(),
                }))
            }
            None => {
                let real = fs::read_dir(path)?;
                Ok(Box::new(MockReader {
                    path: path.to_path_buf(),
                    real,
                    actions: VecDeque::new(),
                    events: self.events.clone(),
                }))
            }
        }
    }
}

/// Create a mock opener with the given script.
fn mock_opener(
    script: VecDeque<OpenAction>,
) -> (Arc<MockOpener>, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let opener = Arc::new(MockOpener {
        events: events.clone(),
        script: Mutex::new(script),
    });
    (opener, events)
}

/// Create an `io::Error` with the given message.
fn io_err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg.to_string())
}

#[test]
fn retry_first_read_failure_then_success_delivers_a_once() {
    let dir = Dir::tmp();
    dir.touch_all(&["a", "b", "c"]);

    let (opener, _events) = mock_opener(VecDeque::from(vec![
        // First open: the reader fails on its first read.
        OpenAction::Succeed(ReaderScript {
            real_dir: dir.path().to_path_buf(),
            actions: VecDeque::from(vec![ReadAction::Fail(io_err(
                "injected first-read failure",
            ))]),
        }),
        // Second open (retry): the reader reads normally.
        OpenAction::Succeed(ReaderScript {
            real_dir: dir.path().to_path_buf(),
            actions: VecDeque::new(),
        }),
    ]));

    let wd = WalkDir::new(dir.path())
        .retry_on_error(3)
        .with_opener(opener);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // `a` should be delivered exactly once (no duplicates from the retry).
    let paths = r.sorted_paths();
    assert_eq!(paths.len(), 4); // root + a + b + c
    let a_count = paths.iter().filter(|p| p.ends_with("a")).count();
    assert_eq!(a_count, 1, "a should be delivered exactly once");
}

#[test]
fn read_a_then_failure_does_not_reopen() {
    let dir = Dir::tmp();
    dir.touch_all(&["a", "b", "c"]);

    let (opener, events) = mock_opener(VecDeque::from(vec![
        OpenAction::Succeed(ReaderScript {
            real_dir: dir.path().to_path_buf(),
            actions: VecDeque::from(vec![
                // Read `a` successfully.
                ReadAction::Read,
                // Then fail.
                ReadAction::Fail(io_err("injected failure after reading a")),
            ]),
        }),
    ]));

    let wd = WalkDir::new(dir.path())
        .retry_on_error(3)
        .with_opener(opener);
    let r = dir.run_recursive(wd);

    // One error, no retry.
    assert_eq!(r.errs().len(), 1);
    let err = &r.errs()[0];
    assert_eq!(err.stage(), Some(ErrorStage::Read));
    assert!(
        !err.is_retryable(),
        "error after reading a is non-retryable"
    );

    // `a` was delivered (read_any is true).
    let paths = r.sorted_paths();
    assert!(paths.iter().any(|p| p.ends_with("a")));

    // Only one open (no retry).
    let evs = events.lock().unwrap();
    let opens = evs
        .iter()
        .filter(|e| matches!(e, Event::Open(_)))
        .count();
    assert_eq!(opens, 1, "should not reopen after reading a");
}

#[test]
fn close_precedes_next_open() {
    let dir = Dir::tmp();
    dir.touch_all(&["a", "b"]);

    let (opener, events) = mock_opener(VecDeque::from(vec![
        // First open: fail on first read.
        OpenAction::Succeed(ReaderScript {
            real_dir: dir.path().to_path_buf(),
            actions: VecDeque::from(vec![ReadAction::Fail(io_err("injected"))]),
        }),
        // Second open: fail on first read.
        OpenAction::Succeed(ReaderScript {
            real_dir: dir.path().to_path_buf(),
            actions: VecDeque::from(vec![ReadAction::Fail(io_err("injected"))]),
        }),
        // Third open: succeed.
        OpenAction::Succeed(ReaderScript {
            real_dir: dir.path().to_path_buf(),
            actions: VecDeque::new(),
        }),
    ]));

    let wd = WalkDir::new(dir.path())
        .retry_on_error(5)
        .with_opener(opener);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // Assert that each Close precedes the next Open.
    let evs = events.lock().unwrap();
    let mut last_close: Option<usize> = None;
    for (i, ev) in evs.iter().enumerate() {
        match ev {
            Event::Open(_) => {
                if let Some(close_idx) = last_close {
                    assert!(
                        close_idx < i,
                        "close (index {}) must precede next open (index {})",
                        close_idx,
                        i
                    );
                }
            }
            Event::Close(_) => {
                last_close = Some(i);
            }
        }
    }

    // Three opens and three closes (the last close is at end of iteration).
    let opens = evs
        .iter()
        .filter(|e| matches!(e, Event::Open(_)))
        .count();
    let closes = evs
        .iter()
        .filter(|e| matches!(e, Event::Close(_)))
        .count();
    assert_eq!(opens, 3);
    assert_eq!(closes, 3);
}

#[test]
fn filtered_items_count_as_read() {
    let dir = Dir::tmp();
    dir.touch_all(&["a", "b", "c"]);

    let (opener, events) = mock_opener(VecDeque::from(vec![
        OpenAction::Succeed(ReaderScript {
            real_dir: dir.path().to_path_buf(),
            actions: VecDeque::from(vec![
                // Read `a` (it will be filtered out).
                ReadAction::Read,
                // Then fail.
                ReadAction::Fail(io_err("injected after filtered read")),
            ]),
        }),
    ]));

    // Filter out `a`.
    let wd = WalkDir::new(dir.path())
        .retry_on_error(3)
        .with_opener(opener)
        .into_iter()
        .filter_entry(|e| e.file_name() != "a");
    let r = dir.run_recursive(wd);

    // One error, no retry (a was read, even though filtered).
    assert_eq!(r.errs().len(), 1);
    let err = &r.errs()[0];
    assert_eq!(err.stage(), Some(ErrorStage::Read));
    assert!(
        !err.is_retryable(),
        "filtered item still counts as read"
    );

    // Only one open (no retry).
    let evs = events.lock().unwrap();
    let opens = evs
        .iter()
        .filter(|e| matches!(e, Event::Open(_)))
        .count();
    assert_eq!(opens, 1, "should not reopen after reading filtered item");
}

#[test]
fn retry_open_failure_then_success() {
    let dir = Dir::tmp();
    dir.touch_all(&["a", "b"]);

    let (opener, _events) = mock_opener(VecDeque::from(vec![
        // First open fails.
        OpenAction::Fail(io_err("injected open failure")),
    ]));

    let wd = WalkDir::new(dir.path())
        .retry_on_error(3)
        .with_opener(opener);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // `a` delivered once after the successful reopen.
    let paths = r.sorted_paths();
    let a_count = paths.iter().filter(|p| p.ends_with("a")).count();
    assert_eq!(a_count, 1);
}

#[test]
fn default_no_retry_yields_error() {
    let dir = Dir::tmp();
    dir.touch_all(&["a"]);

    let (opener, _events) = mock_opener(VecDeque::from(vec![
        OpenAction::Succeed(ReaderScript {
            real_dir: dir.path().to_path_buf(),
            actions: VecDeque::from(vec![ReadAction::Fail(io_err(
                "injected",
            ))]),
        }),
    ]));

    // Default: no retry. The error is yielded as before.
    let wd = WalkDir::new(dir.path()).with_opener(opener);
    let r = dir.run_recursive(wd);

    assert_eq!(r.errs().len(), 1);
    let err = &r.errs()[0];
    assert_eq!(err.stage(), Some(ErrorStage::Read));
    // The error is retryable in nature (no entry read), but no retry was
    // attempted because the default policy is no retries.
    assert!(err.is_retryable());
}

#[test]
fn retry_exhausted_yields_error() {
    let dir = Dir::tmp();
    dir.touch_all(&["a"]);

    let (opener, _events) = mock_opener(VecDeque::from(vec![
        // Every open fails.
        OpenAction::Fail(io_err("injected open failure")),
        OpenAction::Fail(io_err("injected open failure")),
        OpenAction::Fail(io_err("injected open failure")),
        OpenAction::Fail(io_err("injected open failure")),
    ]));

    let wd = WalkDir::new(dir.path())
        .retry_on_error(2)
        .with_opener(opener);
    let r = dir.run_recursive(wd);

    // One error after retries are exhausted.
    assert_eq!(r.errs().len(), 1);
    let err = &r.errs()[0];
    assert_eq!(err.stage(), Some(ErrorStage::Open));
    assert!(err.is_retryable());
}

#[test]
fn other_directories_not_rescanned() {
    let dir = Dir::tmp();
    dir.mkdirp("foo");
    dir.mkdirp("bar");
    dir.touch_all(&["foo/a", "bar/b"]);

    let (opener, events) = mock_opener(VecDeque::from(vec![
        // foo: first read fails, then succeeds (retry).
        OpenAction::Succeed(ReaderScript {
            real_dir: dir.join("foo"),
            actions: VecDeque::from(vec![ReadAction::Fail(io_err(
                "injected foo failure",
            ))]),
        }),
    ]));

    let wd = WalkDir::new(dir.path())
        .retry_on_error(3)
        .with_opener(opener);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // `a` and `b` each delivered once.
    let paths = r.sorted_paths();
    assert_eq!(paths.iter().filter(|p| p.ends_with("a")).count(), 1);
    assert_eq!(paths.iter().filter(|p| p.ends_with("b")).count(), 1);

    // `bar` should be opened exactly once (not rescanned).
    let evs = events.lock().unwrap();
    let bar_opens = evs
        .iter()
        .filter(|e| matches!(e, Event::Open(p) if p.ends_with("bar")))
        .count();
    assert_eq!(bar_opens, 1, "bar should not be rescanned");
}
