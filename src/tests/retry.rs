use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::tests::util::Dir;
use crate::{DirEntryStream, DirEnumerator, ErrorStage, WalkDir};

/// A record of handle lifecycle events, used to assert that an old
/// enumeration handle is closed before its directory is reopened.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Open(PathBuf),
    Close(PathBuf),
}

/// One step in a scripted directory stream.
#[derive(Debug, Clone)]
enum Step {
    /// Yield this entry path.
    Entry(PathBuf),
    /// Fail the enumeration with an IO error.
    Fail,
}

/// One scripted attempt at opening a directory.
#[derive(Debug, Clone)]
enum Attempt {
    /// The open itself fails.
    OpenFail,
    /// The open succeeds and produces the scripted stream.
    Stream(Vec<Step>),
}

/// A scripted directory enumerator used to deterministically inject
/// enumeration failures.
#[derive(Clone)]
struct MockEnumerator {
    scripts: Arc<Mutex<HashMap<PathBuf, VecDeque<Attempt>>>>,
    events: Arc<Mutex<Vec<Event>>>,
    live: Arc<Mutex<usize>>,
    max_live: Arc<Mutex<usize>>,
}

struct MockStream {
    path: PathBuf,
    steps: std::vec::IntoIter<Step>,
    mock: MockEnumerator,
}

impl DirEntryStream for MockStream {
    fn next_entry(&mut self) -> Option<io::Result<PathBuf>> {
        match self.steps.next() {
            None => None,
            Some(Step::Entry(path)) => Some(Ok(path)),
            Some(Step::Fail) => Some(Err(io::Error::new(
                io::ErrorKind::Other,
                "injected read failure",
            ))),
        }
    }
}

impl Drop for MockStream {
    fn drop(&mut self) {
        self.mock.record(Event::Close(self.path.clone()));
        *self.mock.live.lock().unwrap() -= 1;
    }
}

impl DirEnumerator for MockEnumerator {
    fn open(&self, path: &Path) -> io::Result<Box<dyn DirEntryStream>> {
        self.record(Event::Open(path.to_path_buf()));
        let attempt = {
            let mut scripts = self.scripts.lock().unwrap();
            let queue = scripts
                .get_mut(path)
                .unwrap_or_else(|| panic!("no script for {}", path.display()));
            queue.pop_front().unwrap_or_else(|| {
                panic!("unexpected extra open of {}", path.display())
            })
        };
        match attempt {
            Attempt::OpenFail => Err(io::Error::new(
                io::ErrorKind::Other,
                "injected open failure",
            )),
            Attempt::Stream(steps) => {
                {
                    let mut live = self.live.lock().unwrap();
                    *live += 1;
                    let mut max_live = self.max_live.lock().unwrap();
                    if *live > *max_live {
                        *max_live = *live;
                    }
                }
                Ok(Box::new(MockStream {
                    path: path.to_path_buf(),
                    steps: steps.into_iter(),
                    mock: self.clone(),
                }))
            }
        }
    }
}

impl MockEnumerator {
    fn new(scripts: Vec<(PathBuf, Vec<Attempt>)>) -> MockEnumerator {
        let scripts = scripts
            .into_iter()
            .map(|(path, attempts)| (path, attempts.into_iter().collect()))
            .collect();
        MockEnumerator {
            scripts: Arc::new(Mutex::new(scripts)),
            events: Arc::new(Mutex::new(vec![])),
            live: Arc::new(Mutex::new(0)),
            max_live: Arc::new(Mutex::new(0)),
        }
    }

    fn record(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }

    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    fn opens_of(&self, path: &Path) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == Event::Open(path.to_path_buf()))
            .count()
    }

    fn max_live(&self) -> usize {
        *self.max_live.lock().unwrap()
    }
}

/// The first read fails before any entry is produced, the retry succeeds and
/// the entry is delivered exactly once.
#[test]
fn retry_after_first_read_failure_delivers_entry_once() {
    let dir = Dir::tmp();
    dir.touch("a");
    let root = dir.path().to_path_buf();
    let a = dir.join("a");

    let mock = MockEnumerator::new(vec![(
        root.clone(),
        vec![
            Attempt::Stream(vec![Step::Fail]),
            Attempt::Stream(vec![Step::Entry(a.clone())]),
        ],
    )]);
    let wd = WalkDir::new(dir.path())
        .max_dir_retries(1)
        .dir_enumerator(mock.clone());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // `a` is delivered exactly once.
    assert_eq!(vec![root.clone(), a], r.paths());
    // The failed handle was closed before the directory was reopened.
    assert_eq!(
        vec![
            Event::Open(root.clone()),
            Event::Close(root.clone()),
            Event::Open(root.clone()),
            Event::Close(root.clone()),
        ],
        mock.events()
    );
    // At most one handle was ever open at a time.
    assert_eq!(1, mock.max_live());
}

/// Failure example 1: an error that occurs after an entry has already been
/// read from the directory is not retryable, so the directory is not
/// reopened and no entry can be delivered twice.
#[test]
fn error_after_partial_read_is_not_retryable() {
    let dir = Dir::tmp();
    dir.touch("a");
    dir.touch("b");
    let root = dir.path().to_path_buf();
    let a = dir.join("a");

    let mock = MockEnumerator::new(vec![(
        root.clone(),
        // `b` comes after the failure: it may only be delivered from the
        // same handle (iteration continues after the error, as with
        // `std::fs::ReadDir`), never from a reopened one.
        vec![Attempt::Stream(vec![
            Step::Entry(a.clone()),
            Step::Fail,
            Step::Entry(dir.join("b")),
        ])],
    )]);
    let wd = WalkDir::new(dir.path())
        .max_dir_retries(3)
        .dir_enumerator(mock.clone());
    let r = dir.run_recursive(wd);

    assert_eq!(vec![root.clone(), a, dir.join("b")], r.paths());
    assert_eq!(1, r.errs().len());
    let err = &r.errs()[0];
    assert_eq!(Some(ErrorStage::ReadDir), err.stage());
    assert!(!err.is_retryable());
    assert_eq!(Some(root.as_path()), err.path());
    // The directory was never reopened.
    assert_eq!(1, mock.opens_of(&root));
}

/// Failure example 2: an entry that is read but then filtered out still
/// counts as read, so a subsequent failure is not retryable.
#[test]
fn filtered_entry_counts_as_read() {
    let dir = Dir::tmp();
    dir.touch("a");
    let root = dir.path().to_path_buf();
    let a = dir.join("a");

    let mock = MockEnumerator::new(vec![(
        root.clone(),
        vec![Attempt::Stream(vec![Step::Entry(a), Step::Fail])],
    )]);
    let it = WalkDir::new(dir.path())
        .max_dir_retries(3)
        .dir_enumerator(mock.clone())
        .into_iter()
        .filter_entry(|e| e.file_name() != "a");
    let r = dir.run_recursive(it);

    // `a` was read (and filtered out) before the failure, so the failure
    // must not be retried.
    assert_eq!(vec![root.clone()], r.paths());
    assert_eq!(1, r.errs().len());
    let err = &r.errs()[0];
    assert_eq!(Some(ErrorStage::ReadDir), err.stage());
    assert!(!err.is_retryable());
    assert_eq!(Some(root.as_path()), err.path());
    assert_eq!(1, mock.opens_of(&root));
}

/// A failed open is retried and the retry can succeed.
#[test]
fn open_failure_is_retried_and_can_succeed() {
    let dir = Dir::tmp();
    dir.touch("a");
    let root = dir.path().to_path_buf();
    let a = dir.join("a");

    let mock = MockEnumerator::new(vec![(
        root.clone(),
        vec![Attempt::OpenFail, Attempt::Stream(vec![Step::Entry(a.clone())])],
    )]);
    let wd = WalkDir::new(dir.path())
        .max_dir_retries(1)
        .dir_enumerator(mock.clone());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(vec![root.clone(), a], r.paths());
    assert_eq!(2, mock.opens_of(&root));
}

/// The retry budget is finite: once exhausted, the error is yielded with its
/// path and stage intact.
#[test]
fn retry_budget_exhausted_yields_error_with_stage_and_path() {
    let dir = Dir::tmp();
    let root = dir.path().to_path_buf();

    let mock = MockEnumerator::new(vec![(
        root.clone(),
        vec![Attempt::OpenFail, Attempt::OpenFail, Attempt::OpenFail],
    )]);
    let wd = WalkDir::new(dir.path())
        .max_dir_retries(2)
        .dir_enumerator(mock.clone());
    let r = dir.run_recursive(wd);

    assert_eq!(vec![root.clone()], r.paths());
    assert_eq!(1, r.errs().len());
    let err = &r.errs()[0];
    assert_eq!(Some(ErrorStage::OpenDir), err.stage());
    // Open failures are always safe to retry; the budget just ran out.
    assert!(err.is_retryable());
    assert_eq!(Some(root.as_path()), err.path());
    // One initial attempt plus exactly two retries.
    assert_eq!(3, mock.opens_of(&root));
}

/// Only the failed directory is reopened; every other directory is opened
/// exactly once.
#[test]
fn other_directories_are_not_rescanned() {
    let dir = Dir::tmp();
    dir.mkdirp("d1");
    dir.mkdirp("d2");
    dir.touch("d1/x");
    dir.touch("d2/y");
    let root = dir.path().to_path_buf();
    let d1 = dir.join("d1");
    let d2 = dir.join("d2");
    let x = dir.join("d1/x");
    let y = dir.join("d2/y");

    let mock = MockEnumerator::new(vec![
        (
            root.clone(),
            vec![Attempt::Stream(vec![
                Step::Entry(d1.clone()),
                Step::Entry(d2.clone()),
            ])],
        ),
        (
            d1.clone(),
            vec![
                Attempt::Stream(vec![Step::Fail]),
                Attempt::Stream(vec![Step::Entry(x.clone())]),
            ],
        ),
        (d2.clone(), vec![Attempt::Stream(vec![Step::Entry(y.clone())])]),
    ]);
    let wd = WalkDir::new(dir.path())
        .max_dir_retries(1)
        .dir_enumerator(mock.clone());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(vec![root.clone(), d1.clone(), x, d2.clone(), y], r.paths());
    // Only the failed directory was reopened.
    assert_eq!(1, mock.opens_of(&root));
    assert_eq!(2, mock.opens_of(&d1));
    assert_eq!(1, mock.opens_of(&d2));
}

/// Without an explicit retry policy, the default iterator behavior is
/// unchanged: the error is yielded once and no retry is attempted.
#[test]
fn default_behavior_yields_error_once_without_retry() {
    let dir = Dir::tmp();
    let root = dir.path().to_path_buf();

    let mock = MockEnumerator::new(vec![(
        root.clone(),
        vec![Attempt::Stream(vec![Step::Fail])],
    )]);
    let wd = WalkDir::new(dir.path()).dir_enumerator(mock.clone());
    let r = dir.run_recursive(wd);

    assert_eq!(vec![root.clone()], r.paths());
    assert_eq!(1, r.errs().len());
    let err = &r.errs()[0];
    assert_eq!(Some(ErrorStage::ReadDir), err.stage());
    assert_eq!(Some(root.as_path()), err.path());
    // The error is safe to retry, but the default policy never retries.
    assert!(err.is_retryable());
    assert_eq!(1, mock.opens_of(&root));
}
