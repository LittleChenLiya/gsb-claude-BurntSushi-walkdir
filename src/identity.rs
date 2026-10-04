//! Types for identifying directories during a recursive walk.
//!
//! When symbolic links are followed (or when a custom identity provider
//! is installed), the same underlying directory may be reachable through
//! several distinct paths. The types in this module let callers decide
//! how a directory is identified and how revisits are handled, without
//! relying on the displayed path alone.

use std::io;
use std::path::{Path, PathBuf};

/// A unique identifier for a directory.
///
/// Two directory entries that refer to the same underlying directory
/// (for example, a directory and a symbolic link or bind mount pointing
/// to it) have equal identities, even if their displayed paths differ.
///
/// By default, identities are derived from the file system itself (the
/// device and inode numbers on Unix, or the volume serial number and
/// file index on Windows). A custom identity can be created with
/// [`custom`] and returned from a user supplied [`DirIdentifier`].
///
/// [`custom`]: #method.custom
/// [`DirIdentifier`]: trait.DirIdentifier.html
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DirId(DirIdInner);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum DirIdInner {
    /// An identity derived from the file system, such as a device and
    /// inode number pair.
    System { dev: u64, ino: u64 },
    /// A canonicalized path, used as a fallback identity on platforms
    /// where no native directory identity is available.
    #[cfg(not(any(unix, windows)))]
    Path(PathBuf),
    /// An identity supplied by the caller.
    Custom(u64),
}

impl DirId {
    /// Create a custom directory identity from a raw 64-bit value.
    ///
    /// This is primarily useful for implementing a custom
    /// [`DirIdentifier`], e.g. in tests that simulate aliased paths
    /// without relying on symbolic link support in the underlying
    /// platform.
    ///
    /// Custom identities never compare equal to identities derived from
    /// the file system.
    ///
    /// [`DirIdentifier`]: trait.DirIdentifier.html
    pub fn custom(raw: u64) -> DirId {
        DirId(DirIdInner::Custom(raw))
    }
}

/// A provider of directory identities.
///
/// A directory identifier maps a path to the [`DirId`] of the directory
/// it refers to. It is used by [`WalkDir::dir_identifier`] to override
/// how directories are identified when detecting loops or duplicate
/// directories.
///
/// This trait is implemented for all closures of the form
/// `Fn(&Path) -> io::Result<DirId>`, so a closure can be supplied
/// directly.
///
/// If an implementation returns an error, the error is yielded by the
/// iterator (like any other IO error encountered while walking) and the
/// corresponding directory is not descended into. In particular, a
/// failed identification is never treated as a new directory.
///
/// [`DirId`]: struct.DirId.html
/// [`WalkDir::dir_identifier`]: struct.WalkDir.html#method.dir_identifier
pub trait DirIdentifier: Send + Sync {
    /// Return the identity of the directory at `path`.
    fn identify(&self, path: &Path) -> io::Result<DirId>;
}

impl<F> DirIdentifier for F
where
    F: Fn(&Path) -> io::Result<DirId> + Send + Sync,
{
    fn identify(&self, path: &Path) -> io::Result<DirId> {
        self(path)
    }
}

/// A policy describing how directory identities are used to avoid
/// revisiting directories during a walk.
///
/// The policy only takes effect when directory identity tracking is
/// active, which happens when symbolic links are followed, when this
/// policy is set to [`OncePerScan`] or when a custom
/// [`DirIdentifier`] is installed. In all other cases, walking behaves
/// exactly as it always has.
///
/// [`OncePerScan`]: enum.DirIdentityPolicy.html#variant.OncePerScan
/// [`DirIdentifier`]: trait.DirIdentifier.html
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirIdentityPolicy {
    /// Only reject loops back to an ancestor of the current directory.
    ///
    /// This is the default and matches the standard `WalkDir` behavior:
    /// a directory that would loop back to one of its ancestors is
    /// reported as an error, but the same directory reached through
    /// different branches of the tree (e.g. via symlinks) may be
    /// visited more than once.
    AncestorsOnly,
    /// Visit each distinct directory at most once for the entire scan.
    ///
    /// Loops back to an ancestor are still rejected with an error.
    /// Additionally, when a directory is encountered whose identity was
    /// already seen anywhere earlier in the scan, it is not descended
    /// into and is instead reported through
    /// [`IntoIter::skipped_dirs`] together with the path at which the
    /// directory was first visited.
    ///
    /// [`IntoIter::skipped_dirs`]: struct.IntoIter.html#method.skipped_dirs
    OncePerScan,
}

impl Default for DirIdentityPolicy {
    fn default() -> DirIdentityPolicy {
        DirIdentityPolicy::AncestorsOnly
    }
}

/// A directory that was skipped by [`DirIdentityPolicy::OncePerScan`].
///
/// [`DirIdentityPolicy::OncePerScan`]: enum.DirIdentityPolicy.html#variant.OncePerScan
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedDir {
    /// The path at which the duplicate directory was encountered.
    path: PathBuf,
    /// The path at which the same directory was first visited.
    first_path: PathBuf,
    /// The depth at which the duplicate directory was encountered.
    depth: usize,
}

impl SkippedDir {
    pub(crate) fn new(
        path: PathBuf,
        first_path: PathBuf,
        depth: usize,
    ) -> Self {
        SkippedDir { path, first_path, depth }
    }

    /// The path at which the duplicate directory was encountered.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path at which the same directory was first visited during
    /// this scan.
    pub fn first_path(&self) -> &Path {
        &self.first_path
    }

    /// The depth at which the duplicate directory was encountered,
    /// relative to the root of the walk.
    pub fn depth(&self) -> usize {
        self.depth
    }
}

/// Return the identity of the directory at `path` as reported by the
/// underlying file system.
///
/// Symbolic links in `path` are resolved, so an alias and its target
/// share the same identity.
#[cfg(unix)]
pub(crate) fn system_dir_id(path: &Path) -> io::Result<DirId> {
    use std::os::unix::fs::MetadataExt;

    let md = std::fs::metadata(path)?;
    Ok(DirId(DirIdInner::System { dev: md.dev(), ino: md.ino() }))
}

/// Return the identity of the directory at `path` as reported by the
/// underlying file system.
///
/// Symbolic links in `path` are resolved, so an alias and its target
/// share the same identity.
#[cfg(windows)]
pub(crate) fn system_dir_id(path: &Path) -> io::Result<DirId> {
    use winapi_util::{file, Handle};

    // `from_path_any` opens directories and follows symbolic links, so
    // the identity reported corresponds to the link target.
    let h = Handle::from_path_any(path)?;
    let info = file::information(h)?;
    Ok(DirId(DirIdInner::System {
        dev: info.volume_serial_number(),
        ino: info.file_index(),
    }))
}

/// Return the identity of the directory at `path`.
///
/// On platforms without a native directory identity, the canonicalized
/// path is used as a best effort fallback.
#[cfg(not(any(unix, windows)))]
pub(crate) fn system_dir_id(path: &Path) -> io::Result<DirId> {
    std::fs::canonicalize(path).map(|p| DirId(DirIdInner::Path(p)))
}
