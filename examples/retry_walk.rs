//! Example: walking a directory with the retry policy enabled.
//!
//! This demonstrates how to use [`WalkDir::retry_on_error`] and how errors
//! are reported when the retry budget is exhausted.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example retry_walk -- <directory>
//! ```
//!
//! [`WalkDir::retry_on_error`]: https://docs.rs/walkdir/latest/walkdir/struct.WalkDir.html#method.retry_on_error

use std::env;
use walkdir::{ErrorStage, WalkDir};

fn main() {
    let root = env::args().nth(1).unwrap_or_else(|| ".".to_string());

    // Walk the directory with retries enabled. Up to 3 retries are attempted
    // for failed directory operations before an error is yielded.
    for entry in WalkDir::new(&root).retry_on_error(3) {
        match entry {
            Ok(entry) => println!("{}", entry.path().display()),
            Err(err) => {
                // The error retains the path and stage of the failure.
                let path = err
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<none>".to_string());
                let stage = match err.stage() {
                    Some(ErrorStage::Open) => "open",
                    Some(ErrorStage::Read) => "read",
                    None => "n/a",
                };
                eprintln!(
                    "error at {} (stage={}, retryable={}): {}",
                    path,
                    stage,
                    err.is_retryable(),
                    err
                );
            }
        }
    }
}
