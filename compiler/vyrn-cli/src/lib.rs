//! What the driver offers as a LIBRARY: the host the compiled route runs in.
//!
//! `vyrn-cli` is a binary. One module of it is not driver code at all —
//! [`wasmrun`] is the hand-written `wasi_snapshot_preview1` host that every
//! compiled run, test, bench and served request goes through, and it depends on
//! nothing in `main.rs`. It is a library target here so a second crate can run a
//! program the way the driver runs it, rather than write a second host.
//!
//! The second crate is `vyrn-frontend`, whose loader and JSON-decoder tests ran
//! their linked programs through the tree-walking interpreter. RFC-0125 §3 M5's
//! `library-run` row moves them to the compiled route, and this target is how
//! they reach it — a dev-dependency, so the frontend still ships with no
//! dependencies at all.
//!
//! `main.rs` reads `wasmrun` from here too. One host, one copy, one place.

pub mod wasmrun;
