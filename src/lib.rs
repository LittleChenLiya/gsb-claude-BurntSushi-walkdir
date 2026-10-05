/*!
Crate `walkdir` provides an efficient and cross platform implementation
of recursive directory traversal. Several options are exposed to control
iteration, such as whether to follow symbolic links (default off), limit the
maximum number of simultaneous open file descriptors and the ability to
efficiently skip descending into directories.

To use this crate, add `walkdir` as a dependency to your project's
`Cargo.toml`:

```toml
[dependencies]
walkdir = "2"
```

# From the top

The [`WalkDir`] type builds iterators. The [`DirEntry`] type describes values
yielded by the iterator. Finally, the [`Error`] type is a small wrapper around
[`std::io::Error`] with additional information, such as if a loop was detected
while following symbolic links (not enabled by default).

[`WalkDir`]: struct.WalkDir.html
[`DirEntry`]: struct.DirEntry.html
[`Error`]: struct.Error.html
[`std::io::Error`]: https://doc.rust-lang.org/stable/std/io/struct.Error.html

# Example

The following code recursively iterates over the directory given and prints
the path for each entry:

```no_run
use walkdir::WalkDir;
# use walkdir::Error;

# fn try_main() -> Result<(), Error> {
for entry in WalkDir::new("foo") {
    println!("{}", entry?.path().display());
}
# Ok(())
# }
```

Or, if you'd like to iterate over all entries and ignore any errors that
may arise, use [`filter_map`]. (e.g., This code below will silently skip
directories that the owner of the running process does not have permission to
access.)

```no_run
use walkdir::WalkDir;

for entry in WalkDir::new("foo").into_iter().filter_map(|e| e.ok()) {
    println!("{}", entry.path().display());
}
```

[`filter_map`]: https://doc.rust-lang.org/stable/std/iter/trait.Iterator.html#method.filter_map

# Example: follow symbolic links

The same code as above, except [`follow_links`] is enabled:

```no_run
use walkdir::WalkDir;
# use walkdir::Error;

# fn try_main() -> Result<(), Error> {
for entry in WalkDir::new("foo").follow_links(true) {
    println!("{}", entry?.path().display());
}
# Ok(())
# }
```

[`follow_links`]: struct.WalkDir.html#method.follow_links

# Example: skip hidden files and directories on unix

This uses the [`filter_entry`] iterator adapter to avoid yielding hidden files
and directories efficiently (i.e. without recursing into hidden directories):

```no_run
use walkdir::{DirEntry, WalkDir};
# use walkdir::Error;

fn is_hidden(entry: &DirEntry) -> bool {
    entry.file_name()
         .to_str()
         .map(|s| s.starts_with("."))
         .unwrap_or(false)
}

# fn try_main() -> Result<(), Error> {
let walker = WalkDir::new("foo").into_iter();
for entry in walker.filter_entry(|e| !is_hidden(e)) {
    println!("{}", entry?.path().display());
}
# Ok(())
# }
```

[`filter_entry`]: struct.IntoIter.html#method.filter_entry
*/

#![deny(missing_docs)]
#![allow(unknown_lints)]

#[cfg(doctest)]
doc_comment::doctest!("../README.md");

use std::cmp::{min, Ordering};
use std::fmt;
use std::fs::{self, ReadDir};
use std::io;
use std::iter;
use std::path::{Path, PathBuf};
use std::result;
use std::sync::Arc;
use std::vec;

use same_file::Handle;

pub use crate::dent::DirEntry;
#[cfg(unix)]
pub use crate::dent::DirEntryExt;
pub use crate::error::{Error, ErrorStage};

mod dent;
mod error;
#[cfg(test)]
mod tests;
mod util;

/// A trait for opening a directory handle.
///
/// This abstracts [`std::fs::read_dir`] so that the directory retry logic can
/// be tested with an injected opener that fails deterministically.
///
/// [`std::fs::read_dir`]: https://doc.rust-lang.org/stable/std/fs/fn.read_dir.html
pub(crate) trait DirOpener: Send + Sync {
    /// Open a handle to reading the directory at `path`.
    fn open(&self, path: &Path) -> io::Result<Box<dyn DirReader>>;
}

/// A trait for reading entries from an open directory handle.
///
/// This abstracts [`std::fs::ReadDir`] so that the directory retry logic can
/// be tested with an injected reader that fails deterministically.
///
/// [`std::fs::ReadDir`]: https://doc.rust-lang.org/stable/std/fs/struct.ReadDir.html
pub(crate) trait DirReader:
    Iterator<Item = io::Result<fs::DirEntry>> + Send + Sync
{
}

impl DirReader for ReadDir {}

/// Like try, but for iterators that return [`Option<Result<_, _>>`].
///
/// [`Option<Result<_, _>>`]: https://doc.rust-lang.org/stable/std/option/enum.Option.html
macro_rules! itry {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(err) => return Some(Err(From::from(err))),
        }
    };
}

/// A result type for walkdir operations.
///
/// Note that this result type embeds the error type in this crate. This
/// is only useful if you care about the additional information provided by
/// the error (such as the path associated with the error or whether a loop
/// was dectected). If you want things to Just Work, then you can use
/// [`io::Result`] instead since the error type in this package will
/// automatically convert to an [`io::Result`] when using the [`try!`] macro.
///
/// [`io::Result`]: https://doc.rust-lang.org/stable/std/io/type.Result.html
/// [`try!`]: https://doc.rust-lang.org/stable/std/macro.try.html
pub type Result<T> = ::std::result::Result<T, Error>;

/// A builder to create an iterator for recursively walking a directory.
///
/// Results are returned in depth first fashion, with directories yielded
/// before their contents. If [`contents_first`] is true, contents are yielded
/// before their directories. The order is unspecified but if [`sort_by`] is
/// given, directory entries are sorted according to this function. Directory
/// entries `.` and `..` are always omitted.
///
/// If an error occurs at any point during iteration, then it is returned in
/// place of its corresponding directory entry and iteration continues as
/// normal. If an error occurs while opening a directory for reading, then it
/// is not descended into (but the error is still yielded by the iterator).
/// Iteration may be stopped at any time. When the iterator is destroyed, all
/// resources associated with it are freed.
///
/// [`contents_first`]: struct.WalkDir.html#method.contents_first
/// [`sort_by`]: struct.WalkDir.html#method.sort_by
///
/// # Usage
///
/// This type implements [`IntoIterator`] so that it may be used as the subject
/// of a `for` loop. You may need to call [`into_iter`] explicitly if you want
/// to use iterator adapters such as [`filter_entry`].
///
/// Idiomatic use of this type should use method chaining to set desired
/// options. For example, this only shows entries with a depth of `1`, `2` or
/// `3` (relative to `foo`):
///
/// ```no_run
/// use walkdir::WalkDir;
/// # use walkdir::Error;
///
/// # fn try_main() -> Result<(), Error> {
/// for entry in WalkDir::new("foo").min_depth(1).max_depth(3) {
///     println!("{}", entry?.path().display());
/// }
/// # Ok(())
/// # }
/// ```
///
/// [`IntoIterator`]: https://doc.rust-lang.org/stable/std/iter/trait.IntoIterator.html
/// [`into_iter`]: https://doc.rust-lang.org/nightly/core/iter/trait.IntoIterator.html#tymethod.into_iter
/// [`filter_entry`]: struct.IntoIter.html#method.filter_entry
///
/// Note that the iterator by default includes the top-most directory. Since
/// this is the only directory yielded with depth `0`, it is easy to ignore it
/// with the [`min_depth`] setting:
///
/// ```no_run
/// use walkdir::WalkDir;
/// # use walkdir::Error;
///
/// # fn try_main() -> Result<(), Error> {
/// for entry in WalkDir::new("foo").min_depth(1) {
///     println!("{}", entry?.path().display());
/// }
/// # Ok(())
/// # }
/// ```
///
/// [`min_depth`]: struct.WalkDir.html#method.min_depth
///
/// This will only return descendents of the `foo` directory and not `foo`
/// itself.
///
/// # Loops
///
/// This iterator (like most/all recursive directory iterators) assumes that
/// no loops can be made with *hard* links on your file system. In particular,
/// this would require creating a hard link to a directory such that it creates
/// a loop. On most platforms, this operation is illegal.
///
/// Note that when following symbolic/soft links, loops are detected and an
/// error is reported.
#[derive(Debug)]
pub struct WalkDir {
    opts: WalkDirOptions,
    root: PathBuf,
}

/// The default [`DirOpener`], which delegates to [`std::fs::read_dir`].
///
/// [`DirOpener`]: trait.DirOpener.html
/// [`std::fs::read_dir`]: https://doc.rust-lang.org/stable/std/fs/fn.read_dir.html
#[derive(Debug)]
pub(crate) struct RealOpener;

impl DirOpener for RealOpener {
    fn open(&self, path: &Path) -> io::Result<Box<dyn DirReader>> {
        fs::read_dir(path).map(|rd| Box::new(rd) as Box<dyn DirReader>)
    }
}

struct WalkDirOptions {
    follow_links: bool,
    follow_root_links: bool,
    max_open: usize,
    min_depth: usize,
    max_depth: usize,
    sorter: Option<
        Box<
            dyn FnMut(&DirEntry, &DirEntry) -> Ordering
                + Send
                + Sync
                + 'static,
        >,
    >,
    contents_first: bool,
    same_file_system: bool,
    max_retries: usize,
    opener: Arc<dyn DirOpener>,
}

impl fmt::Debug for WalkDirOptions {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> result::Result<(), fmt::Error> {
        let sorter_str = if self.sorter.is_some() {
            // FnMut isn't `Debug`
            "Some(...)"
        } else {
            "None"
        };
        f.debug_struct("WalkDirOptions")
            .field("follow_links", &self.follow_links)
            .field("follow_root_link", &self.follow_root_links)
            .field("max_open", &self.max_open)
            .field("min_depth", &self.min_depth)
            .field("max_depth", &self.max_depth)
            .field("sorter", &sorter_str)
            .field("contents_first", &self.contents_first)
            .field("same_file_system", &self.same_file_system)
            .field("max_retries", &self.max_retries)
            .field("opener", &"...")
            .finish()
    }
}

impl WalkDir {
    /// Create a builder for a recursive directory iterator starting at the
    /// file path `root`. If `root` is a directory, then it is the first item
    /// yielded by the iterator. If `root` is a file, then it is the first
    /// and only item yielded by the iterator. If `root` is a symlink, then it
    /// is always followed for the purposes of directory traversal. (A root
    /// `DirEntry` still obeys its documentation with respect to symlinks and
    /// the `follow_links` setting.)
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        WalkDir {
            opts: WalkDirOptions {
                follow_links: false,
                follow_root_links: true,
                max_open: 10,
                min_depth: 0,
                max_depth: ::std::usize::MAX,
                sorter: None,
                contents_first: false,
                same_file_system: false,
                max_retries: 0,
                opener: Arc::new(RealOpener),
            },
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Set the minimum depth of entries yielded by the iterator.
    ///
    /// The smallest depth is `0` and always corresponds to the path given
    /// to the `new` function on this type. Its direct descendents have depth
    /// `1`, and their descendents have depth `2`, and so on.
    pub fn min_depth(mut self, depth: usize) -> Self {
        self.opts.min_depth = depth;
        if self.opts.min_depth > self.opts.max_depth {
            self.opts.min_depth = self.opts.max_depth;
        }
        self
    }

    /// Set the maximum depth of entries yield by the iterator.
    ///
    /// The smallest depth is `0` and always corresponds to the path given
    /// to the `new` function on this type. Its direct descendents have depth
    /// `1`, and their descendents have depth `2`, and so on.
    ///
    /// Note that this will not simply filter the entries of the iterator, but
    /// it will actually avoid descending into directories when the depth is
    /// exceeded.
    pub fn max_depth(mut self, depth: usize) -> Self {
        self.opts.max_depth = depth;
        if self.opts.max_depth < self.opts.min_depth {
            self.opts.max_depth = self.opts.min_depth;
        }
        self
    }

    /// Follow symbolic links. By default, this is disabled.
    ///
    /// When `yes` is `true`, symbolic links are followed as if they were
    /// normal directories and files. If a symbolic link is broken or is
    /// involved in a loop, an error is yielded.
    ///
    /// When enabled, the yielded [`DirEntry`] values represent the target of
    /// the link while the path corresponds to the link. See the [`DirEntry`]
    /// type for more details.
    ///
    /// [`DirEntry`]: struct.DirEntry.html
    pub fn follow_links(mut self, yes: bool) -> Self {
        self.opts.follow_links = yes;
        self
    }

    /// Follow symbolic links if these are the root of the traversal.
    /// By default, this is enabled.
    ///
    /// When `yes` is `true`, symbolic links on root paths are followed
    /// which is effective if the symbolic link points to a directory.
    /// If a symbolic link is broken or is involved in a loop, an error is yielded
    /// as the first entry of the traversal.
    ///
    /// When enabled, the yielded [`DirEntry`] values represent the target of
    /// the link while the path corresponds to the link. See the [`DirEntry`]
    /// type for more details, and all future entries will be contained within
    /// the resolved directory behind the symbolic link of the root path.
    ///
    /// [`DirEntry`]: struct.DirEntry.html
    pub fn follow_root_links(mut self, yes: bool) -> Self {
        self.opts.follow_root_links = yes;
        self
    }

    /// Set the maximum number of simultaneously open file descriptors used
    /// by the iterator.
    ///
    /// `n` must be greater than or equal to `1`. If `n` is `0`, then it is set
    /// to `1` automatically. If this is not set, then it defaults to some
    /// reasonably low number.
    ///
    /// This setting has no impact on the results yielded by the iterator
    /// (even when `n` is `1`). Instead, this setting represents a trade off
    /// between scarce resources (file descriptors) and memory. Namely, when
    /// the maximum number of file descriptors is reached and a new directory
    /// needs to be opened to continue iteration, then a previous directory
    /// handle is closed and has its unyielded entries stored in memory. In
    /// practice, this is a satisfying trade off because it scales with respect
    /// to the *depth* of your file tree. Therefore, low values (even `1`) are
    /// acceptable.
    ///
    /// Note that this value does not impact the number of system calls made by
    /// an exhausted iterator.
    ///
    /// # Platform behavior
    ///
    /// On Windows, if `follow_links` is enabled, then this limit is not
    /// respected. In particular, the maximum number of file descriptors opened
    /// is proportional to the depth of the directory tree traversed.
    pub fn max_open(mut self, mut n: usize) -> Self {
        if n == 0 {
            n = 1;
        }
        self.opts.max_open = n;
        self
    }

    /// Set a function for sorting directory entries with a comparator
    /// function.
    ///
    /// If a compare function is set, the resulting iterator will return all
    /// paths in sorted order. The compare function will be called to compare
    /// entries from the same directory.
    ///
    /// ```rust,no_run
    /// use std::cmp;
    /// use std::ffi::OsString;
    /// use walkdir::WalkDir;
    ///
    /// WalkDir::new("foo").sort_by(|a,b| a.file_name().cmp(b.file_name()));
    /// ```
    pub fn sort_by<F>(mut self, cmp: F) -> Self
    where
        F: FnMut(&DirEntry, &DirEntry) -> Ordering + Send + Sync + 'static,
    {
        self.opts.sorter = Some(Box::new(cmp));
        self
    }

    /// Set a function for sorting directory entries with a key extraction
    /// function.
    ///
    /// If a compare function is set, the resulting iterator will return all
    /// paths in sorted order. The compare function will be called to compare
    /// entries from the same directory.
    ///
    /// ```rust,no_run
    /// use std::cmp;
    /// use std::ffi::OsString;
    /// use walkdir::WalkDir;
    ///
    /// WalkDir::new("foo").sort_by_key(|a| a.file_name().to_owned());
    /// ```
    pub fn sort_by_key<K, F>(self, mut cmp: F) -> Self
    where
        F: FnMut(&DirEntry) -> K + Send + Sync + 'static,
        K: Ord,
    {
        self.sort_by(move |a, b| cmp(a).cmp(&cmp(b)))
    }

    /// Sort directory entries by file name, to ensure a deterministic order.
    ///
    /// This is a convenience function for calling `Self::sort_by()`.
    ///
    /// ```rust,no_run
    /// use walkdir::WalkDir;
    ///
    /// WalkDir::new("foo").sort_by_file_name();
    /// ```
    pub fn sort_by_file_name(self) -> Self {
        self.sort_by(|a, b| a.file_name().cmp(b.file_name()))
    }

    /// Yield a directory's contents before the directory itself. By default,
    /// this is disabled.
    ///
    /// When `yes` is `false` (as is the default), the directory is yielded
    /// before its contents are read. This is useful when, e.g. you want to
    /// skip processing of some directories.
    ///
    /// When `yes` is `true`, the iterator yields the contents of a directory
    /// before yielding the directory itself. This is useful when, e.g. you
    /// want to recursively delete a directory.
    ///
    /// # Example
    ///
    /// Assume the following directory tree:
    ///
    /// ```text
    /// foo/
    ///   abc/
    ///     qrs
    ///     tuv
    ///   def/
    /// ```
    ///
    /// With contents_first disabled (the default), the following code visits
    /// the directory tree in depth-first order:
    ///
    /// ```no_run
    /// use walkdir::WalkDir;
    ///
    /// for entry in WalkDir::new("foo") {
    ///     let entry = entry.unwrap();
    ///     println!("{}", entry.path().display());
    /// }
    ///
    /// // foo
    /// // foo/abc
    /// // foo/abc/qrs
    /// // foo/abc/tuv
    /// // foo/def
    /// ```
    ///
    /// With contents_first enabled:
    ///
    /// ```no_run
    /// use walkdir::WalkDir;
    ///
    /// for entry in WalkDir::new("foo").contents_first(true) {
    ///     let entry = entry.unwrap();
    ///     println!("{}", entry.path().display());
    /// }
    ///
    /// // foo/abc/qrs
    /// // foo/abc/tuv
    /// // foo/abc
    /// // foo/def
    /// // foo
    /// ```
    pub fn contents_first(mut self, yes: bool) -> Self {
        self.opts.contents_first = yes;
        self
    }

    /// Do not cross file system boundaries.
    ///
    /// When this option is enabled, directory traversal will not descend into
    /// directories that are on a different file system from the root path.
    ///
    /// Currently, this option is only supported on Unix and Windows. If this
    /// option is used on an unsupported platform, then directory traversal
    /// will immediately return an error and will not yield any entries.
    pub fn same_file_system(mut self, yes: bool) -> Self {
        self.opts.same_file_system = yes;
        self
    }

    /// Set the maximum number of times a failed directory operation is
    /// retried.
    ///
    /// When `max_retries` is `0` (the default), no retries are attempted: the
    /// first error encountered is yielded by the iterator, exactly as it was
    /// before this option existed.
    ///
    /// When `max_retries` is greater than `0`, the iterator will retry a
    /// failed directory operation up to `max_retries` times before yielding
    /// the error. Retries are only attempted under the following
    /// circumstances:
    ///
    /// * **Opening the directory fails.** The directory handle is reopened.
    /// * **Reading the first entry from the directory fails.** No entry has
    ///   been delivered yet, so the old handle is closed and the directory is
    ///   reopened.
    ///
    /// Once at least one entry has been successfully read from a directory,
    /// any subsequent read error is **not** retried. This avoids delivering
    /// duplicate entries: reopening the directory would restart enumeration
    /// from the beginning. Such errors are marked as non-retryable via
    /// [`Error::is_retryable`].
    ///
    /// Only the directory that failed is retried. Other directories on the
    /// stack are not rescanned. When retrying a read failure, the old
    /// directory handle is closed (dropped) before the directory is
    /// reopened.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use walkdir::WalkDir;
    ///
    /// for entry in WalkDir::new("foo").retry_on_error(3) {
    ///     let entry = entry?;
    ///     println!("{}", entry.path().display());
    /// }
    /// # Ok::<(), walkdir::Error>(())
    /// ```
    ///
    /// [`Error::is_retryable`]: struct.Error.html#method.is_retryable
    pub fn retry_on_error(mut self, max_retries: usize) -> Self {
        self.opts.max_retries = max_retries;
        self
    }

    /// Replace the directory opener.
    ///
    /// This is used by tests to inject an opener that fails deterministically.
    #[cfg(test)]
    pub(crate) fn with_opener(mut self, opener: Arc<dyn DirOpener>) -> Self {
        self.opts.opener = opener;
        self
    }
}

impl IntoIterator for WalkDir {
    type Item = Result<DirEntry>;
    type IntoIter = IntoIter;

    fn into_iter(self) -> IntoIter {
        IntoIter {
            opts: self.opts,
            start: Some(self.root),
            stack_list: vec![],
            stack_path: vec![],
            oldest_opened: 0,
            depth: 0,
            deferred_dirs: vec![],
            root_device: None,
        }
    }
}

/// An iterator for recursively descending into a directory.
///
/// A value with this type must be constructed with the [`WalkDir`] type, which
/// uses a builder pattern to set options such as min/max depth, max open file
/// descriptors and whether the iterator should follow symbolic links. After
/// constructing a `WalkDir`, call [`.into_iter()`] at the end of the chain.
///
/// The order of elements yielded by this iterator is unspecified.
///
/// [`WalkDir`]: struct.WalkDir.html
/// [`.into_iter()`]: struct.WalkDir.html#into_iter.v
#[derive(Debug)]
pub struct IntoIter {
    /// Options specified in the builder. Depths, max fds, etc.
    opts: WalkDirOptions,
    /// The start path.
    ///
    /// This is only `Some(...)` at the beginning. After the first iteration,
    /// this is always `None`.
    start: Option<PathBuf>,
    /// A stack of open (up to max fd) or closed handles to directories.
    /// An open handle is a plain [`fs::ReadDir`] while a closed handle is
    /// a `Vec<fs::DirEntry>` corresponding to the as-of-yet consumed entries.
    ///
    /// [`fs::ReadDir`]: https://doc.rust-lang.org/stable/std/fs/struct.ReadDir.html
    stack_list: Vec<DirList>,
    /// A stack of file paths.
    ///
    /// This is *only* used when [`follow_links`] is enabled. In all other
    /// cases this stack is empty.
    ///
    /// [`follow_links`]: struct.WalkDir.html#method.follow_links
    stack_path: Vec<Ancestor>,
    /// An index into `stack_list` that points to the oldest open directory
    /// handle. If the maximum fd limit is reached and a new directory needs to
    /// be read, the handle at this index is closed before the new directory is
    /// opened.
    oldest_opened: usize,
    /// The current depth of iteration (the length of the stack at the
    /// beginning of each iteration).
    depth: usize,
    /// A list of DirEntries corresponding to directories, that are
    /// yielded after their contents has been fully yielded. This is only
    /// used when `contents_first` is enabled.
    deferred_dirs: Vec<DirEntry>,
    /// The device of the root file path when the first call to `next` was
    /// made.
    ///
    /// If the `same_file_system` option isn't enabled, then this is always
    /// `None`. Conversely, if it is enabled, this is always `Some(...)` after
    /// handling the root path.
    root_device: Option<u64>,
}

/// An ancestor is an item in the directory tree traversed by walkdir, and is
/// used to check for loops in the tree when traversing symlinks.
#[derive(Debug)]
struct Ancestor {
    /// The path of this ancestor.
    path: PathBuf,
    /// An open file to this ancesor. This is only used on Windows where
    /// opening a file handle appears to be quite expensive, so we choose to
    /// cache it. This comes at the cost of not respecting the file descriptor
    /// limit set by the user.
    #[cfg(windows)]
    handle: Handle,
}

impl Ancestor {
    /// Create a new ancestor from the given directory path.
    #[cfg(windows)]
    fn new(dent: &DirEntry) -> io::Result<Ancestor> {
        let handle = Handle::from_path(dent.path())?;
        Ok(Ancestor { path: dent.path().to_path_buf(), handle })
    }

    /// Create a new ancestor from the given directory path.
    #[cfg(not(windows))]
    fn new(dent: &DirEntry) -> io::Result<Ancestor> {
        Ok(Ancestor { path: dent.path().to_path_buf() })
    }

    /// Returns true if and only if the given open file handle corresponds to
    /// the same directory as this ancestor.
    #[cfg(windows)]
    fn is_same(&self, child: &Handle) -> io::Result<bool> {
        Ok(child == &self.handle)
    }

    /// Returns true if and only if the given open file handle corresponds to
    /// the same directory as this ancestor.
    #[cfg(not(windows))]
    fn is_same(&self, child: &Handle) -> io::Result<bool> {
        Ok(child == &Handle::from_path(&self.path)?)
    }
}

/// A sequence of unconsumed directory entries.
///
/// This represents the opened or closed state of a directory handle. When
/// open, future entries are read by iterating over the raw `fs::ReadDir`.
/// When closed, all future entries are read into memory. Iteration then
/// proceeds over a [`Vec<fs::DirEntry>`].
///
/// [`fs::ReadDir`]: https://doc.rust-lang.org/stable/std/fs/struct.ReadDir.html
/// [`Vec<fs::DirEntry>`]: https://doc.rust-lang.org/stable/std/vec/struct.Vec.html
#[derive(Debug)]
enum DirList {
    /// An opened handle.
    ///
    /// This includes the depth of the handle itself, the path being read,
    /// and the state needed to enforce the directory retry policy.
    Opened(OpenDir),
    /// A closed handle.
    ///
    /// All remaining directory entries are read into memory.
    Closed(vec::IntoIter<Result<DirEntry>>),
}

/// The state of an open directory handle.
///
/// This tracks the underlying reader along with the state needed to enforce
/// the directory retry policy: a retry is only permitted before any entry
/// has been successfully read from the directory.
struct OpenDir {
    /// The depth of the directory itself (entries are at `depth + 1`).
    depth: usize,
    /// The path of the directory being read.
    path: PathBuf,
    /// The underlying reader. This is `None` once the directory has been
    /// exhausted or a terminal error has been yielded.
    reader: Option<Box<dyn DirReader>>,
    /// Whether at least one entry has been successfully read from this
    /// directory. Once `true`, any subsequent read error is non-retryable.
    read_any: bool,
    /// The number of retries attempted so far.
    attempts: usize,
    /// The maximum number of retries permitted.
    max_retries: usize,
    /// The opener used to reopen the directory when a retry is permitted.
    opener: Arc<dyn DirOpener>,
}

impl fmt::Debug for OpenDir {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> result::Result<(), fmt::Error> {
        f.debug_struct("OpenDir")
            .field("depth", &self.depth)
            .field("path", &self.path)
            .field("reader", &self.reader.is_some())
            .field("read_any", &self.read_any)
            .field("attempts", &self.attempts)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

impl Iterator for IntoIter {
    type Item = Result<DirEntry>;
    /// Advances the iterator and returns the next value.
    ///
    /// # Errors
    ///
    /// If the iterator fails to retrieve the next value, this method returns
    /// an error value. The error will be wrapped in an Option::Some.
    fn next(&mut self) -> Option<Result<DirEntry>> {
        if let Some(start) = self.start.take() {
            if self.opts.same_file_system {
                let result = util::device_num(&start)
                    .map_err(|e| Error::from_path(0, start.clone(), e));
                self.root_device = Some(itry!(result));
            }
            let dent = itry!(DirEntry::from_path(0, start, false));
            if let Some(result) = self.handle_entry(dent) {
                return Some(result);
            }
        }
        while !self.stack_list.is_empty() {
            self.depth = self.stack_list.len();
            if let Some(dentry) = self.get_deferred_dir() {
                return Some(Ok(dentry));
            }
            if self.depth > self.opts.max_depth {
                // If we've exceeded the max depth, pop the current dir
                // so that we don't descend.
                self.pop();
                continue;
            }
            // Unwrap is safe here because we've verified above that
            // `self.stack_list` is not empty
            let next = self
                .stack_list
                .last_mut()
                .expect("BUG: stack should be non-empty")
                .next();
            match next {
                None => self.pop(),
                Some(Err(err)) => return Some(Err(err)),
                Some(Ok(dent)) => {
                    if let Some(result) = self.handle_entry(dent) {
                        return Some(result);
                    }
                }
            }
        }
        if self.opts.contents_first {
            self.depth = self.stack_list.len();
            if let Some(dentry) = self.get_deferred_dir() {
                return Some(Ok(dentry));
            }
        }
        None
    }
}

impl IntoIter {
    /// Skips the current directory.
    ///
    /// This causes the iterator to stop traversing the contents of the least
    /// recently yielded directory. This means any remaining entries in that
    /// directory will be skipped (including sub-directories).
    ///
    /// Note that the ergonomics of this method are questionable since it
    /// borrows the iterator mutably. Namely, you must write out the looping
    /// condition manually. For example, to skip hidden entries efficiently on
    /// unix systems:
    ///
    /// ```no_run
    /// use walkdir::{DirEntry, WalkDir};
    ///
    /// fn is_hidden(entry: &DirEntry) -> bool {
    ///     entry.file_name()
    ///          .to_str()
    ///          .map(|s| s.starts_with("."))
    ///          .unwrap_or(false)
    /// }
    ///
    /// let mut it = WalkDir::new("foo").into_iter();
    /// loop {
    ///     let entry = match it.next() {
    ///         None => break,
    ///         Some(Err(err)) => panic!("ERROR: {}", err),
    ///         Some(Ok(entry)) => entry,
    ///     };
    ///     if is_hidden(&entry) {
    ///         if entry.file_type().is_dir() {
    ///             it.skip_current_dir();
    ///         }
    ///         continue;
    ///     }
    ///     println!("{}", entry.path().display());
    /// }
    /// ```
    ///
    /// You may find it more convenient to use the [`filter_entry`] iterator
    /// adapter. (See its documentation for the same example functionality as
    /// above.)
    ///
    /// [`filter_entry`]: #method.filter_entry
    pub fn skip_current_dir(&mut self) {
        if !self.stack_list.is_empty() {
            self.pop();
        }
    }

    /// Yields only entries which satisfy the given predicate and skips
    /// descending into directories that do not satisfy the given predicate.
    ///
    /// The predicate is applied to all entries. If the predicate is
    /// true, iteration carries on as normal. If the predicate is false, the
    /// entry is ignored and if it is a directory, it is not descended into.
    ///
    /// This is often more convenient to use than [`skip_current_dir`]. For
    /// example, to skip hidden files and directories efficiently on unix
    /// systems:
    ///
    /// ```no_run
    /// use walkdir::{DirEntry, WalkDir};
    /// # use walkdir::Error;
    ///
    /// fn is_hidden(entry: &DirEntry) -> bool {
    ///     entry.file_name()
    ///          .to_str()
    ///          .map(|s| s.starts_with("."))
    ///          .unwrap_or(false)
    /// }
    ///
    /// # fn try_main() -> Result<(), Error> {
    /// for entry in WalkDir::new("foo")
    ///                      .into_iter()
    ///                      .filter_entry(|e| !is_hidden(e)) {
    ///     println!("{}", entry?.path().display());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Note that the iterator will still yield errors for reading entries that
    /// may not satisfy the predicate.
    ///
    /// Note that entries skipped with [`min_depth`] and [`max_depth`] are not
    /// passed to this predicate.
    ///
    /// Note that if the iterator has `contents_first` enabled, then this
    /// method is no different than calling the standard `Iterator::filter`
    /// method (because directory entries are yielded after they've been
    /// descended into).
    ///
    /// [`skip_current_dir`]: #method.skip_current_dir
    /// [`min_depth`]: struct.WalkDir.html#method.min_depth
    /// [`max_depth`]: struct.WalkDir.html#method.max_depth
    pub fn filter_entry<P>(self, predicate: P) -> FilterEntry<Self, P>
    where
        P: FnMut(&DirEntry) -> bool,
    {
        FilterEntry { it: self, predicate }
    }

    fn handle_entry(
        &mut self,
        mut dent: DirEntry,
    ) -> Option<Result<DirEntry>> {
        if self.opts.follow_links && dent.file_type().is_symlink() {
            dent = itry!(self.follow(dent));
        }
        let is_normal_dir = !dent.file_type().is_symlink() && dent.is_dir();
        if is_normal_dir {
            if self.opts.same_file_system && dent.depth() > 0 {
                if itry!(self.is_same_file_system(&dent)) {
                    itry!(self.push(&dent));
                }
            } else {
                itry!(self.push(&dent));
            }
        } else if dent.depth() == 0
            && dent.file_type().is_symlink()
            && self.opts.follow_root_links
        {
            // As a special case, if we are processing a root entry, then we
            // always follow it even if it's a symlink and follow_links is
            // false. We are careful to not let this change the semantics of
            // the DirEntry however. Namely, the DirEntry should still respect
            // the follow_links setting. When it's disabled, it should report
            // itself as a symlink. When it's enabled, it should always report
            // itself as the target.
            let md = itry!(fs::metadata(dent.path()).map_err(|err| {
                Error::from_path(dent.depth(), dent.path().to_path_buf(), err)
            }));
            if md.file_type().is_dir() {
                itry!(self.push(&dent));
            }
        }
        if is_normal_dir && self.opts.contents_first {
            self.deferred_dirs.push(dent);
            None
        } else if self.skippable() {
            None
        } else {
            Some(Ok(dent))
        }
    }

    fn get_deferred_dir(&mut self) -> Option<DirEntry> {
        if self.opts.contents_first {
            if self.depth < self.deferred_dirs.len() {
                // Unwrap is safe here because we've guaranteed that
                // `self.deferred_dirs.len()` can never be less than 1
                let deferred: DirEntry = self
                    .deferred_dirs
                    .pop()
                    .expect("BUG: deferred_dirs should be non-empty");
                if !self.skippable() {
                    return Some(deferred);
                }
            }
        }
        None
    }

    fn push(&mut self, dent: &DirEntry) -> Result<()> {
        // Make room for another open file descriptor if we've hit the max.
        let free =
            self.stack_list.len().checked_sub(self.oldest_opened).unwrap();
        if free == self.opts.max_open {
            self.stack_list[self.oldest_opened].close();
        }
        // Open a handle to reading the directory's entries, retrying according
        // to the directory retry policy.
        let path = dent.path().to_path_buf();
        let reader = self.open_dir(&path)?;
        let mut list = DirList::Opened(OpenDir {
            depth: self.depth,
            path,
            reader: Some(reader),
            read_any: false,
            attempts: 0,
            max_retries: self.opts.max_retries,
            opener: self.opts.opener.clone(),
        });
        if let Some(ref mut cmp) = self.opts.sorter {
            let mut entries: Vec<_> = list.collect();
            entries.sort_by(|a, b| match (a, b) {
                (&Ok(ref a), &Ok(ref b)) => cmp(a, b),
                (&Err(_), &Err(_)) => Ordering::Equal,
                (&Ok(_), &Err(_)) => Ordering::Greater,
                (&Err(_), &Ok(_)) => Ordering::Less,
            });
            list = DirList::Closed(entries.into_iter());
        }
        if self.opts.follow_links {
            let ancestor = Ancestor::new(&dent)
                .map_err(|err| Error::from_io(self.depth, err))?;
            self.stack_path.push(ancestor);
        }
        // We push this after stack_path since creating the Ancestor can fail.
        // If it fails, then we return the error and won't descend.
        self.stack_list.push(list);
        // If we had to close out a previous directory stream, then we need to
        // increment our index the oldest still-open stream. We do this only
        // after adding to our stack, in order to ensure that the oldest_opened
        // index remains valid. The worst that can happen is that an already
        // closed stream will be closed again, which is a no-op.
        //
        // We could move the close of the stream above into this if-body, but
        // then we would have more than the maximum number of file descriptors
        // open at a particular point in time.
        if free == self.opts.max_open {
            // Unwrap is safe here because self.oldest_opened is guaranteed to
            // never be greater than `self.stack_list.len()`, which implies
            // that the subtraction won't underflow and that adding 1 will
            // never overflow.
            self.oldest_opened = self.oldest_opened.checked_add(1).unwrap();
        }
        Ok(())
    }

    /// Open a handle to reading the directory at `path`, retrying according to
    /// the directory retry policy.
    ///
    /// Each failed open attempt consumes one retry. Once the retry budget is
    /// exhausted, the error is returned with [`ErrorStage::Open`].
    fn open_dir(&self, path: &Path) -> Result<Box<dyn DirReader>> {
        let mut attempts = 0;
        loop {
            match self.opts.opener.open(path) {
                Ok(reader) => return Ok(reader),
                Err(err) => {
                    if attempts < self.opts.max_retries {
                        attempts += 1;
                        continue;
                    }
                    return Err(Error::open(
                        self.depth,
                        path.to_path_buf(),
                        err,
                    ));
                }
            }
        }
    }

    fn pop(&mut self) {
        self.stack_list.pop().expect("BUG: cannot pop from empty stack");
        if self.opts.follow_links {
            self.stack_path.pop().expect("BUG: list/path stacks out of sync");
        }
        // If everything in the stack is already closed, then there is
        // room for at least one more open descriptor and it will
        // always be at the top of the stack.
        self.oldest_opened = min(self.oldest_opened, self.stack_list.len());
    }

    fn follow(&self, mut dent: DirEntry) -> Result<DirEntry> {
        dent =
            DirEntry::from_path(self.depth, dent.path().to_path_buf(), true)?;
        // The only way a symlink can cause a loop is if it points
        // to a directory. Otherwise, it always points to a leaf
        // and we can omit any loop checks.
        if dent.is_dir() {
            self.check_loop(dent.path())?;
        }
        Ok(dent)
    }

    fn check_loop<P: AsRef<Path>>(&self, child: P) -> Result<()> {
        let hchild = Handle::from_path(&child)
            .map_err(|err| Error::from_io(self.depth, err))?;
        for ancestor in self.stack_path.iter().rev() {
            let is_same = ancestor
                .is_same(&hchild)
                .map_err(|err| Error::from_io(self.depth, err))?;
            if is_same {
                return Err(Error::from_loop(
                    self.depth,
                    &ancestor.path,
                    child.as_ref(),
                ));
            }
        }
        Ok(())
    }

    fn is_same_file_system(&mut self, dent: &DirEntry) -> Result<bool> {
        let dent_device = util::device_num(dent.path())
            .map_err(|err| Error::from_entry(dent, err))?;
        Ok(self
            .root_device
            .map(|d| d == dent_device)
            .expect("BUG: called is_same_file_system without root device"))
    }

    fn skippable(&self) -> bool {
        self.depth < self.opts.min_depth || self.depth > self.opts.max_depth
    }
}

impl iter::FusedIterator for IntoIter {}

impl DirList {
    fn close(&mut self) {
        if let DirList::Opened(ref mut od) = *self {
            let depth = od.depth;
            let path = od.path.clone();
            let read_any = od.read_any;
            // Collect the remaining entries into memory without retrying. The
            // purpose of closing is to free a file descriptor, and reopening
            // the directory here would keep the descriptor open. In practice
            // a directory is only closed after at least one entry has been
            // read from it, so no retry is permitted anyway.
            let entries: Vec<_> = od
                .reader
                .take()
                .into_iter()
                .flatten()
                .map(|r| match r {
                    Ok(r) => DirEntry::from_entry(depth + 1, &r),
                    Err(err) => {
                        Err(Error::read(depth + 1, path.clone(), !read_any, err))
                    }
                })
                .collect();
            *self = DirList::Closed(entries.into_iter());
        }
    }
}

impl Iterator for OpenDir {
    type Item = Result<DirEntry>;

    fn next(&mut self) -> Option<Result<DirEntry>> {
        loop {
            // Read the next item from the current reader. The borrow of
            // `self.reader` ends with this block, so we are free to mutate
            // `self.reader` (e.g. drop it before reopening) below.
            let read_result = {
                let reader = match self.reader {
                    Some(ref mut r) => r,
                    None => return None,
                };
                reader.next()
            };
            match read_result {
                None => return None,
                Some(Ok(r)) => {
                    self.read_any = true;
                    return Some(DirEntry::from_entry(self.depth + 1, &r));
                }
                Some(Err(err)) => {
                    if self.read_any || self.attempts >= self.max_retries {
                        // Either an entry has already been delivered (so a
                        // reopen would duplicate entries) or the retry budget
                        // is exhausted. Yield the error and stop.
                        self.reader = None;
                        return Some(Err(Error::read(
                            self.depth + 1,
                            self.path.clone(),
                            !self.read_any,
                            err,
                        )));
                    }
                    // No entry has been read yet and retries remain. Close the
                    // old handle before reopening the directory.
                    self.reader = None;
                    self.attempts += 1;
                    match self.opener.open(&self.path) {
                        Ok(new_reader) => {
                            self.reader = Some(new_reader);
                            // Loop to read from the reopened handle.
                        }
                        Err(open_err) => {
                            // Reopening failed. Yield the open error and stop.
                            self.reader = None;
                            return Some(Err(Error::open(
                                self.depth,
                                self.path.clone(),
                                open_err,
                            )));
                        }
                    }
                }
            }
        }
    }
}

impl Iterator for DirList {
    type Item = Result<DirEntry>;

    #[inline(always)]
    fn next(&mut self) -> Option<Result<DirEntry>> {
        match *self {
            DirList::Closed(ref mut it) => it.next(),
            DirList::Opened(ref mut od) => od.next(),
        }
    }
}

/// A recursive directory iterator that skips entries.
///
/// Values of this type are created by calling [`.filter_entry()`] on an
/// `IntoIter`, which is formed by calling [`.into_iter()`] on a `WalkDir`.
///
/// Directories that fail the predicate `P` are skipped. Namely, they are
/// never yielded and never descended into.
///
/// Entries that are skipped with the [`min_depth`] and [`max_depth`] options
/// are not passed through this filter.
///
/// If opening a handle to a directory resulted in an error, then it is yielded
/// and no corresponding call to the predicate is made.
///
/// Type parameter `I` refers to the underlying iterator and `P` refers to the
/// predicate, which is usually `FnMut(&DirEntry) -> bool`.
///
/// [`.filter_entry()`]: struct.IntoIter.html#method.filter_entry
/// [`.into_iter()`]: struct.WalkDir.html#into_iter.v
/// [`min_depth`]: struct.WalkDir.html#method.min_depth
/// [`max_depth`]: struct.WalkDir.html#method.max_depth
#[derive(Debug)]
pub struct FilterEntry<I, P> {
    it: I,
    predicate: P,
}

impl<P> Iterator for FilterEntry<IntoIter, P>
where
    P: FnMut(&DirEntry) -> bool,
{
    type Item = Result<DirEntry>;

    /// Advances the iterator and returns the next value.
    ///
    /// # Errors
    ///
    /// If the iterator fails to retrieve the next value, this method returns
    /// an error value. The error will be wrapped in an `Option::Some`.
    fn next(&mut self) -> Option<Result<DirEntry>> {
        loop {
            let dent = match self.it.next() {
                None => return None,
                Some(result) => itry!(result),
            };
            if !(self.predicate)(&dent) {
                if dent.is_dir() {
                    self.it.skip_current_dir();
                }
                continue;
            }
            return Some(Ok(dent));
        }
    }
}

impl<P> iter::FusedIterator for FilterEntry<IntoIter, P> where
    P: FnMut(&DirEntry) -> bool
{
}

impl<P> FilterEntry<IntoIter, P>
where
    P: FnMut(&DirEntry) -> bool,
{
    /// Yields only entries which satisfy the given predicate and skips
    /// descending into directories that do not satisfy the given predicate.
    ///
    /// The predicate is applied to all entries. If the predicate is
    /// true, iteration carries on as normal. If the predicate is false, the
    /// entry is ignored and if it is a directory, it is not descended into.
    ///
    /// This is often more convenient to use than [`skip_current_dir`]. For
    /// example, to skip hidden files and directories efficiently on unix
    /// systems:
    ///
    /// ```no_run
    /// use walkdir::{DirEntry, WalkDir};
    /// # use walkdir::Error;
    ///
    /// fn is_hidden(entry: &DirEntry) -> bool {
    ///     entry.file_name()
    ///          .to_str()
    ///          .map(|s| s.starts_with("."))
    ///          .unwrap_or(false)
    /// }
    ///
    /// # fn try_main() -> Result<(), Error> {
    /// for entry in WalkDir::new("foo")
    ///                      .into_iter()
    ///                      .filter_entry(|e| !is_hidden(e)) {
    ///     println!("{}", entry?.path().display());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Note that the iterator will still yield errors for reading entries that
    /// may not satisfy the predicate.
    ///
    /// Note that entries skipped with [`min_depth`] and [`max_depth`] are not
    /// passed to this predicate.
    ///
    /// Note that if the iterator has `contents_first` enabled, then this
    /// method is no different than calling the standard `Iterator::filter`
    /// method (because directory entries are yielded after they've been
    /// descended into).
    ///
    /// [`skip_current_dir`]: #method.skip_current_dir
    /// [`min_depth`]: struct.WalkDir.html#method.min_depth
    /// [`max_depth`]: struct.WalkDir.html#method.max_depth
    pub fn filter_entry(self, predicate: P) -> FilterEntry<Self, P> {
        FilterEntry { it: self, predicate }
    }

    /// Skips the current directory.
    ///
    /// This causes the iterator to stop traversing the contents of the least
    /// recently yielded directory. This means any remaining entries in that
    /// directory will be skipped (including sub-directories).
    ///
    /// Note that the ergonomics of this method are questionable since it
    /// borrows the iterator mutably. Namely, you must write out the looping
    /// condition manually. For example, to skip hidden entries efficiently on
    /// unix systems:
    ///
    /// ```no_run
    /// use walkdir::{DirEntry, WalkDir};
    ///
    /// fn is_hidden(entry: &DirEntry) -> bool {
    ///     entry.file_name()
    ///          .to_str()
    ///          .map(|s| s.starts_with("."))
    ///          .unwrap_or(false)
    /// }
    ///
    /// let mut it = WalkDir::new("foo").into_iter();
    /// loop {
    ///     let entry = match it.next() {
    ///         None => break,
    ///         Some(Err(err)) => panic!("ERROR: {}", err),
    ///         Some(Ok(entry)) => entry,
    ///     };
    ///     if is_hidden(&entry) {
    ///         if entry.file_type().is_dir() {
    ///             it.skip_current_dir();
    ///         }
    ///         continue;
    ///     }
    ///     println!("{}", entry.path().display());
    /// }
    /// ```
    ///
    /// You may find it more convenient to use the [`filter_entry`] iterator
    /// adapter. (See its documentation for the same example functionality as
    /// above.)
    ///
    /// [`filter_entry`]: #method.filter_entry
    pub fn skip_current_dir(&mut self) {
        self.it.skip_current_dir();
    }
}
