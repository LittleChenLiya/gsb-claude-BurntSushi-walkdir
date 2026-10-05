use std::io;
use std::path::{Path, PathBuf};

/// A factory for opening directory enumeration handles.
///
/// By default, [`WalkDir`] enumerates directories with
/// [`std::fs::read_dir`]. A custom enumerator can be injected with
/// [`WalkDir::dir_enumerator`], which is primarily useful for testing (for
/// example, to deterministically inject enumeration failures and exercise
/// the retry policy configured with [`WalkDir::max_dir_retries`]).
///
/// Each call to [`open`] must return a fresh, independent stream positioned
/// at the beginning of the directory. When the retry policy reopens a
/// directory, the previous [`DirEntryStream`] is always dropped (closing the
/// old enumeration handle) before [`open`] is called again.
///
/// [`WalkDir`]: struct.WalkDir.html
/// [`WalkDir::dir_enumerator`]: struct.WalkDir.html#method.dir_enumerator
/// [`WalkDir::max_dir_retries`]: struct.WalkDir.html#method.max_dir_retries
/// [`std::fs::read_dir`]: https://doc.rust-lang.org/stable/std/fs/fn.read_dir.html
/// [`open`]: #tymethod.open
/// [`DirEntryStream`]: trait.DirEntryStream.html
pub trait DirEnumerator: Send + Sync {
    /// Open `path` for enumeration of its immediate children.
    ///
    /// Returning an error is treated like a failed [`std::fs::read_dir`]
    /// call: the error is yielded by the iterator (with
    /// [`ErrorStage::OpenDir`]) and the directory is not descended into,
    /// unless the retry policy reopens it.
    ///
    /// [`std::fs::read_dir`]: https://doc.rust-lang.org/stable/std/fs/fn.read_dir.html
    /// [`ErrorStage::OpenDir`]: enum.ErrorStage.html#variant.OpenDir
    fn open(&self, path: &Path) -> io::Result<Box<dyn DirEntryStream>>;
}

/// An open directory enumeration handle.
///
/// A stream yields the paths of the immediate children of one directory, one
/// at a time. Dropping the stream closes the underlying enumeration handle.
///
/// Errors returned from [`next_entry`] are treated like errors from
/// [`std::fs::ReadDir`]: they are yielded by the iterator (with
/// [`ErrorStage::ReadDir`]). If no entry has been read from this stream yet
/// and the retry policy has retries left, the stream is dropped and the
/// directory is reopened instead.
///
/// [`next_entry`]: #tymethod.next_entry
/// [`std::fs::ReadDir`]: https://doc.rust-lang.org/stable/std/fs/struct.ReadDir.html
/// [`ErrorStage::ReadDir`]: enum.ErrorStage.html#variant.ReadDir
pub trait DirEntryStream: Send + Sync {
    /// Returns the path of the next child entry in this directory.
    ///
    /// Returns `None` when the directory has been fully enumerated. Only
    /// immediate children of the directory should be yielded; recursion is
    /// driven by the iterator itself.
    fn next_entry(&mut self) -> Option<io::Result<PathBuf>>;
}
