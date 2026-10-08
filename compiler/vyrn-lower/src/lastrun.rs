//! The counts of the last `vyrn run --profile`, saved per root so `vyrn why --cost` can show
//! them beside the static facts.
//!
//! A root has one file in [`vyrn_frontend::manifest::profile_dir`], named by the hash of the
//! root's canonical path. Line 1 is the stamp: the hash of the root's source and of every
//! module it imports ([`vyrn_frontend::ast::Program::module_hashes`]), so an edit to any of them
//! makes the file stale. Each further line is one site: `function`, `line`, `verb`, then
//! blocks made, bytes made, blocks freed and bytes live at exit, tab-separated.

use std::collections::BTreeMap;
use std::path::PathBuf;
use vyrn_frontend::ast::Program;
use vyrn_frontend::hash::sha256_hex;

/// What a run made at one site of the root file: blocks and bytes made, blocks freed, bytes
/// still live at exit.
pub struct SiteCount {
    pub function: String,
    pub line: u32,
    pub verb: String,
    pub blocks: u64,
    pub bytes: u64,
    pub freed: u64,
    pub live: u64,
}

/// The blocks and bytes a run made at `(function, line, verb)`.
pub type Sites = BTreeMap<(String, u32, String), [u64; 4]>;

/// What [`load`] found for a root.
pub enum Found {
    /// The root has never run under `--profile`.
    Never,
    /// The source or an import changed since the run.
    Stale,
    Fresh(Sites),
}

/// What a saved profile must match to be fresh: `source` and the modules `program` linked.
pub fn stamp(source: &str, program: &Program) -> String {
    let mut text = source.to_string();
    for (module, hash) in &program.module_hashes {
        text.push_str(&format!("\n{module}={hash}"));
    }
    sha256_hex(text.as_bytes())
}

fn file(root: &str) -> PathBuf {
    let root = std::fs::canonicalize(root).map_or_else(
        |_| root.to_string(),
        |p| p.to_string_lossy().replace('\\', "/"),
    );
    vyrn_frontend::manifest::profile_dir().join(sha256_hex(root.as_bytes()))
}

/// Saves `sites` as the last run of `root`. A failure is ignored: the profile is optional.
pub fn save(root: &str, stamp: &str, sites: &[SiteCount]) {
    let mut text = format!("{stamp}\n");
    for s in sites.iter().filter(|s| s.blocks > 0 || s.freed > 0) {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            s.function, s.line, s.verb, s.blocks, s.bytes, s.freed, s.live
        ));
    }
    let file = file(root);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = vyrn_frontend::manifest::write_whole(&file, text.as_bytes());
}

/// The last run of `root`, if its stamp is `stamp`. Site 0, the blocks made before any site
/// ran, has no function and is not saved.
pub fn load(root: &str, stamp: &str) -> Found {
    let Ok(text) = std::fs::read_to_string(file(root)) else {
        return Found::Never;
    };
    let mut lines = text.lines();
    if lines.next() != Some(stamp) {
        return Found::Stale;
    }
    let mut sites = Sites::new();
    for row in lines {
        let f: Vec<&str> = row.split('\t').collect();
        let [function, line, verb, counts @ ..] = f.as_slice() else {
            continue;
        };
        let n = |i: usize| counts.get(i).and_then(|c| c.parse().ok()).unwrap_or(0);
        let at = sites
            .entry((
                function.to_string(),
                line.parse().unwrap_or(0),
                verb.to_string(),
            ))
            .or_default();
        for (i, v) in at.iter_mut().enumerate() {
            *v += n(i);
        }
    }
    Found::Fresh(sites)
}
