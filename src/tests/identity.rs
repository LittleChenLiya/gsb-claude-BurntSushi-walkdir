use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use crate::tests::util::Dir;
use crate::{DirId, DirIdentifier, DirIdentityPolicy, WalkDir};

/// A fake directory identity provider for tests.
///
/// Paths registered with `alias` share the same identity, which
/// simulates distinct paths (e.g. symlinks, junctions or bind mounts)
/// that refer to the same underlying directory without requiring any
/// support from the platform. Unregistered paths receive a unique
/// identity derived from their path, and paths registered with `fail`
/// produce an error when identified.
struct FakeIdentifier {
    ids: HashMap<PathBuf, u64>,
    errs: Vec<PathBuf>,
    next: u64,
}

impl FakeIdentifier {
    fn new() -> FakeIdentifier {
        FakeIdentifier { ids: HashMap::new(), errs: vec![], next: 0 }
    }

    /// Map `path` to the same identity as `alias_of`.
    fn alias<P1: AsRef<Path>, P2: AsRef<Path>>(
        &mut self,
        path: P1,
        alias_of: P2,
    ) {
        let id = match self.ids.get(alias_of.as_ref()) {
            Some(&id) => id,
            None => {
                let id = self.next;
                self.next += 1;
                self.ids.insert(alias_of.as_ref().to_path_buf(), id);
                id
            }
        };
        self.ids.insert(path.as_ref().to_path_buf(), id);
    }

    /// Make identifying `path` fail with an IO error.
    fn fail<P: AsRef<Path>>(&mut self, path: P) {
        self.errs.push(path.as_ref().to_path_buf());
    }
}

impl DirIdentifier for FakeIdentifier {
    fn identify(&self, path: &Path) -> io::Result<DirId> {
        if self.errs.iter().any(|p| p == path) {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "fake identity error",
            ));
        }
        let id = match self.ids.get(path) {
            Some(&id) => id,
            None => default_id(path),
        };
        Ok(DirId::custom(id))
    }
}

/// Return an identity that is unique to this path and that can never
/// collide with an identity explicitly assigned by `FakeIdentifier`.
fn default_id(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish() | (1 << 63)
}

/// Return true if this process can create directory symlinks.
///
/// On Windows, creating symlinks usually requires elevated privileges or
/// developer mode, so the native symlink tests below probe for support
/// first and skip themselves when it is missing. The fake identifier
/// fixture used by the other tests in this module provides equivalent
/// coverage of the identity logic without relying on native links.
fn symlink_dir_supported() -> bool {
    let dir = Dir::tmp();
    dir.mkdirp("target");
    #[cfg(windows)]
    let result = {
        use std::os::windows::fs::symlink_dir;
        symlink_dir(dir.join("target"), dir.join("link"))
    };
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::symlink;
        symlink(dir.join("target"), dir.join("link"))
    };
    #[cfg(not(any(unix, windows)))]
    let result: io::Result<()> = Err(io::Error::new(
        io::ErrorKind::Other,
        "symlinks not supported on this platform",
    ));
    result.is_ok()
}

/// Skip the current test when directory symlinks cannot be created,
/// explaining which fixture covers the behavior instead.
macro_rules! require_symlink_dir {
    () => {
        if !symlink_dir_supported() {
            eprintln!(
                "skipping: directory symlinks are not supported here; \
                 the fake-identifier tests in this module cover the same \
                 identity logic without native links"
            );
            return;
        }
    };
}

#[test]
fn identity_global_dedup_visits_dir_once_sorted() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.mkdirp("b");
    dir.touch("a/af");
    dir.touch("b/bf");

    // `a` and `b` are aliases of the same directory.
    let mut fake = FakeIdentifier::new();
    fake.alias(dir.join("b"), dir.join("a"));

    let wd = WalkDir::new(dir.path())
        .sort_by_file_name()
        .dir_identifier(fake)
        .dir_identity_policy(DirIdentityPolicy::OncePerScan);
    let mut it = wd.into_iter();
    let r = dir.run_recursive(&mut it);
    r.assert_no_errors();

    // The shared directory is traversed exactly once (through `a`, which
    // sorts first), in a fully deterministic order.
    assert_eq!(
        vec![dir.path().to_path_buf(), dir.join("a"), dir.join("a/af")],
        r.paths()
    );
    // The second encounter (`b`) is reported as skipped, together with
    // the path at which the directory was first visited.
    let skipped = it.skipped_dirs();
    assert_eq!(1, skipped.len());
    assert_eq!(dir.join("b"), skipped[0].path());
    assert_eq!(dir.join("a"), skipped[0].first_path());
    assert_eq!(1, skipped[0].depth());
}

#[test]
fn identity_ancestors_only_allows_distinct_branches() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.mkdirp("b");
    dir.touch("a/af");
    dir.touch("b/bf");

    // Same fixture as above, but with the ancestor-only policy both
    // branches are visited even though they share an identity.
    let mut fake = FakeIdentifier::new();
    fake.alias(dir.join("b"), dir.join("a"));

    let wd = WalkDir::new(dir.path())
        .sort_by_file_name()
        .dir_identifier(fake)
        .dir_identity_policy(DirIdentityPolicy::AncestorsOnly);
    let mut it = wd.into_iter();
    let r = dir.run_recursive(&mut it);
    r.assert_no_errors();

    assert_eq!(
        vec![
            dir.path().to_path_buf(),
            dir.join("a"),
            dir.join("a/af"),
            dir.join("b"),
            dir.join("b/bf"),
        ],
        r.paths()
    );
    assert!(it.skipped_dirs().is_empty());
}

#[test]
fn identity_loop_to_root_rejected_in_both_policies() {
    let dir = Dir::tmp();
    dir.mkdirp("a/sub");
    dir.touch("a/sub/leaf");

    for &policy in
        &[DirIdentityPolicy::AncestorsOnly, DirIdentityPolicy::OncePerScan]
    {
        // `a/sub` "points back" to the root of the walk.
        let mut fake = FakeIdentifier::new();
        fake.alias(dir.join("a/sub"), dir.path());

        let wd = WalkDir::new(dir.path())
            .dir_identifier(fake)
            .dir_identity_policy(policy);
        let r = dir.run_recursive(wd);

        assert_eq!(1, r.errs().len(), "policy: {:?}", policy);
        let err = &r.errs()[0];
        assert_eq!(dir.path(), err.loop_ancestor().unwrap());
        assert_eq!(dir.join("a/sub"), err.path().unwrap());
        // The loop is rejected: `a/sub` is neither yielded nor descended.
        assert_eq!(
            vec![dir.path().to_path_buf(), dir.join("a")],
            r.paths(),
            "policy: {:?}",
            policy
        );
    }
}

#[test]
fn identity_error_is_yielded_and_dir_not_descended() {
    let dir = Dir::tmp();
    dir.mkdirp("bad");
    dir.touch("bad/hidden");
    dir.mkdirp("good");
    dir.touch("good/visible");

    let mut fake = FakeIdentifier::new();
    fake.fail(dir.join("bad"));

    let wd = WalkDir::new(dir.path())
        .sort_by_file_name()
        .dir_identifier(fake)
        .dir_identity_policy(DirIdentityPolicy::OncePerScan);
    let r = dir.run_recursive(wd);

    // The identity error is delivered like any other IO error and is
    // not swallowed.
    assert_eq!(1, r.errs().len());
    let err = &r.errs()[0];
    assert_eq!(dir.join("bad"), err.path().unwrap());
    assert!(err.io_error().is_some());
    assert_eq!("fake identity error", err.io_error().unwrap().to_string());

    // The failed directory is not treated as a new directory: it is
    // neither yielded nor descended into, but the walk continues.
    assert_eq!(
        vec![
            dir.path().to_path_buf(),
            dir.join("good"),
            dir.join("good/visible"),
        ],
        r.paths()
    );
}

#[test]
fn identity_global_dedup_respects_max_depth() {
    let dir = Dir::tmp();
    dir.mkdirp("a/aa");
    dir.touch("a/aa/leaf");
    dir.mkdirp("b");
    dir.touch("b/bf");

    let mut fake = FakeIdentifier::new();
    fake.alias(dir.join("b"), dir.join("a"));

    let wd = WalkDir::new(dir.path())
        .max_depth(1)
        .sort_by_file_name()
        .dir_identifier(fake)
        .dir_identity_policy(DirIdentityPolicy::OncePerScan);
    let mut it = wd.into_iter();
    let r = dir.run_recursive(&mut it);
    r.assert_no_errors();

    // Depth is limited to 1, so `a/aa` is never descended into, while
    // the duplicate `b` is still recognized and skipped.
    assert_eq!(vec![dir.path().to_path_buf(), dir.join("a")], r.paths());
    let skipped = it.skipped_dirs();
    assert_eq!(1, skipped.len());
    assert_eq!(dir.join("b"), skipped[0].path());
    assert_eq!(dir.join("a"), skipped[0].first_path());
}

#[test]
fn identity_native_symlink_global_dedup() {
    require_symlink_dir!();
    let dir = Dir::tmp();
    dir.mkdirp("real");
    dir.touch("real/f");
    // `zlink` sorts after `real`, so `real` is visited first.
    dir.symlink_dir("real", "zlink");

    let wd = WalkDir::new(dir.path())
        .follow_links(true)
        .sort_by_file_name()
        .dir_identity_policy(DirIdentityPolicy::OncePerScan);
    let mut it = wd.into_iter();
    let r = dir.run_recursive(&mut it);
    r.assert_no_errors();

    // The directory behind the symlink is traversed once, through the
    // real path; the link itself is reported as skipped.
    assert_eq!(
        vec![dir.path().to_path_buf(), dir.join("real"), dir.join("real/f")],
        r.paths()
    );
    let skipped = it.skipped_dirs();
    assert_eq!(1, skipped.len());
    assert_eq!(dir.join("zlink"), skipped[0].path());
    assert_eq!(dir.join("real"), skipped[0].first_path());
}

#[test]
fn identity_native_symlink_loop_rejected_in_both_policies() {
    require_symlink_dir!();
    let dir = Dir::tmp();
    dir.mkdirp("sub");
    // `sub/loop` points back to the root of the walk.
    dir.symlink_dir(".", "sub/loop");

    for &policy in
        &[DirIdentityPolicy::AncestorsOnly, DirIdentityPolicy::OncePerScan]
    {
        let wd = WalkDir::new(dir.path())
            .follow_links(true)
            .dir_identity_policy(policy);
        let r = dir.run_recursive(wd);

        assert_eq!(1, r.errs().len(), "policy: {:?}", policy);
        let err = &r.errs()[0];
        assert_eq!(dir.path(), err.loop_ancestor().unwrap());
        assert_eq!(dir.join("sub/loop"), err.path().unwrap());
        assert_eq!(
            vec![dir.path().to_path_buf(), dir.join("sub")],
            r.sorted_paths(),
            "policy: {:?}",
            policy
        );
    }
}

#[test]
fn identity_default_behavior_visits_aliases() {
    require_symlink_dir!();
    let dir = Dir::tmp();
    dir.mkdirp("real");
    dir.touch("real/f");
    dir.symlink_dir("real", "zlink");

    // Without an explicit identity policy, the default WalkDir behavior
    // is unchanged: the directory is visited under both names.
    let wd = WalkDir::new(dir.path()).follow_links(true).sort_by_file_name();
    let r = dir.run_recursive(wd);
    r.assert_no_errors();
    assert_eq!(
        vec![
            dir.path().to_path_buf(),
            dir.join("real"),
            dir.join("real/f"),
            dir.join("zlink"),
            dir.join("zlink/f"),
        ],
        r.paths()
    );
}
