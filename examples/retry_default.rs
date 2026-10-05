//! Example: walking a directory with the default error behavior (no retries).
//!
//! This demonstrates that the default behavior is unchanged: the first error
//! encountered is yielded by the iterator and no retries are attempted.
//! Compare with [`retry_walk.rs`], which enables retries.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example retry_default -- <directory>
//! ```
//!
//! [`retry_walk.rs`]: retry_walk.rs

use std::env;
use walkdir::WalkDir;

fn main() {
    let root = env::args().nth(1).unwrap_or_else(|| ".".to_string());

    // Default behavior: no retries. The first error encountered is yielded
    // and iteration continues with the next directory.
    for entry in WalkDir::new(&root) {
        match entry {
            Ok(entry) => println!("{}", entry.path().display()),
            Err(err) => {
                eprintln!("error: {}", err);
            }
        }
    }
}
