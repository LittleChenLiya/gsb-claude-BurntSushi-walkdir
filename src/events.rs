//! Explicit directory traversal with paired enter/leave events.
//!
//! The ordinary [`WalkDir`](crate::WalkDir) iterator flattens a traversal
//! into a sequence of [`DirEntry`](crate::DirEntry) values; a directory
//! boundary has to be inferred from the depth going back down. This module
//! provides a parallel API, [`walk_events`], that makes directory boundaries
//! explicit instead. Every traversal produces three kinds of [`Event`]:
//!
//! * [`Event::Enter`] is emitted before a directory is read, exactly once
//!   for every directory that is actually visited.
//! * [`Event::Entry`] is emitted for files, symbolic links and every other
//!   non-directory entry.
//! * [`Event::Leave`] is emitted after a directory has been finished, exactly
//!   once for every preceding [`Event::Enter`]. The attached [`LeaveReason`]
//!   says *why* the directory was left: it was drained normally, reading it
//!   failed, it was skipped, the traversal was cancelled, or it was abandoned
//!   before it was closed.
//!
//! Directory filtering and file filtering are configured independently via
//! [`WalkEvents::filter_dirs`] and [`WalkEvents::filter_files`]. A filtered
//! directory produces neither an `Enter` nor a `Leave`, and its contents are
//! never read; a filtered file only suppresses its own `Entry`.
//!
//! # Event graph
//!
//! For a tree such as:
//!
//! ```text
//! foo/
//!   a.txt
//!   bar/
//!     b.txt
//!   empty/
//! ```
//!
//! `walk_events("foo")` produces events in depth-first pre-order (the order
//! of siblings is unspecified):
//!
//! ```text
//! Enter  foo
//! Entry  foo/a.txt
//! Enter  foo/bar
//! Entry  foo/bar/b.txt
//! Leave  foo/bar            reason = Completed
//! Enter  foo/empty
//! Leave  foo/empty          reason = Completed
//! Leave  foo                reason = Completed
//! ```
//!
//! Each `Enter` has a matching `Leave`, so nesting forms properly balanced
//! pairs. Other outcomes only change the reason of the matching `Leave`:
//!
//! ```text
//! Enter  foo
//! Enter  foo/unreadable
//!                          (fs::read_dir fails -> Err is yielded here)
//! Leave  foo/unreadable    reason = ReadFailed
//! Enter  foo/skipped
//! Leave  foo/skipped       reason = Skipped      (children never read)
//! Leave  foo               reason = Completed
//! ```
//!
//! Calling [`EventIter::cancel`] while `foo/bar` is open closes every
//! directory that is still open, innermost first, without reading anything
//! else:
//!
//! ```text
//! Enter  foo
//! Enter  foo/bar
//! Leave  foo/bar           reason = Cancelled
//! Leave  foo               reason = Cancelled
//! ```
//!
//! If the root path does not exist (or its metadata cannot be read), no
//! `Enter` is fabricated: the iterator yields one `Err` and is then finished.
//! If iteration is simply abandoned by dropping the iterator midway, no
//! synthetic events are produced and no directories are opened for
//! verification; use [`EventIter::completion`] to observe that the traversal
//! was left incomplete.
//!
//! Note that symbolic links are never followed by this API: a symbolic link
//! to a directory is reported like any other non-directory entry.
//!
//! # Targeted commands
//!
//! Only the small, deterministic test suite for this API needs to be run:
//!
//! ```text
//! cargo test --lib events
//! cargo test --doc events
//! ```
//!
//! # Example
//!
//! ```no_run
//! use walkdir::{walk_events, Event};
//!
//! # fn try_main() -> walkdir::Result<()> {
//! for result in walk_events("foo") {
//!     match result? {
//!         Event::Enter { dir } => {
//!             println!("enter {}", dir.path().display());
//!         }
//!         Event::Entry { entry } => {
//!             println!("entry {}", entry.path().display());
//!         }
//!         Event::Leave { dir, reason } => {
//!             println!("leave {} ({:?})", dir.path().display(), reason);
//!         }
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::fs;
use std::iter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::dent::DirEntry;
use crate::error::Error;

/// A predicate invoked for directory entries.
type DirFilter = Box<dyn FnMut(&DirEntry) -> bool>;
/// A predicate invoked for non-directory entries.
type FileFilter = Box<dyn FnMut(&DirEntry) -> bool>;

/// Start an explicit, event based traversal of `root`.
///
/// This returns a [`WalkEvents`] builder; configure it with
/// [`WalkEvents::filter_dirs`] and [`WalkEvents::filter_files`], then iterate
/// it directly (it implements [`IntoIterator`]) or finish it with
/// [`WalkEvents::build`].
///
/// See the [module documentation](self) for a description of the emitted
/// events and an event graph.
pub fn walk_events<P: AsRef<Path>>(root: P) -> WalkEvents {
    WalkEvents {
        root: root.as_ref().to_path_buf(),
        filter_dir: None,
        filter_file: None,
    }
}

/// A builder for an explicit, event based directory traversal.
///
/// Construct one with [`walk_events`]. A value can be iterated directly or
/// turned into an [`EventIter`] with [`WalkEvents::build`].
pub struct WalkEvents {
    root: PathBuf,
    filter_dir: Option<DirFilter>,
    filter_file: Option<FileFilter>,
}

impl WalkEvents {
    /// Filter directory entries independently of file entries.
    ///
    /// The predicate is invoked for every directory *before* its
    /// [`Event::Enter`] is emitted (including the root, if it is a
    /// directory). If it returns `false`, the directory is never entered:
    /// no `Enter`, no `Leave` and, most importantly, its contents are never
    /// read from the file system.
    pub fn filter_dirs<F>(mut self, predicate: F) -> Self
    where
        F: FnMut(&DirEntry) -> bool + 'static,
    {
        self.filter_dir = Some(Box::new(predicate));
        self
    }

    /// Filter non-directory entries independently of directories.
    ///
    /// The predicate is invoked for every file, symbolic link and other
    /// non-directory entry. If it returns `false`, no [`Event::Entry`] is
    /// emitted for it. Directory traversal is unaffected; use
    /// [`WalkEvents::filter_dirs`] to prune directories.
    pub fn filter_files<F>(mut self, predicate: F) -> Self
    where
        F: FnMut(&DirEntry) -> bool + 'static,
    {
        self.filter_file = Some(Box::new(predicate));
        self
    }

    /// Finish the builder and create the event iterator.
    pub fn build(self) -> EventIter {
        EventIter {
            root: Some(self.root),
            stack: vec![],
            filter_dir: self.filter_dir,
            filter_file: self.filter_file,
            done: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl IntoIterator for WalkEvents {
    type Item = crate::Result<Event>;
    type IntoIter = EventIter;

    fn into_iter(self) -> EventIter {
        self.build()
    }
}

impl fmt::Debug for WalkEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalkEvents")
            .field("root", &self.root)
            .field("filter_dir", &self.filter_dir.is_some())
            .field("filter_file", &self.filter_file.is_some())
            .finish()
    }
}

/// An iterator producing explicit traversal [`Event`]s.
///
/// Every yielded item is a [`Result<Event>`]: file system errors are
/// delivered as `Err` without disturbing the pairing of [`Event::Enter`] and
/// [`Event::Leave`].
///
/// Use [`EventIter::skip_current_dir`] to prune the directory that was
/// entered most recently, [`EventIter::cancel`] to explicitly close every
/// open directory, and [`EventIter::completion`] to check whether the
/// traversal was drained completely.
pub struct EventIter {
    /// The root path. `Some` until the first call to `next`.
    root: Option<PathBuf>,
    /// Directories that have been entered but not yet left.
    stack: Vec<Frame>,
    /// Predicate run for directories before they are entered, if any.
    filter_dir: Option<DirFilter>,
    /// Predicate run for non-directory entries, if any.
    filter_file: Option<FileFilter>,
    /// Set to `true` only once the traversal has been drained completely.
    done: Arc<AtomicBool>,
}

impl fmt::Debug for EventIter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventIter")
            .field("root", &self.root)
            .field("open_dirs", &self.stack.len())
            .field("complete", &self.is_complete())
            .finish()
    }
}

impl Iterator for EventIter {
    type Item = crate::Result<Event>;

    fn next(&mut self) -> Option<crate::Result<Event>> {
        if let Some(root) = self.root.take() {
            return self.start_root(root);
        }
        if self.stack.is_empty() {
            // Normal termination: every Enter already has its Leave.
            self.done.store(true, Ordering::Release);
            return None;
        }
        let idx = self.stack.len() - 1;
        // Take the current state temporarily so we can mutate the stack
        // (push, pop) while handling the result.
        let state =
            std::mem::replace(&mut self.stack[idx].children, Children::Done);
        match state {
            Children::Pending => self.open_dir(idx),
            Children::Read(mut rd) => match rd.next() {
                None => {
                    self.stack[idx].children = Children::Exhausted;
                    self.next()
                }
                Some(Err(err)) => {
                    // An error reading one entry does not close the
                    // directory: the error is yielded and traversal
                    // continues. Failure to open a directory is reported
                    // separately via Children::Failed.
                    let depth = self.stack[idx].dir.depth() + 1;
                    self.stack[idx].children = Children::Read(rd);
                    Some(Err(Error::from_io(depth, err)))
                }
                Some(Ok(ent)) => {
                    let depth = self.stack[idx].dir.depth() + 1;
                    let dent = match DirEntry::from_entry(depth, &ent) {
                        Ok(dent) => dent,
                        Err(err) => {
                            self.stack[idx].children = Children::Read(rd);
                            return Some(Err(err));
                        }
                    };
                    if dent.file_type().is_dir() {
                        let keep = self.keep_dir(&dent);
                        // Preserve the open handle whether we descend into
                        // this child or move on to the next sibling.
                        self.stack[idx].children = Children::Read(rd);
                        if !keep {
                            return self.next();
                        }
                        self.stack.push(Frame {
                            dir: dent.clone(),
                            children: Children::Pending,
                        });
                        Some(Ok(Event::Enter { dir: dent }))
                    } else {
                        let keep = self.keep_file(&dent);
                        self.stack[idx].children = Children::Read(rd);
                        if !keep {
                            return self.next();
                        }
                        Some(Ok(Event::Entry { entry: dent }))
                    }
                }
            },
            terminal => Some(Ok(self.leave(terminal))),
        }
    }
}

impl iter::FusedIterator for EventIter {}

impl EventIter {
    /// Skip the contents of the directory that was entered most recently.
    ///
    /// Its children are not read (if they have not been already), and the
    /// next event is its [`Event::Leave`] with reason
    /// [`LeaveReason::Skipped`]. If no directory is currently open, this is
    /// a no-op.
    pub fn skip_current_dir(&mut self) {
        if let Some(top) = self.stack.last_mut() {
            top.children = Children::Skipped;
        }
    }

    /// Cancel the traversal explicitly.
    ///
    /// Every directory that is still open is marked so that subsequent calls
    /// to [`EventIter::next`] yield its [`Event::Leave`] with reason
    /// [`LeaveReason::Cancelled`], innermost first. No unvisited contents are
    /// read and no further [`Event::Enter`] or [`Event::Entry`] events are
    /// produced.
    pub fn cancel(&mut self) {
        for frame in &mut self.stack {
            frame.children = Children::Cancelled;
        }
    }

    /// Return a cheaply cloneable handle that reports whether the traversal
    /// has finished cleanly.
    ///
    /// It becomes `true` only after [`EventIter::next`] has returned `None`,
    /// meaning every [`Event::Enter`] received its matching
    /// [`Event::Leave`] (whether completed, skipped or cancelled). If the
    /// iterator is dropped midway through a traversal, it stays `false`;
    /// those still open directories are considered
    /// [`LeaveReason::Incomplete`].
    pub fn completion(&self) -> Completion {
        Completion { done: self.done.clone() }
    }

    /// Returns `true` if the traversal was drained completely.
    ///
    /// This is equivalent to `self.completion().is_complete()`.
    pub fn is_complete(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Run the directory filter, if any, for `dent`. With no filter every
    /// directory is kept.
    fn keep_dir(&mut self, dent: &DirEntry) -> bool {
        match self.filter_dir {
            None => true,
            Some(ref mut f) => f(dent),
        }
    }

    /// Run the file filter, if any, for `dent`. With no filter every file is
    /// kept.
    fn keep_file(&mut self, dent: &DirEntry) -> bool {
        match self.filter_file {
            None => true,
            Some(ref mut f) => f(dent),
        }
    }

    /// Handle the very first call to `next`: inspect the root.
    ///
    /// A missing (or otherwise uninspectable) root produces an `Err` without
    /// fabricating an `Enter`. A directory is entered, anything else is
    /// reported as an [`Event::Entry`].
    fn start_root(&mut self, root: PathBuf) -> Option<crate::Result<Event>> {
        let dent = match DirEntry::from_path(0, root, false) {
            Ok(dent) => dent,
            Err(err) => return Some(Err(err)),
        };
        if dent.file_type().is_dir() {
            if !self.keep_dir(&dent) {
                return self.next();
            }
            self.stack.push(Frame {
                dir: dent.clone(),
                children: Children::Pending,
            });
            Some(Ok(Event::Enter { dir: dent }))
        } else {
            if !self.keep_file(&dent) {
                return self.next();
            }
            Some(Ok(Event::Entry { entry: dent }))
        }
    }

    /// Lazily open the directory at `idx` for reading.
    ///
    /// This is where an [`Event::Enter`] is followed by the actual
    /// `fs::read_dir` call, so entering never pays for a directory that is
    /// skipped or cancelled before it is read.
    fn open_dir(&mut self, idx: usize) -> Option<crate::Result<Event>> {
        let path = self.stack[idx].dir.path().to_path_buf();
        let depth = self.stack[idx].dir.depth();
        match fs::read_dir(&path) {
            Ok(rd) => {
                self.stack[idx].children = Children::Read(rd);
                self.next()
            }
            Err(err) => {
                self.stack[idx].children = Children::Failed;
                Some(Err(Error::from_path(depth, path, err)))
            }
        }
    }

    /// Pop the top directory and build its matching `Leave` event.
    fn leave(&mut self, state: Children) -> Event {
        let frame = self
            .stack
            .pop()
            .expect("BUG: cannot leave without an open directory");
        let reason = match state {
            Children::Exhausted => LeaveReason::Completed,
            Children::Failed => LeaveReason::ReadFailed,
            Children::Skipped => LeaveReason::Skipped,
            Children::Cancelled => LeaveReason::Cancelled,
            // Pending is opened before it can be popped, Read is handled by
            // `next`, and Done only exists transiently inside `next`.
            Children::Pending | Children::Read(_) | Children::Done => {
                unreachable!("BUG: directory left in a non-terminal state")
            }
        };
        Event::Leave { dir: frame.dir, reason }
    }
}

/// A traversal frame: a directory that has been entered and its child state.
struct Frame {
    /// The directory that was entered.
    dir: DirEntry,
    /// How far we have got reading its children.
    children: Children,
}

/// The state of reading one directory's children.
///
/// The open `fs::ReadDir` handle makes the `Read` variant larger than the
/// state-only variants; boxing it would only add indirection without saving
/// real memory (one frame per active directory level), so the size lint is
/// allowed here, mirroring `DirList` in the rest of the crate.
#[allow(clippy::large_enum_variant)]
enum Children {
    /// [`Event::Enter`] was emitted but the directory has not been opened
    /// yet. This is lazy on purpose: skipping or cancelling never opens it.
    Pending,
    /// The directory is open and being read.
    Read(fs::ReadDir),
    /// The directory was read until it was exhausted.
    Exhausted,
    /// Opening the directory failed.
    Failed,
    /// The directory was explicitly skipped.
    Skipped,
    /// The traversal was explicitly cancelled while this was open.
    Cancelled,
    /// A transient placeholder used while the active state is taken.
    Done,
}

/// A handle that reports whether an [`EventIter`] was drained completely.
///
/// Obtain one with [`EventIter::completion`]. It can be held onto while the
/// iterator is moved and even outlive the iterator itself.
#[derive(Clone)]
pub struct Completion {
    done: Arc<AtomicBool>,
}

impl Completion {
    /// Returns `true` exactly when the traversal finished cleanly, i.e. every
    /// emitted [`Event::Enter`] received its matching [`Event::Leave`].
    pub fn is_complete(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

impl fmt::Debug for Completion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Completion").field(&self.is_complete()).finish()
    }
}

/// Why a directory was left.
///
/// Every [`Event::Leave`] carries one of these reasons. It is the explicit
/// counterpart to an [`Event::Enter`] and never has to be inferred from
/// changes in traversal depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeaveReason {
    /// The directory's contents were read and exhausted normally.
    Completed,
    /// The directory could not be opened for reading. The matching file
    /// system error was yielded as an `Err` item just before this `Leave`.
    ReadFailed,
    /// The directory was skipped via
    /// [`EventIter::skip_current_dir`] and its contents were not read.
    Skipped,
    /// The traversal was cancelled via [`EventIter::cancel`] while this
    /// directory was still open.
    Cancelled,
    /// The iterator was dropped before this open directory was left. Its
    /// matching `Leave` is therefore never delivered; detect this through
    /// [`EventIter::completion`] returning `false`.
    Incomplete,
}

/// One event in an explicit directory traversal.
///
/// * [`Event::Enter`] and [`Event::Leave`] come in matched pairs for every
///   directory that is visited.
/// * [`Event::Entry`] is emitted once for every non-directory entry.
#[derive(Debug)]
#[non_exhaustive]
pub enum Event {
    /// A directory is about to be read. Its children (if any) follow until
    /// the matching [`Event::Leave`].
    Enter {
        /// The entered directory.
        dir: DirEntry,
    },
    /// A non-directory entry (a file, a symbolic link, and so on). Symbolic
    /// links are never followed by this API.
    Entry {
        /// The visited entry.
        entry: DirEntry,
    },
    /// A previously entered directory has been finished. The
    /// [`LeaveReason`] explains why.
    Leave {
        /// The left directory; this is the same entry that was passed to
        /// [`Event::Enter`].
        dir: DirEntry,
        /// Why the directory was left.
        reason: LeaveReason,
    },
}

impl Event {
    /// Borrow the directory entry if this is an [`Event::Enter`].
    pub fn enter(&self) -> Option<&DirEntry> {
        match *self {
            Event::Enter { ref dir } => Some(dir),
            _ => None,
        }
    }

    /// Borrow the non-directory entry if this is an [`Event::Entry`].
    pub fn as_entry(&self) -> Option<&DirEntry> {
        match *self {
            Event::Entry { ref entry } => Some(entry),
            _ => None,
        }
    }

    /// Borrow the directory entry if this is an [`Event::Leave`].
    pub fn leave(&self) -> Option<&DirEntry> {
        match *self {
            Event::Leave { ref dir, .. } => Some(dir),
            _ => None,
        }
    }

    /// Return the leave reason if this is an [`Event::Leave`].
    pub fn leave_reason(&self) -> Option<LeaveReason> {
        match *self {
            Event::Leave { reason, .. } => Some(reason),
            _ => None,
        }
    }
}
