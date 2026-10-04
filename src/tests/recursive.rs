use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::tests::util::Dir;
use crate::{
    DirIdentity, DirIdentityPolicy, DirIdentityProvider, WalkDir,
};

#[test]
fn send_sync_traits() {
    use crate::{FilterEntry, IntoIter};

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    assert_send::<WalkDir>();
    assert_sync::<WalkDir>();
    assert_send::<IntoIter>();
    assert_sync::<IntoIter>();
    assert_send::<FilterEntry<IntoIter, u8>>();
    assert_sync::<FilterEntry<IntoIter, u8>>();
}

#[test]
fn empty() {
    let dir = Dir::tmp();
    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(1, r.ents().len());
    let ent = &r.ents()[0];
    assert!(ent.file_type().is_dir());
    assert!(!ent.path_is_symlink());
    assert_eq!(0, ent.depth());
    assert_eq!(dir.path(), ent.path());
    assert_eq!(dir.path().file_name().unwrap(), ent.file_name());
}

#[test]
fn empty_follow() {
    let dir = Dir::tmp();
    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(1, r.ents().len());
    let ent = &r.ents()[0];
    assert!(ent.file_type().is_dir());
    assert!(!ent.path_is_symlink());
    assert_eq!(0, ent.depth());
    assert_eq!(dir.path(), ent.path());
    assert_eq!(dir.path().file_name().unwrap(), ent.file_name());
}

#[test]
fn empty_file() {
    let dir = Dir::tmp();
    dir.touch("a");

    let wd = WalkDir::new(dir.path().join("a"));
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(1, r.ents().len());
    let ent = &r.ents()[0];
    assert!(ent.file_type().is_file());
    assert!(!ent.path_is_symlink());
    assert_eq!(0, ent.depth());
    assert_eq!(dir.join("a"), ent.path());
    assert_eq!("a", ent.file_name());
}

#[test]
fn empty_file_follow() {
    let dir = Dir::tmp();
    dir.touch("a");

    let wd = WalkDir::new(dir.path().join("a")).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(1, r.ents().len());
    let ent = &r.ents()[0];
    assert!(ent.file_type().is_file());
    assert!(!ent.path_is_symlink());
    assert_eq!(0, ent.depth());
    assert_eq!(dir.join("a"), ent.path());
    assert_eq!("a", ent.file_name());
}

#[test]
fn one_dir() {
    let dir = Dir::tmp();
    dir.mkdirp("a");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(2, ents.len());
    let ent = &ents[1];
    assert_eq!(dir.join("a"), ent.path());
    assert_eq!(1, ent.depth());
    assert_eq!("a", ent.file_name());
    assert!(ent.file_type().is_dir());
}

#[test]
fn one_file() {
    let dir = Dir::tmp();
    dir.touch("a");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(2, ents.len());
    let ent = &ents[1];
    assert_eq!(dir.join("a"), ent.path());
    assert_eq!(1, ent.depth());
    assert_eq!("a", ent.file_name());
    assert!(ent.file_type().is_file());
}

#[test]
fn one_dir_one_file() {
    let dir = Dir::tmp();
    dir.mkdirp("foo");
    dir.touch("foo/a");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("a"),
    ];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn many_files() {
    let dir = Dir::tmp();
    dir.mkdirp("foo");
    dir.touch_all(&["foo/a", "foo/b", "foo/c"]);

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("a"),
        dir.join("foo").join("b"),
        dir.join("foo").join("c"),
    ];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn many_dirs() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/a");
    dir.mkdirp("foo/b");
    dir.mkdirp("foo/c");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("a"),
        dir.join("foo").join("b"),
        dir.join("foo").join("c"),
    ];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn many_mixed() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/a");
    dir.mkdirp("foo/c");
    dir.mkdirp("foo/e");
    dir.touch_all(&["foo/b", "foo/d", "foo/f"]);

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("a"),
        dir.join("foo").join("b"),
        dir.join("foo").join("c"),
        dir.join("foo").join("d"),
        dir.join("foo").join("e"),
        dir.join("foo").join("f"),
    ];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn nested() {
    let nested =
        PathBuf::from("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z");
    let dir = Dir::tmp();
    dir.mkdirp(&nested);
    dir.touch(nested.join("A"));

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("a"),
        dir.join("a/b"),
        dir.join("a/b/c"),
        dir.join("a/b/c/d"),
        dir.join("a/b/c/d/e"),
        dir.join("a/b/c/d/e/f"),
        dir.join("a/b/c/d/e/f/g"),
        dir.join("a/b/c/d/e/f/g/h"),
        dir.join("a/b/c/d/e/f/g/h/i"),
        dir.join("a/b/c/d/e/f/g/h/i/j"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z"),
        dir.join(&nested).join("A"),
    ];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn nested_small_max_open() {
    let nested =
        PathBuf::from("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z");
    let dir = Dir::tmp();
    dir.mkdirp(&nested);
    dir.touch(nested.join("A"));

    let wd = WalkDir::new(dir.path()).max_open(1);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("a"),
        dir.join("a/b"),
        dir.join("a/b/c"),
        dir.join("a/b/c/d"),
        dir.join("a/b/c/d/e"),
        dir.join("a/b/c/d/e/f"),
        dir.join("a/b/c/d/e/f/g"),
        dir.join("a/b/c/d/e/f/g/h"),
        dir.join("a/b/c/d/e/f/g/h/i"),
        dir.join("a/b/c/d/e/f/g/h/i/j"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z"),
        dir.join(&nested).join("A"),
    ];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn siblings() {
    let dir = Dir::tmp();
    dir.mkdirp("foo");
    dir.mkdirp("bar");
    dir.touch_all(&["foo/a", "foo/b"]);
    dir.touch_all(&["bar/a", "bar/b"]);

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("bar"),
        dir.join("bar").join("a"),
        dir.join("bar").join("b"),
        dir.join("foo"),
        dir.join("foo").join("a"),
        dir.join("foo").join("b"),
    ];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn sym_root_file_nofollow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.touch("a");
    dir.symlink_file("a", "a-link");

    let wd = WalkDir::new(dir.join("a-link"));
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.sorted_ents();
    assert_eq!(1, ents.len());
    let link = &ents[0];

    assert_eq!(dir.join("a-link"), link.path());

    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(0, link.depth());

    assert!(link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[test]
fn sym_root_file_follow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.touch("a");
    dir.symlink_file("a", "a-link");

    let wd = WalkDir::new(dir.join("a-link")).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.sorted_ents();
    let link = &ents[0];

    assert_eq!(dir.join("a-link"), link.path());

    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(0, link.depth());

    assert!(!link.file_type().is_symlink());
    assert!(link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(!link.metadata().unwrap().file_type().is_symlink());
    assert!(link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[test]
fn broken_sym_root_dir_nofollow_and_root_nofollow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.symlink_dir("broken", "a-link");

    let wd = WalkDir::new(dir.join("a-link"))
        .follow_links(false)
        .follow_root_links(false);
    let r = dir.run_recursive(wd);
    let ents = r.sorted_ents();
    assert_eq!(ents.len(), 1);
    let link = &ents[0];
    assert_eq!(dir.join("a-link"), link.path());
    assert!(link.path_is_symlink());
}

#[test]
fn broken_sym_root_dir_follow_and_root_nofollow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.symlink_dir("broken", "a-link");

    let wd = WalkDir::new(dir.join("a-link"))
        .follow_links(true)
        .follow_root_links(false);
    let r = dir.run_recursive(wd);
    assert!(r.sorted_ents().is_empty());
    assert_eq!(
        r.errs().len(),
        1,
        "broken symlink cannot be traversed - they are followed if symlinks are followed"
    );
}

#[test]
fn broken_sym_root_dir_root_is_always_followed() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.symlink_dir("broken", "a-link");

    for follow_symlinks in &[true, false] {
        let wd =
            WalkDir::new(dir.join("a-link")).follow_links(*follow_symlinks);
        let r = dir.run_recursive(wd);
        assert!(r.sorted_ents().is_empty());
        assert_eq!(
            r.errs().len(),
            1,
            "broken symlink in roots cannot be traversed, they are always followed"
        );
    }
}

#[test]
fn sym_root_dir_nofollow_root_nofollow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "a-link");
    dir.touch("a/zzz");

    let wd = WalkDir::new(dir.join("a-link")).follow_root_links(false);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.sorted_ents();
    assert_eq!(1, ents.len());
    let link = &ents[0];
    assert_eq!(dir.join("a-link"), link.path());
    assert_eq!(0, link.depth());
}

#[test]
fn sym_root_dir_nofollow_root_follow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "a-link");
    dir.touch("a/zzz");

    let wd = WalkDir::new(dir.join("a-link"));
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.sorted_ents();
    assert_eq!(2, ents.len());
    let link = &ents[0];

    assert_eq!(dir.join("a-link"), link.path());

    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(0, link.depth());

    assert!(link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());

    let link_zzz = &ents[1];
    assert_eq!(dir.join("a-link").join("zzz"), link_zzz.path());
    assert!(!link_zzz.path_is_symlink());
}

#[test]
fn sym_root_dir_follow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "a-link");
    dir.touch("a/zzz");

    let wd = WalkDir::new(dir.join("a-link")).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.sorted_ents();
    assert_eq!(2, ents.len());
    let link = &ents[0];

    assert_eq!(dir.join("a-link"), link.path());

    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(0, link.depth());

    assert!(!link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(link.file_type().is_dir());

    assert!(!link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(link.metadata().unwrap().is_dir());

    let link_zzz = &ents[1];
    assert_eq!(dir.join("a-link").join("zzz"), link_zzz.path());
    assert!(!link_zzz.path_is_symlink());
}

#[test]
fn sym_file_nofollow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.touch("a");
    dir.symlink_file("a", "a-link");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.sorted_ents();
    assert_eq!(3, ents.len());
    let (src, link) = (&ents[1], &ents[2]);

    assert_eq!(dir.join("a"), src.path());
    assert_eq!(dir.join("a-link"), link.path());

    assert!(!src.path_is_symlink());
    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(1, src.depth());
    assert_eq!(1, link.depth());

    assert!(src.file_type().is_file());
    assert!(link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(src.metadata().unwrap().is_file());
    assert!(link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[test]
fn sym_file_follow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.touch("a");
    dir.symlink_file("a", "a-link");

    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.sorted_ents();
    assert_eq!(3, ents.len());
    let (src, link) = (&ents[1], &ents[2]);

    assert_eq!(dir.join("a"), src.path());
    assert_eq!(dir.join("a-link"), link.path());

    assert!(!src.path_is_symlink());
    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(1, src.depth());
    assert_eq!(1, link.depth());

    assert!(src.file_type().is_file());
    assert!(!link.file_type().is_symlink());
    assert!(link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(src.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().file_type().is_symlink());
    assert!(link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[test]
fn sym_dir_nofollow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "a-link");
    dir.touch("a/zzz");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.sorted_ents();
    assert_eq!(4, ents.len());
    let (src, link) = (&ents[1], &ents[3]);

    assert_eq!(dir.join("a"), src.path());
    assert_eq!(dir.join("a-link"), link.path());

    assert!(!src.path_is_symlink());
    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(1, src.depth());
    assert_eq!(1, link.depth());

    assert!(src.file_type().is_dir());
    assert!(link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(src.metadata().unwrap().is_dir());
    assert!(link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[test]
fn sym_dir_follow() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "a-link");
    dir.touch("a/zzz");

    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.sorted_ents();
    assert_eq!(5, ents.len());
    let (src, link) = (&ents[1], &ents[3]);

    assert_eq!(dir.join("a"), src.path());
    assert_eq!(dir.join("a-link"), link.path());

    assert!(!src.path_is_symlink());
    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(1, src.depth());
    assert_eq!(1, link.depth());

    assert!(src.file_type().is_dir());
    assert!(!link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(link.file_type().is_dir());

    assert!(src.metadata().unwrap().is_dir());
    assert!(!link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(link.metadata().unwrap().is_dir());

    let (src_zzz, link_zzz) = (&ents[2], &ents[4]);
    assert_eq!(dir.join("a").join("zzz"), src_zzz.path());
    assert_eq!(dir.join("a-link").join("zzz"), link_zzz.path());
    assert!(!src_zzz.path_is_symlink());
    assert!(!link_zzz.path_is_symlink());
}

#[test]
fn sym_noloop() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.symlink_dir("a", "a/b/c/a-link");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    // There's no loop if we aren't following symlinks.
    r.assert_no_errors();

    assert_eq!(5, r.ents().len());
}

#[test]
fn sym_loop_detect() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.symlink_dir("a", "a/b/c/a-link");

    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);

    let (ents, errs) = (r.sorted_ents(), r.errs());
    assert_eq!(4, ents.len());
    assert_eq!(1, errs.len());

    let err = &errs[0];

    let expected = dir.join("a/b/c/a-link");
    assert_eq!(Some(&*expected), err.path());

    let expected = dir.join("a");
    assert_eq!(Some(&*expected), err.loop_ancestor());

    assert_eq!(4, err.depth());
    assert!(err.io_error().is_none());
}

#[test]
fn sym_self_loop_no_error() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.symlink_file("a", "a");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    // No errors occur because even though the symlink points to nowhere, it
    // is never followed, and thus no error occurs.
    r.assert_no_errors();
    assert_eq!(2, r.ents().len());

    let ent = &r.ents()[1];
    assert_eq!(dir.join("a"), ent.path());
    assert!(ent.path_is_symlink());

    assert!(ent.file_type().is_symlink());
    assert!(!ent.file_type().is_file());
    assert!(!ent.file_type().is_dir());

    assert!(ent.metadata().unwrap().file_type().is_symlink());
    assert!(!ent.metadata().unwrap().file_type().is_file());
    assert!(!ent.metadata().unwrap().file_type().is_dir());
}

#[test]
fn sym_file_self_loop_io_error() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.symlink_file("a", "a");

    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);

    let (ents, errs) = (r.sorted_ents(), r.errs());
    assert_eq!(1, ents.len());
    assert_eq!(1, errs.len());

    let err = &errs[0];

    let expected = dir.join("a");
    assert_eq!(Some(&*expected), err.path());
    assert_eq!(1, err.depth());
    assert!(err.loop_ancestor().is_none());
    assert!(err.io_error().is_some());
}

#[test]
fn sym_dir_self_loop_io_error() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.symlink_dir("a", "a");

    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);

    let (ents, errs) = (r.sorted_ents(), r.errs());
    assert_eq!(1, ents.len());
    assert_eq!(1, errs.len());

    let err = &errs[0];

    let expected = dir.join("a");
    assert_eq!(Some(&*expected), err.path());
    assert_eq!(1, err.depth());
    assert!(err.loop_ancestor().is_none());
    assert!(err.io_error().is_some());
}

#[test]
fn min_depth_1() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).min_depth(1);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a"), dir.join("a").join("b")];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn min_depth_2() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).min_depth(2);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a").join("b")];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn max_depth_0() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).max_depth(0);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.path().to_path_buf()];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn max_depth_1() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).max_depth(1);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.path().to_path_buf(), dir.join("a")];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn max_depth_2() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).max_depth(2);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected =
        vec![dir.path().to_path_buf(), dir.join("a"), dir.join("a").join("b")];
    assert_eq!(expected, r.sorted_paths());
}

// FIXME: This test seems wrong. It should return nothing!
#[test]
fn min_max_depth_diff_nada() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");

    let wd = WalkDir::new(dir.path()).min_depth(3).max_depth(2);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a").join("b").join("c")];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn min_max_depth_diff_0() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");

    let wd = WalkDir::new(dir.path()).min_depth(2).max_depth(2);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a").join("b")];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn min_max_depth_diff_1() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");

    let wd = WalkDir::new(dir.path()).min_depth(1).max_depth(2);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a"), dir.join("a").join("b")];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn contents_first() {
    let dir = Dir::tmp();
    dir.touch("a");

    let wd = WalkDir::new(dir.path()).contents_first(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a"), dir.path().to_path_buf()];
    assert_eq!(expected, r.paths());
}

#[test]
fn skip_current_dir() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/bar/baz");
    dir.mkdirp("quux");

    let mut paths = vec![];
    let mut it = WalkDir::new(dir.path()).into_iter();
    while let Some(result) = it.next() {
        let ent = result.unwrap();
        paths.push(ent.path().to_path_buf());
        if ent.file_name() == "bar" {
            it.skip_current_dir();
        }
    }
    paths.sort();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("bar"),
        dir.join("quux"),
    ];
    assert_eq!(expected, paths);
}

#[test]
fn filter_entry() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/bar/baz/abc");
    dir.mkdirp("quux");

    let wd = WalkDir::new(dir.path())
        .into_iter()
        .filter_entry(|ent| ent.file_name() != "baz");
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("bar"),
        dir.join("quux"),
    ];
    assert_eq!(expected, r.sorted_paths());
}

#[test]
fn sort_by() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/bar/baz/abc");
    dir.mkdirp("quux");

    let wd = WalkDir::new(dir.path())
        .sort_by(|a, b| a.file_name().cmp(b.file_name()).reverse());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("quux"),
        dir.join("foo"),
        dir.join("foo").join("bar"),
        dir.join("foo").join("bar").join("baz"),
        dir.join("foo").join("bar").join("baz").join("abc"),
    ];
    assert_eq!(expected, r.paths());
}

#[test]
fn sort_by_key() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/bar/baz/abc");
    dir.mkdirp("quux");

    let wd =
        WalkDir::new(dir.path()).sort_by_key(|a| a.file_name().to_owned());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("bar"),
        dir.join("foo").join("bar").join("baz"),
        dir.join("foo").join("bar").join("baz").join("abc"),
        dir.join("quux"),
    ];
    assert_eq!(expected, r.paths());
}

#[test]
fn sort_by_file_name() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/bar/baz/abc");
    dir.mkdirp("quux");

    let wd = WalkDir::new(dir.path()).sort_by_file_name();
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("bar"),
        dir.join("foo").join("bar").join("baz"),
        dir.join("foo").join("bar").join("baz").join("abc"),
        dir.join("quux"),
    ];
    assert_eq!(expected, r.paths());
}

#[test]
fn sort_max_open() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/bar/baz/abc");
    dir.mkdirp("quux");

    let wd = WalkDir::new(dir.path())
        .max_open(1)
        .sort_by(|a, b| a.file_name().cmp(b.file_name()).reverse());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("quux"),
        dir.join("foo"),
        dir.join("foo").join("bar"),
        dir.join("foo").join("bar").join("baz"),
        dir.join("foo").join("bar").join("baz").join("abc"),
    ];
    assert_eq!(expected, r.paths());
}

#[cfg(target_os = "linux")]
#[test]
fn same_file_system() {
    use std::path::Path;

    // This test is a little weird since it's not clear whether it's a good
    // idea to setup a distinct mounted volume in these tests. Instead, we
    // probe for an existing one.
    if !Path::new("/sys").is_dir() {
        return;
    }

    let dir = Dir::tmp();
    dir.touch("a");
    dir.symlink_dir("/sys", "sys-link");

    // First, do a sanity check that things work without following symlinks.
    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected =
        vec![dir.path().to_path_buf(), dir.join("a"), dir.join("sys-link")];
    assert_eq!(expected, r.sorted_paths());

    // ... now follow symlinks and ensure we don't descend into /sys.
    let wd =
        WalkDir::new(dir.path()).same_file_system(true).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected =
        vec![dir.path().to_path_buf(), dir.join("a"), dir.join("sys-link")];
    assert_eq!(expected, r.sorted_paths());
}

// Tests that skip_current_dir doesn't destroy internal invariants.
//
// See: https://github.com/BurntSushi/walkdir/issues/118
#[test]
fn regression_skip_current_dir() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/a/b");
    dir.mkdirp("foo/1/2");

    let mut wd = WalkDir::new(dir.path()).max_open(1).into_iter();
    wd.next();
    wd.next();
    wd.next();
    wd.next();

    wd.skip_current_dir();
    wd.skip_current_dir();
    wd.next();
}

// ---------------------------------------------------------------------------
// Directory identity policy tests
//
// These tests exercise the directory identity policy. Because native
// symbolic links require special privileges on some platforms (notably
// Windows), the primary fixture is a *fake* identity provider that maps
// chosen paths to chosen identities. This lets us simulate aliases (two
// distinct paths that point to the same directory) and loops without
// requiring real symlink support. Native symlink tests, gated by
// `skip_without_symlinks!()`, are provided where useful.
// ---------------------------------------------------------------------------

/// A fake directory identity provider for testing.
///
/// Paths registered with [`FakeIdentityProvider::with_id``] are mapped to a
/// fixed custom identity. Paths registered with
/// [`FakeIdentityProvider::with_error`] return an error when their identity is
/// requested. All other paths use the real file system identity.
#[derive(Debug, Default)]
struct FakeIdentityProvider {
    ids: HashMap<PathBuf, u64>,
    error_paths: Vec<PathBuf>,
}

impl FakeIdentityProvider {
    fn new() -> Self {
        Self::default()
    }

    fn with_id<P: AsRef<Path>>(mut self, path: P, id: u64) -> Self {
        self.ids.insert(path.as_ref().to_path_buf(), id);
        self
    }

    fn with_error<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.error_paths.push(path.as_ref().to_path_buf());
        self
    }
}

impl DirIdentityProvider for FakeIdentityProvider {
    fn identity(&self, path: &Path) -> io::Result<DirIdentity> {
        if self.error_paths.iter().any(|p| p == path) {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "fake identity provider error",
            ));
        }
        if let Some(&id) = self.ids.get(path) {
            return Ok(DirIdentity::custom(id));
        }
        DirIdentity::from_path(path)
    }
}

/// In global mode, two distinct paths with the same identity are visited only
/// once. The skipped entry and the first path are reported.
#[test]
fn dir_identity_global_dedup() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.mkdirp("b");
    dir.touch_all(&["a/a1", "b/b1"]);

    // a and b are distinct directories on disk, but the fake provider maps
    // them to the same identity.
    let provider = FakeIdentityProvider::new()
        .with_id(dir.join("a"), 42)
        .with_id(dir.join("b"), 42);

    let wd = WalkDir::new(dir.path())
        .follow_links(true)
        .dir_identity_policy(DirIdentityPolicy::Global)
        .dir_identity_provider(provider)
        .sort_by_file_name();
    let r = dir.run_recursive(wd);

    // b is skipped because it has the same identity as a.
    let paths = r.sorted_paths();
    assert_eq!(
        paths,
        vec![dir.path().to_path_buf(), dir.join("a"), dir.join("a/a1"),]
    );

    // One duplicate error reporting b as the skipped entry and a as the
    // first path.
    assert_eq!(r.errs().len(), 1);
    let err = &r.errs()[0];
    assert_eq!(err.path(), Some(dir.join("b").as_path()));
    assert_eq!(err.duplicate_first_path(), Some(dir.join("a").as_path()));
    assert!(err.loop_ancestor().is_none());
    assert!(err.io_error().is_none());
}

/// In ancestor mode, two distinct paths with the same identity may both be
/// visited as long as they are in different branches.
#[test]
fn dir_identity_ancestor_allows_branches() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.mkdirp("b");
    dir.touch_all(&["a/a1", "b/b1"]);

    let provider = FakeIdentityProvider::new()
        .with_id(dir.join("a"), 42)
        .with_id(dir.join("b"), 42);

    let wd = WalkDir::new(dir.path())
        .follow_links(true)
        .dir_identity_policy(DirIdentityPolicy::Ancestor)
        .dir_identity_provider(provider)
        .sort_by_file_name();
    let r = dir.run_recursive(wd);

    // Both a and b are visited because they are in different branches.
    r.assert_no_errors();
    let paths = r.sorted_paths();
    assert_eq!(
        paths,
        vec![
            dir.path().to_path_buf(),
            dir.join("a"),
            dir.join("a/a1"),
            dir.join("b"),
            dir.join("b/b1"),
        ]
    );
}

/// A directory that points back to the root (an ancestor) is rejected as a
/// loop in both ancestor and global modes.
///
/// This uses the fake provider to simulate a symlink from `a/back` to the
/// root: `a/back` is given the same identity as the root. The native
/// equivalent is tested in `dir_identity_native_symlink_loop`.
#[test]
fn dir_identity_loop_to_root_both_modes() {
    let dir = Dir::tmp();
    dir.mkdirp("a/back");

    for policy in &[DirIdentityPolicy::Ancestor, DirIdentityPolicy::Global] {
        let provider = FakeIdentityProvider::new()
            .with_id(dir.path(), 1) // root
            .with_id(dir.join("a"), 2) // a
            .with_id(dir.join("a/back"), 1); // a/back "points back to" root

        let wd = WalkDir::new(dir.path())
            .follow_links(true)
            .dir_identity_policy(*policy)
            .dir_identity_provider(provider)
            .sort_by_file_name();
        let r = dir.run_recursive(wd);

        // a/back is rejected because it points back to the root (an ancestor).
        let paths = r.sorted_paths();
        assert_eq!(
            paths,
            vec![dir.path().to_path_buf(), dir.join("a"),]
        );

        assert_eq!(r.errs().len(), 1);
        let err = &r.errs()[0];
        assert_eq!(err.path(), Some(dir.join("a/back").as_path()));
        assert_eq!(err.loop_ancestor(), Some(dir.path()));
        assert!(err.duplicate_first_path().is_none());
        assert!(err.io_error().is_none());
    }
}

/// An identity provider failure is delivered through the error channel and
/// does not cause the directory to be descended into.
#[test]
fn dir_identity_error_not_swallowed() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.mkdirp("b");

    let provider =
        FakeIdentityProvider::new().with_error(dir.join("b"));

    let wd = WalkDir::new(dir.path())
        .follow_links(true)
        .dir_identity_policy(DirIdentityPolicy::Global)
        .dir_identity_provider(provider)
        .sort_by_file_name();
    let r = dir.run_recursive(wd);

    // b is not descended into because its identity cannot be determined.
    let paths = r.sorted_paths();
    assert_eq!(
        paths,
        vec![dir.path().to_path_buf(), dir.join("a"),]
    );

    // The identity error is delivered (not swallowed).
    assert_eq!(r.errs().len(), 1);
    let err = &r.errs()[0];
    assert_eq!(err.path(), Some(dir.join("b").as_path()));
    assert!(err.io_error().is_some());
    assert_eq!(err.io_error().unwrap().kind(), io::ErrorKind::Other);
}

/// The max_depth option continues to work when the identity policy is enabled.
#[test]
fn dir_identity_max_depth() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");

    let wd = WalkDir::new(dir.path())
        .follow_links(true)
        .max_depth(2)
        .dir_identity_policy(DirIdentityPolicy::Global)
        .sort_by_file_name();
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let paths = r.sorted_paths();
    assert_eq!(
        paths,
        vec![dir.path().to_path_buf(), dir.join("a"), dir.join("a/b"),]
    );
}

/// Entries are yielded in depth-first, sorted order when the identity policy
/// is enabled. The duplicate error is yielded in the order in which the
/// duplicate entry is encountered.
#[test]
fn dir_identity_order() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.mkdirp("b");
    dir.touch_all(&["a/a1", "b/b1"]);

    let provider = FakeIdentityProvider::new()
        .with_id(dir.join("a"), 42)
        .with_id(dir.join("b"), 42);

    let wd = WalkDir::new(dir.path())
        .follow_links(true)
        .dir_identity_policy(DirIdentityPolicy::Global)
        .dir_identity_provider(provider)
        .sort_by_file_name();

    // Collect entries and errors in the order in which they are yielded.
    let mut items: Vec<String> = vec![];
    for result in wd {
        match result {
            Ok(ent) => items.push(format!("ok:{}", ent.path().display())),
            Err(err) => {
                items.push(format!("err:{}", err.path().unwrap().display()))
            }
        }
    }

    assert_eq!(
        items,
        vec![
            format!("ok:{}", dir.path().display()),
            format!("ok:{}", dir.join("a").display()),
            format!("ok:{}", dir.join("a").join("a1").display()),
            format!("err:{}", dir.join("b").display()),
        ]
    );
}

/// With `contents_first`, a directory's contents are yielded before the
/// directory itself. The identity policy continues to detect duplicates.
#[test]
fn dir_identity_contents_first() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.mkdirp("b");
    dir.touch_all(&["a/a1", "b/b1"]);

    let provider = FakeIdentityProvider::new()
        .with_id(dir.join("a"), 42)
        .with_id(dir.join("b"), 42);

    let wd = WalkDir::new(dir.path())
        .follow_links(true)
        .contents_first(true)
        .dir_identity_policy(DirIdentityPolicy::Global)
        .dir_identity_provider(provider)
        .sort_by_file_name();
    let r = dir.run_recursive(wd);

    // a's contents are yielded before a itself. b is a duplicate of a and is
    // skipped (the error is yielded in the order in which b is encountered).
    let paths = r.paths();
    assert_eq!(
        paths,
        vec![
            dir.join("a").join("a1"),
            dir.join("a"),
            dir.path().to_path_buf(),
        ]
    );

    assert_eq!(r.errs().len(), 1);
    let err = &r.errs()[0];
    assert_eq!(err.path(), Some(dir.join("b").as_path()));
    assert_eq!(err.duplicate_first_path(), Some(dir.join("a").as_path()));
}

/// Native symlink test: a symlink from `a/back` to the root is rejected as a
/// loop in both ancestor and global modes.
///
/// This test only runs when the system supports symbolic links. On systems
/// that do not (for example, Windows without the symlink privilege or
/// developer mode), the fake identity provider fixture in
/// `dir_identity_loop_to_root_both_modes` provides equivalent coverage.
#[test]
fn dir_identity_native_symlink_loop() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.symlink_dir(".", "a/back"); // a/back -> root

    for policy in &[DirIdentityPolicy::Ancestor, DirIdentityPolicy::Global] {
        let wd = WalkDir::new(dir.path())
            .follow_links(true)
            .dir_identity_policy(*policy)
            .sort_by_file_name();
        let r = dir.run_recursive(wd);

        // a/back points back to the root, which is an ancestor.
        let paths = r.sorted_paths();
        assert_eq!(
            paths,
            vec![
                dir.path().to_path_buf(),
                dir.join("a"),
                dir.join("a/b"),
                dir.join("a/b/c"),
            ]
        );

        assert_eq!(r.errs().len(), 1);
        let err = &r.errs()[0];
        assert_eq!(err.path(), Some(dir.join("a/back").as_path()));
        assert_eq!(err.loop_ancestor(), Some(dir.path()));
        assert!(err.duplicate_first_path().is_none());
    }
}

/// Native symlink test: a symlink to a previously visited directory is
/// skipped in global mode.
///
/// This test only runs when the system supports symbolic links. On systems
/// that do not, the fake identity provider fixture in
/// `dir_identity_global_dedup` provides equivalent coverage.
#[test]
fn dir_identity_native_symlink_global_dedup() {
    skip_without_symlinks!();
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "b"); // b is a symlink to a
    dir.touch("a/a1");

    let wd = WalkDir::new(dir.path())
        .follow_links(true)
        .dir_identity_policy(DirIdentityPolicy::Global)
        .sort_by_file_name();
    let r = dir.run_recursive(wd);

    // b is a symlink to a, so they have the same identity. b is skipped.
    let paths = r.sorted_paths();
    assert_eq!(
        paths,
        vec![dir.path().to_path_buf(), dir.join("a"), dir.join("a/a1"),]
    );

    assert_eq!(r.errs().len(), 1);
    let err = &r.errs()[0];
    assert_eq!(err.path(), Some(dir.join("b").as_path()));
    assert_eq!(err.duplicate_first_path(), Some(dir.join("a").as_path()));
    assert!(err.loop_ancestor().is_none());
    assert!(err.io_error().is_none());
}
