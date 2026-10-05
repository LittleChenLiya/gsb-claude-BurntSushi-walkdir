//! Explicit event-based directory walking.
//!
//! This module provides [`WalkDir::walk_events`], an alternative to the
//! standard iterator that emits explicit [`WalkEvent`]s:
//!
//! * [`WalkEvent::Enter`] — emitted when the walker enters a directory,
//!   before its contents are read.
//! * [`WalkEvent::Entry`] — emitted for each non-directory entry (files,
//!   symlinks, etc.).
//! * [`WalkEvent::Leave`] — emitted when the walker leaves a directory, with
//!   a [`LeaveReason`] describing why the directory walk ended.
//!
//! Unlike the depth-based [`Iterator`] implementation, every `Enter` is
//! guaranteed to be matched by a corresponding `Leave` (except when the
//! walker is dropped mid-traversal, in which case open directories are
//! marked [`LeaveReason::Incomplete`]). This makes it straightforward to
//! track the set of currently open directories without guessing from depth
//! changes.
//!
//! # Event graph
//!
//! A normal (exhausted) walk of a small tree produces events in this order:
//!
//! ```text
//! Enter(root)
//! ├── Entry(root/a.txt)
//! ├── Enter(root/sub)
//! │   ├── Entry(root/sub/b.txt)
//! │   └── Leave(root/sub, Exhausted)
//! └── Leave(root, Exhausted)
//! ```
//!
//! The callback can influence the walk by returning a [`WalkControl`]:
//!
//! ```text
//! Enter(root) ──Continue──► Entry(root/a.txt)
//!                           │
//!                           └──Continue──► Enter(root/sub)
//!                                           │
//!                                           ├──SkipDir──► Leave(root/sub, Skipped)
//!                                           │
//!                                           └──Cancel──► Leave(root/sub, Cancelled)
//!                                                         │
//!                                                         └──► Leave(root, Cancelled)
//! ```
//!
//! If the callback panics (or the walker is otherwise dropped) while
//! directories are still open, those directories receive
//! [`LeaveReason::Incomplete`] during the drop. No additional I/O is
//! performed:
//!
//! ```text
//! Enter(root) ──panic──► [drop] ──► Leave(root, Incomplete)
//! ```
//!
//! # Leave reasons
//!
//! | Reason | When |
//! |---|---|
//! | [`Exhausted`](LeaveReason::Exhausted) | All entries were read normally. |
//! | [`Error`](LeaveReason::Error) | The directory could not be opened or an I/O error occurred while reading entries. |
//! | [`Skipped`](LeaveReason::Skipped) | The callback returned [`WalkControl::SkipDir`] on the `Enter` event. |
//! | [`Cancelled`](LeaveReason::Cancelled) | The callback returned [`WalkControl::Cancel`]; all open directories are cancelled. |
//! | [`Incomplete`](LeaveReason::Incomplete) | The walker was dropped while the directory was still open (e.g. callback panicked). |
//!
//! # Running the tests
//!
//! The event-based walker has targeted tests. Run them with:
//!
//! ```sh
//! cargo test --lib events
//! ```
//!
//! To run a specific test:
//!
//! ```sh
//! cargo test --lib events_nested_tree
//! cargo test --lib events_cancel_leaves_cancelled
//! cargo test --lib events_drop_emits_incomplete
//! ```
//!
//! To run the `read_dir` failure test (Unix only):
//!
//! ```sh
//! cargo test --lib events_read_dir_failure_still_leaves
//! ```
//!
//! [`Iterator`]: std::iter::Iterator

use std::cmp::Ordering;
use std::fmt;
use std::fs::{self, ReadDir};
use std::io;
use std::path::{Path, PathBuf};
use std::panic::{self, AssertUnwindSafe};
use std::vec;

use same_file::Handle;

use crate::dent::DirEntry;
use crate::error::Error;
use crate::{util, WalkDir, WalkDirOptions};

/// An event emitted while recursively walking a directory.
///
/// Every [`Enter`] event is matched by exactly one [`Leave`] event (unless
/// the walker is dropped mid-traversal). [`Entry`] events are emitted for
/// non-directory entries only.
///
/// [`Enter`]: WalkEvent::Enter
/// [`Entry`]: WalkEvent::Entry
/// [`Leave`]: WalkEvent::Leave
#[derive(Debug)]
pub enum WalkEvent {
    /// The walker is entering a directory. The directory's contents have
    /// not yet been read.
    Enter(DirEntry),
    /// A non-directory entry (a file, symlink, etc.) was found.
    Entry(DirEntry),
    /// The walker is leaving a directory. The [`LeaveReason`] describes why
    /// the directory walk ended.
    Leave(DirEntry, LeaveReason),
}

impl WalkEvent {
    /// Returns the directory entry associated with this event.
    pub fn entry(&self) -> &DirEntry {
        match self {
            WalkEvent::Enter(d) => d,
            WalkEvent::Entry(d) => d,
            WalkEvent::Leave(d, _) => d,
        }
    }

    /// Returns `true` if this is an [`Enter`](WalkEvent::Enter) event.
    pub fn is_enter(&self) -> bool {
        matches!(self, WalkEvent::Enter(_))
    }

    /// Returns `true` if this is an [`Entry`](WalkEvent::Entry) event.
    pub fn is_entry(&self) -> bool {
        matches!(self, WalkEvent::Entry(_))
    }

    /// Returns `true` if this is a [`Leave`](WalkEvent::Leave) event.
    pub fn is_leave(&self) -> bool {
        matches!(self, WalkEvent::Leave(_, _))
    }
}

/// The reason a directory walk was left.
///
/// Every directory that is entered is eventually left with one of these
/// reasons.
#[derive(Debug)]
pub enum LeaveReason {
    /// All entries in the directory were read and processed normally.
    Exhausted,
    /// The directory could not be opened or an error occurred while reading
    /// its entries.
    Error(Error),
    /// The directory was skipped by the callback (via
    /// [`WalkControl::SkipDir`]); its contents were not read.
    Skipped,
    /// The walk was cancelled (via [`WalkControl::Cancel`]) before the
    /// directory finished.
    Cancelled,
    /// The walker was dropped while the directory was still open. No
    /// additional I/O was performed during the drop.
    Incomplete,
}

impl LeaveReason {
    /// Returns `true` if the directory was left normally (i.e. it was fully
    /// exhausted).
    pub fn is_exhausted(&self) -> bool {
        matches!(self, LeaveReason::Exhausted)
    }

    /// Returns `true` if the directory was left due to an error.
    pub fn is_error(&self) -> bool {
        matches!(self, LeaveReason::Error(_))
    }

    /// Returns the error if the leave was due to an error.
    pub fn error(&self) -> Option<&Error> {
        match self {
            LeaveReason::Error(e) => Some(e),
            _ => None,
        }
    }
}

/// A control signal returned by the event callback.
///
/// This allows the callback to influence the walk: skip the current
/// directory, cancel the walk entirely, or continue normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkControl {
    /// Continue the walk normally.
    Continue,
    /// Skip the current directory. Only meaningful when returned for an
    /// [`Enter`](WalkEvent::Enter) event; otherwise treated as
    /// [`Continue`](WalkControl::Continue).
    SkipDir,
    /// Cancel the walk. All directories that have been entered but not yet
    /// left will receive a [`LeaveReason::Cancelled`] event.
    Cancel,
}

/// The state of a single directory frame in the walk stack.
enum FrameState {
    /// The directory was successfully opened and is being read.
    Open(DirIter),
    /// The directory could not be opened.
    Failed(Error),
    /// The directory was skipped by the callback.
    Skipped,
    /// The directory was not descended into (e.g. due to a file system
    /// boundary). It is left immediately without reading contents.
    NotDescended,
    /// The walk was cancelled while this directory was open.
    Cancelled,
}

impl fmt::Debug for FrameState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameState::Open(_) => write!(f, "Open(..)"),
            FrameState::Failed(e) => write!(f, "Failed({:?})", e),
            FrameState::Skipped => write!(f, "Skipped"),
            FrameState::NotDescended => write!(f, "NotDescended"),
            FrameState::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// An iterator over a directory's entries, supporting both raw `ReadDir`
/// and sorted collection.
enum DirIter {
    /// A raw directory handle.
    ReadDir { depth: usize, rd: ReadDir },
    /// Collected and sorted entries.
    Sorted(vec::IntoIter<Result<DirEntry, Error>>),
}

impl DirIter {
    fn next(&mut self) -> Option<Result<DirEntry, Error>> {
        match self {
            DirIter::ReadDir { depth, rd } => {
                rd.next().map(|r| match r {
                    Ok(r) => DirEntry::from_entry(*depth, &r),
                    Err(err) => Err(Error::from_io(*depth, err)),
                })
            }
            DirIter::Sorted(it) => it.next(),
        }
    }
}

/// A single directory frame in the walk stack.
#[derive(Debug)]
struct DirFrame {
    /// The directory entry.
    entry: DirEntry,
    /// The state of the directory.
    state: FrameState,
}

/// An ancestor is an item in the directory tree traversed by the walker, and
/// is used to check for loops when following symbolic links.
#[derive(Debug)]
struct Ancestor {
    path: PathBuf,
    #[cfg(windows)]
    handle: Handle,
}

impl Ancestor {
    #[cfg(windows)]
    fn new(dent: &DirEntry) -> io::Result<Ancestor> {
        let handle = Handle::from_path(dent.path())?;
        Ok(Ancestor { path: dent.path().to_path_buf(), handle })
    }

    #[cfg(not(windows))]
    fn new(dent: &DirEntry) -> io::Result<Ancestor> {
        Ok(Ancestor { path: dent.path().to_path_buf() })
    }

    #[cfg(windows)]
    fn is_same(&self, child: &Handle) -> io::Result<bool> {
        Ok(child == &self.handle)
    }

    #[cfg(not(windows))]
    fn is_same(&self, child: &Handle) -> io::Result<bool> {
        Ok(child == &Handle::from_path(&self.path)?)
    }
}

/// The internal state of the event walker.
struct EventWalker<F>
where
    F: FnMut(WalkEvent) -> WalkControl,
{
    opts: WalkDirOptions,
    root: Option<PathBuf>,
    root_device: Option<u64>,
    stack: Vec<DirFrame>,
    stack_path: Vec<Ancestor>,
    callback: Option<F>,
    finished: bool,
    last_was_enter: bool,
}

impl<F> EventWalker<F>
where
    F: FnMut(WalkEvent) -> WalkControl,
{
    fn new(walk_dir: WalkDir, callback: F) -> EventWalker<F> {
        EventWalker {
            opts: walk_dir.opts,
            root: Some(walk_dir.root),
            root_device: None,
            stack: vec![],
            stack_path: vec![],
            callback: Some(callback),
            finished: false,
            last_was_enter: false,
        }
    }

    /// Drive the walk to completion.
    fn run(mut self) {
        while let Some(event) = self.next_event() {
            self.last_was_enter = event.is_enter();
            match self.invoke_callback(event) {
                WalkControl::Continue => {}
                WalkControl::SkipDir => {
                    if self.last_was_enter {
                        self.skip_current_dir();
                    }
                }
                WalkControl::Cancel => {
                    self.cancel();
                    while let Some(event) = self.next_event() {
                        self.invoke_callback(event);
                    }
                    return;
                }
            }
        }
    }

    /// Invoke the callback, catching panics so that the Drop impl can emit
    /// Incomplete events for any still-open directories.
    fn invoke_callback(&mut self, event: WalkEvent) -> WalkControl {
        let cb = self.callback.as_mut().expect("callback should be set");
        match panic::catch_unwind(AssertUnwindSafe(|| cb(event))) {
            Ok(control) => control,
            Err(payload) => {
                // Don't mark the walk as finished: the Drop impl will emit
                // Incomplete events for any still-open directories.
                panic::resume_unwind(payload);
            }
        }
    }

    /// Advance the walk and return the next event.
    fn next_event(&mut self) -> Option<WalkEvent> {
        if let Some(root) = self.root.take() {
            return self.start_root(root);
        }

        loop {
            if self.stack.is_empty() {
                self.finished = true;
                return None;
            }

            let action = {
                let frame = self.stack.last().expect("stack should be non-empty");
                match frame.state {
                    FrameState::Open(_) => FrameAction::ReadEntry,
                    FrameState::Failed(_) => FrameAction::LeaveError,
                    FrameState::Skipped => FrameAction::LeaveSkipped,
                    FrameState::NotDescended => FrameAction::LeaveExhausted,
                    FrameState::Cancelled => FrameAction::LeaveCancelled,
                }
            };

            match action {
                FrameAction::LeaveSkipped => {
                    let frame = self.stack.pop().expect("stack should be non-empty");
                    self.pop_ancestor();
                    return Some(WalkEvent::Leave(
                        frame.entry,
                        LeaveReason::Skipped,
                    ));
                }
                FrameAction::LeaveError => {
                    let frame = self.stack.pop().expect("stack should be non-empty");
                    self.pop_ancestor();
                    let err = match frame.state {
                        FrameState::Failed(e) => e,
                        _ => unreachable!(),
                    };
                    return Some(WalkEvent::Leave(
                        frame.entry,
                        LeaveReason::Error(err),
                    ));
                }
                FrameAction::LeaveExhausted => {
                    let frame = self.stack.pop().expect("stack should be non-empty");
                    self.pop_ancestor();
                    return Some(WalkEvent::Leave(
                        frame.entry,
                        LeaveReason::Exhausted,
                    ));
                }
                FrameAction::LeaveCancelled => {
                    let frame = self.stack.pop().expect("stack should be non-empty");
                    self.pop_ancestor();
                    return Some(WalkEvent::Leave(
                        frame.entry,
                        LeaveReason::Cancelled,
                    ));
                }
                FrameAction::ReadEntry => {
                    let dent = {
                        let frame =
                            self.stack.last_mut().expect("stack should be non-empty");
                        match frame.state {
                            FrameState::Open(ref mut it) => it.next(),
                            _ => unreachable!(),
                        }
                    };
                    match dent {
                        None => {
                            let frame =
                                self.stack.pop().expect("stack should be non-empty");
                            self.pop_ancestor();
                            return Some(WalkEvent::Leave(
                                frame.entry,
                                LeaveReason::Exhausted,
                            ));
                        }
                        Some(Err(err)) => {
                            let frame =
                                self.stack.pop().expect("stack should be non-empty");
                            self.pop_ancestor();
                            return Some(WalkEvent::Leave(
                                frame.entry,
                                LeaveReason::Error(err),
                            ));
                        }
                        Some(Ok(dent)) => {
                            if let Some(event) = self.handle_entry(dent) {
                                return Some(event);
                            }
                            // The entry was skipped (e.g. due to depth
                            // settings or a broken symlink). Continue
                            // reading the next entry.
                        }
                    }
                }
            }
        }
    }

    /// Process the root path.
    fn start_root(&mut self, root: PathBuf) -> Option<WalkEvent> {
        if self.opts.same_file_system {
            let result = util::device_num(&root)
                .map_err(|e| Error::from_path(0, root.clone(), e));
            match result {
                Ok(d) => self.root_device = Some(d),
                Err(_) => {
                    // Can't determine the root device. Don't fake an entry.
                    self.finished = true;
                    return None;
                }
            }
        }

        let dent = match DirEntry::from_path(0, root.clone(), false) {
            Ok(d) => d,
            Err(_) => {
                // The root doesn't exist or its metadata can't be read.
                // Don't fake an Enter event.
                self.finished = true;
                return None;
            }
        };

        // Handle the root directly. Unlike `handle_entry`, we always push
        // the root directory so that its children are still walked even if
        // the root itself is skipped (e.g. due to `min_depth`).
        let is_normal_dir = !dent.file_type().is_symlink() && dent.is_dir();
        if is_normal_dir {
            if dent.depth() > self.opts.max_depth {
                return None;
            }
            let frame = self.open_dir(&dent);
            self.push_ancestor(&dent);
            self.stack.push(frame);
            if self.skippable(dent.depth()) {
                // Below `min_depth`: don't emit Enter, but read children.
                return self.next_event();
            }
            return Some(WalkEvent::Enter(dent));
        } else if dent.depth() == 0
            && dent.file_type().is_symlink()
            && self.opts.follow_root_links
        {
            // Root symlink: follow it to determine if it's a directory.
            match fs::metadata(dent.path()) {
                Ok(md) => {
                    if md.file_type().is_dir() {
                        let frame = self.open_dir(&dent);
                        self.stack.push(frame);
                        if self.skippable(dent.depth()) {
                            return self.next_event();
                        }
                        return Some(WalkEvent::Enter(dent));
                    } else {
                        if self.skippable(dent.depth()) {
                            return None;
                        }
                        return Some(WalkEvent::Entry(dent));
                    }
                }
                Err(_) => {
                    return None;
                }
            }
        }

        // Not a directory.
        if self.skippable(dent.depth()) {
            return None;
        }
        Some(WalkEvent::Entry(dent))
    }

    /// Process a single entry from a directory.
    fn handle_entry(&mut self, mut dent: DirEntry) -> Option<WalkEvent> {
        if self.opts.follow_links && dent.file_type().is_symlink() {
            dent = match self.follow(dent) {
                Ok(d) => d,
                Err(_) => {
                    // Error following a symlink. Skip this entry.
                    return None;
                }
            };
        }

        let is_normal_dir = !dent.file_type().is_symlink() && dent.is_dir();
        if is_normal_dir {
            // Don't descend beyond `max_depth`.
            if dent.depth() > self.opts.max_depth {
                return None;
            }

            if self.opts.same_file_system && dent.depth() > 0 {
                match self.is_same_file_system(&dent) {
                    Ok(true) => {}
                    Ok(false) => {
                        // Different file system. Enter but don't descend;
                        // leave immediately without reading contents.
                        let frame = DirFrame {
                            entry: dent.clone(),
                            state: FrameState::NotDescended,
                        };
                        self.stack.push(frame);
                        return Some(WalkEvent::Enter(dent));
                    }
                    Err(err) => {
                        // Error checking the file system. Enter and leave
                        // with the error.
                        let frame = DirFrame {
                            entry: dent.clone(),
                            state: FrameState::Failed(err),
                        };
                        self.stack.push(frame);
                        return Some(WalkEvent::Enter(dent));
                    }
                }
            }

            // Descend into the directory.
            let frame = self.open_dir(&dent);
            self.push_ancestor(&dent);
            self.stack.push(frame);
            if self.skippable(dent.depth()) {
                // Below `min_depth`: don't emit Enter, but children will be
                // read by the loop in `next_event`.
                return None;
            }
            return Some(WalkEvent::Enter(dent));
        } else if dent.depth() == 0
            && dent.file_type().is_symlink()
            && self.opts.follow_root_links
        {
            // Root symlink: follow it to determine if it's a directory.
            match fs::metadata(dent.path()) {
                Ok(md) => {
                    if md.file_type().is_dir() {
                        let frame = self.open_dir(&dent);
                        self.stack.push(frame);
                        return Some(WalkEvent::Enter(dent));
                    } else {
                        if self.skippable(dent.depth()) {
                            return None;
                        }
                        return Some(WalkEvent::Entry(dent));
                    }
                }
                Err(_) => {
                    return None;
                }
            }
        }

        // Not a directory.
        if self.skippable(dent.depth()) {
            return None;
        }
        Some(WalkEvent::Entry(dent))
    }

    /// Open a directory for reading.
    fn open_dir(&mut self, dent: &DirEntry) -> DirFrame {
        let depth = dent.depth() + 1;
        match fs::read_dir(dent.path()) {
            Ok(rd) => {
                let it = if let Some(ref mut cmp) = self.opts.sorter {
                    let mut entries: Vec<Result<DirEntry, Error>> = rd
                        .map(|r| match r {
                            Ok(r) => DirEntry::from_entry(depth, &r),
                            Err(err) => Err(Error::from_io(depth, err)),
                        })
                        .collect();
                    entries.sort_by(|a, b| match (a, b) {
                        (Ok(a), Ok(b)) => cmp(a, b),
                        (Err(_), Err(_)) => Ordering::Equal,
                        (Ok(_), Err(_)) => Ordering::Greater,
                        (Err(_), Ok(_)) => Ordering::Less,
                    });
                    DirIter::Sorted(entries.into_iter())
                } else {
                    DirIter::ReadDir { depth, rd }
                };
                DirFrame {
                    entry: dent.clone(),
                    state: FrameState::Open(it),
                }
            }
            Err(err) => DirFrame {
                entry: dent.clone(),
                state: FrameState::Failed(Error::from_path(
                    dent.depth(),
                    dent.path().to_path_buf(),
                    err,
                )),
            },
        }
    }

    /// Skip the current directory (the one most recently entered).
    fn skip_current_dir(&mut self) {
        if let Some(frame) = self.stack.last_mut() {
            let old = mem_replace(&mut frame.state, FrameState::Skipped);
            drop(old);
        }
    }

    /// Cancel the walk: mark all open directories as cancelled.
    fn cancel(&mut self) {
        for frame in self.stack.iter_mut() {
            let old = mem_replace(&mut frame.state, FrameState::Cancelled);
            drop(old);
        }
    }

    /// Follow a symbolic link.
    fn follow(&self, mut dent: DirEntry) -> Result<DirEntry, Error> {
        dent = DirEntry::from_path(
            self.current_depth(),
            dent.path().to_path_buf(),
            true,
        )?;
        if dent.is_dir() {
            self.check_loop(dent.path())?;
        }
        Ok(dent)
    }

    /// Check for a symbolic link loop.
    fn check_loop<P: AsRef<Path>>(&self, child: P) -> Result<(), Error> {
        let hchild = Handle::from_path(&child)
            .map_err(|err| Error::from_io(self.current_depth(), err))?;
        for ancestor in self.stack_path.iter().rev() {
            let is_same = ancestor
                .is_same(&hchild)
                .map_err(|err| Error::from_io(self.current_depth(), err))?;
            if is_same {
                return Err(Error::from_loop(
                    self.current_depth(),
                    &ancestor.path,
                    child.as_ref(),
                ));
            }
        }
        Ok(())
    }

    /// Check whether an entry at the given depth should be skipped due to
    /// min/max depth settings.
    fn skippable(&self, depth: usize) -> bool {
        depth < self.opts.min_depth || depth > self.opts.max_depth
    }

    /// Check whether the given entry is on the same file system as the root.
    fn is_same_file_system(&mut self, dent: &DirEntry) -> Result<bool, Error> {
        let dent_device = util::device_num(dent.path())
            .map_err(|err| Error::from_entry(dent, err))?;
        Ok(self
            .root_device
            .map(|d| d == dent_device)
            .expect("root device should be set"))
    }

    /// The depth at which the next entry would be generated.
    fn current_depth(&self) -> usize {
        self.stack.last().map(|f| f.entry.depth() + 1).unwrap_or(0)
    }

    fn push_ancestor(&mut self, dent: &DirEntry) {
        if self.opts.follow_links {
            if let Ok(ancestor) = Ancestor::new(dent) {
                self.stack_path.push(ancestor);
            }
        }
    }

    fn pop_ancestor(&mut self) {
        if self.opts.follow_links {
            self.stack_path.pop();
        }
    }
}

impl<F> Drop for EventWalker<F>
where
    F: FnMut(WalkEvent) -> WalkControl,
{
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        // Mark all still-open directories as incomplete. No additional I/O
        // is performed here.
        if let Some(mut cb) = self.callback.take() {
            while let Some(frame) = self.stack.pop() {
                let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                    cb(WalkEvent::Leave(
                        frame.entry,
                        LeaveReason::Incomplete,
                    ));
                }));
            }
        }
    }
}

/// The action to take for the top frame of the stack.
enum FrameAction {
    ReadEntry,
    LeaveError,
    LeaveSkipped,
    LeaveExhausted,
    LeaveCancelled,
}

/// A helper to replace a value without requiring `Default`.
fn mem_replace<T>(dest: &mut T, src: T) -> T {
    std::mem::replace(dest, src)
}

impl WalkDir {
    /// Walk the directory tree, emitting explicit [`WalkEvent`]s.
    ///
    /// This is an alternative to the standard [`IntoIter`] iterator that
    /// emits explicit enter, entry, and leave events. Every
    /// [`Enter`](WalkEvent::Enter) event is matched by exactly one
    /// [`Leave`](WalkEvent::Leave) event, making it straightforward to track
    /// the set of currently open directories.
    ///
    /// The callback returns a [`WalkControl`] to influence the walk:
    ///
    /// * [`WalkControl::Continue`] — continue normally.
    /// * [`WalkControl::SkipDir`] — skip the current directory (only
    ///   meaningful for [`Enter`](WalkEvent::Enter) events).
    /// * [`WalkControl::Cancel`] — cancel the walk; all entered but unleft
    ///   directories receive [`LeaveReason::Cancelled`].
    ///
    /// If the walker is dropped mid-traversal (e.g. the callback panics),
    /// any still-open directories receive [`LeaveReason::Incomplete`]. No
    /// additional I/O is performed during the drop.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use walkdir::{WalkDir, WalkEvent, WalkControl};
    ///
    /// WalkDir::new("foo").walk_events(|event| {
    ///     match event {
    ///         WalkEvent::Enter(dir) => {
    ///             println!("enter: {}", dir.path().display());
    ///             WalkControl::Continue
    ///         }
    ///         WalkEvent::Entry(file) => {
    ///             println!("entry: {}", file.path().display());
    ///             WalkControl::Continue
    ///         }
    ///         WalkEvent::Leave(dir, reason) => {
    ///             println!("leave: {} ({:?})", dir.path().display(), reason);
    ///             WalkControl::Continue
    ///         }
    ///     }
    /// });
    /// ```
    ///
    /// [`IntoIter`]: crate::IntoIter
    pub fn walk_events<F>(self, callback: F)
    where
        F: FnMut(WalkEvent) -> WalkControl,
    {
        let walker = EventWalker::new(self, callback);
        walker.run();
    }
}
