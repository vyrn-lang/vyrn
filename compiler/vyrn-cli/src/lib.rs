//! The `wasi_snapshot_preview1` host as a library target, so `main.rs` and a
//! second crate (`vyrn-frontend`'s tests, as a dev-dependency) run compiled
//! programs through one host.

pub mod wasmrun;
