//! RFC-0033 — origin maps: format-agnostic editor support inside generator
//! inputs.
//!
//! A generator (RFC-0021) may interleave **origin directives** in the source
//! text it returns:
//!
//! ```text
//! //@origin ./components/ItemRow.vyx:14:21
//! …generated lines derived from that input position…
//! //@origin end
//! ```
//!
//! A directive governs the generated lines that FOLLOW it, until the next
//! directive or `//@origin end`. `path` is relative to the generator's importing
//! module (the same base its inputs resolve against); `line:col` are 1-based
//! positions in that input file. The directive is an ordinary `//` comment, so
//! parsing, hashing (gen cache keys), `fmt`, and `emit-gen` treat it as inert
//! source text — a generator that emits none behaves exactly as before.
//!
//! The toolchain does two things with the table, both single-sourced here:
//!
//! 1. **Diagnostic remapping (CLI + LSP).** [`OriginMaps::remap`] moves a
//!    diagnostic that landed in a synthesized module at a governed line to its
//!    origin `file:line:col`, preserving the generated location as a secondary
//!    note. A malformed directive never LOSES the diagnostic: it stays at the
//!    generated location with the malformed directive noted.
//! 2. **Forward mapping (LSP).** [`OriginMaps::regions_for`] inverts the table so
//!    the editor can map a position inside an input file to the governed
//!    generated span and answer hover / completion / go-to-definition against
//!    the synthesized module's existing analysis.

use crate::diagnostics::Diagnostic;
use std::collections::HashMap;

/// A resolved origin position an origin directive points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The input file the generated text was derived from — a module-resolver
    /// key (slash path), resolved relative to the generator's importing module.
    pub file: String,
    /// 1-based line in the input file.
    pub line: usize,
    /// 1-based start column in the input file (the verbatim region's start).
    pub col: usize,
}

/// One parsed `//@origin` directive from a synthesized module.
#[derive(Debug, Clone)]
struct Directive {
    /// The first generated line (1-based) this directive governs — the line
    /// immediately after the directive's own line.
    gen_line: usize,
    /// `Some(origin)` for a resolvable directive; `None` for `//@origin end`
    /// (which ends the previous region without starting a new one).
    origin: Option<Origin>,
    /// `Some(reason)` when the directive text was malformed. The region is still
    /// recorded (so the diagnostic is never dropped) but is NOT remapped — the
    /// diagnostic surfaces at the generated location with this reason noted.
    malformed: Option<String>,
}

/// A verbatim/derived region in an input file mapped to a generated span — the
/// inverse of the directive table, for the LSP's forward requests.
#[derive(Debug, Clone)]
pub struct Region {
    /// The synthesized module (banner key) the generated span lives in.
    pub gen_module: String,
    /// The origin position in the input file (region start).
    pub origin: Origin,
    /// First generated line (1-based, inclusive) governed by this region.
    pub gen_start_line: usize,
    /// Last generated line (1-based, inclusive) governed by this region.
    pub gen_end_line: usize,
}

/// The per-module origin directive tables built during a load (RFC-0033).
#[derive(Debug, Clone, Default)]
pub struct OriginMaps {
    /// banner key → its directives, sorted ascending by `gen_line`.
    modules: HashMap<String, Vec<Directive>>,
    /// How many generated lines each module has (for a region's end line).
    module_lines: HashMap<String, usize>,
}

impl OriginMaps {
    pub fn new() -> Self {
        OriginMaps::default()
    }

    /// Parse the `//@origin` directives out of one synthesized module's source.
    /// `banner` is the module key; `importer_dir` is the slash directory the
    /// origin paths resolve against (the loader computes it from the banner).
    pub fn add_module(&mut self, banner: &str, source: &str, importer_dir: &str) {
        let mut dirs: Vec<Directive> = Vec::new();
        let mut total = 0usize;
        for (i, raw) in source.lines().enumerate() {
            total = i + 1;
            let trimmed = raw.trim_start();
            let Some(rest) = trimmed.strip_prefix("//@origin") else {
                continue;
            };
            let rest = rest.trim();
            let gen_line = i + 2; // the directive governs the NEXT line onward
            if rest == "end" {
                dirs.push(Directive {
                    gen_line,
                    origin: None,
                    malformed: None,
                });
                continue;
            }
            match parse_origin_body(rest, importer_dir) {
                Ok(origin) => dirs.push(Directive {
                    gen_line,
                    origin: Some(origin),
                    malformed: None,
                }),
                Err(reason) => dirs.push(Directive {
                    gen_line,
                    origin: None,
                    malformed: Some(reason),
                }),
            }
        }
        if !dirs.is_empty() {
            dirs.sort_by_key(|d| d.gen_line);
            self.modules.insert(banner.to_string(), dirs);
            self.module_lines.insert(banner.to_string(), total);
        }
    }

    /// Whether any module carries directives (a fast no-op guard).
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    /// Remap `d` in place to its origin input file when it landed at a governed
    /// generated line. Returns `true` when it was relocated to a real input file
    /// (the CLI prints it there; the LSP publishes it against that file's URI).
    ///
    /// Never LOSES a diagnostic: a `//@origin end` region or a malformed
    /// directive keeps the generated location (the latter adds a note explaining
    /// why it could not be followed).
    pub fn remap(&self, d: &mut Diagnostic) -> bool {
        let Some(file) = d.file.clone() else {
            return false;
        };
        let Some(dirs) = self.modules.get(&file) else {
            return false;
        };
        // The governing directive is the last one whose region starts at or
        // before the diagnostic's generated line.
        let Some(gov) = dirs.iter().rev().find(|dir| dir.gen_line <= d.line) else {
            return false;
        };
        if let Some(reason) = &gov.malformed {
            d.note = Some(format!(
                "malformed `//@origin` directive ({reason}); reported at generated location"
            ));
            return false;
        }
        let Some(origin) = &gov.origin else {
            // A `//@origin end` region: not governed, keep the generated location.
            return false;
        };
        d.note = Some(format!(
            "in generated code {file}:{}:{} (see `vyrn emit-gen`)",
            d.line,
            d.col.max(1)
        ));
        d.file = Some(origin.file.clone());
        d.line = origin.line;
        d.col = origin.col;
        d.end_col = 0;
        d.from_generated = true;
        true
    }

    /// The input-file regions whose origin file matches `input_file` — the
    /// forward-mapping index the LSP queries for hover/completion/go-to-def.
    /// Regions are returned with their governed generated line span.
    pub fn regions_for(&self, input_file: &str) -> Vec<Region> {
        let want = Self::norm_path_key(input_file);
        let mut out = Vec::new();
        for (banner, dirs) in &self.modules {
            let total = self.module_lines.get(banner).copied().unwrap_or(0);
            for (idx, dir) in dirs.iter().enumerate() {
                let Some(origin) = &dir.origin else { continue };
                if Self::norm_path_key(&origin.file) != want {
                    continue;
                }
                // The region ends on the line before the next directive's own
                // comment line (a directive at source line M has `gen_line == M + 1`,
                // so the preceding content ends at `M - 1 == gen_line - 2`), or at
                // EOF when this is the last directive.
                let end = dirs
                    .get(idx + 1)
                    .map(|n| n.gen_line.saturating_sub(2))
                    .unwrap_or(total);
                out.push(Region {
                    gen_module: banner.clone(),
                    origin: origin.clone(),
                    gen_start_line: dir.gen_line,
                    gen_end_line: end,
                });
            }
        }
        out
    }

    /// Canonical comparison key for a filesystem path.
    ///
    /// Origin directives carry paths as the loader resolved them (`N:/lang/…`),
    /// while an editor supplies them from a URI — and VS Code sends a Windows
    /// drive letter percent-encoded AND lower-cased (`file:///n%3A/…` →
    /// `n:/lang/…`). Windows paths are case-insensitive, but string equality is
    /// not, so every path comparison here MUST go through this or a `.vyx`
    /// silently resolves to nothing (the RFC-0047..0050 editor bugs).
    pub fn norm_path_key(p: &str) -> String {
        let s = p.replace('\\', "/");
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    }

    /// Every input file referenced by some directive (for the LSP's registry of
    /// which open buffers a synthesized module covers).
    pub fn input_files(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for dirs in self.modules.values() {
            for d in dirs {
                if let Some(o) = &d.origin {
                    if !out.contains(&o.file) {
                        out.push(o.file.clone());
                    }
                }
            }
        }
        out
    }
}

/// Parse a `<path>:<line>:<col>` directive body and resolve `path` against
/// `importer_dir`. The two trailing colon-separated fields are the position, so
/// a path may itself contain colons only if they are not the last two — v1 input
/// paths are plain relative specifiers, so this is exact.
fn parse_origin_body(body: &str, importer_dir: &str) -> Result<Origin, String> {
    // Split from the right so the path keeps any interior separators.
    let (rest, col) = body
        .rsplit_once(':')
        .ok_or_else(|| "missing `:col`".to_string())?;
    let (path, line) = rest
        .rsplit_once(':')
        .ok_or_else(|| "missing `:line`".to_string())?;
    let line: usize = line.parse().map_err(|_| format!("bad line `{line}`"))?;
    let col: usize = col.parse().map_err(|_| format!("bad column `{col}`"))?;
    if line == 0 || col == 0 {
        return Err("positions are 1-based".to_string());
    }
    if path.is_empty() {
        return Err("empty path".to_string());
    }
    Ok(Origin {
        file: resolve_origin_path(importer_dir, path),
        line,
        col,
    })
}

/// Resolve a directive `path` (relative to the generator's importing module)
/// into a normalized slash key, mirroring the loader's relative-import
/// resolution for local paths.
fn resolve_origin_path(importer_dir: &str, path: &str) -> String {
    let joined = if importer_dir.is_empty() {
        path.to_string()
    } else {
        format!("{importer_dir}/{path}")
    };
    normalize_slashes(&joined)
}

/// Collapse `.`/`..` segments in a slash path (a local copy of the loader's
/// `normalize`, kept private so `origin` has no loader dependency).
///
/// MUST preserve a leading `/`: splitting drops the empty first segment of a
/// Unix absolute path, and without restoring it every origin file key loses
/// its root — no key ever matches an LSP URI path again and the entire `.vyx`
/// editor surface goes dark on Linux/macOS (the first-Linux-CI-run hang).
/// Windows never sees this because `n:` is a real first segment.
fn normalize_slashes(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(&s) if s != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if p.starts_with('/') && !joined.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Diagnostic;

    fn diag(file: &str, line: usize, col: usize) -> Diagnostic {
        let mut d = Diagnostic::error(line, col, "check", "boom".to_string());
        d.file = Some(file.to_string());
        d
    }

    #[test]
    fn remaps_a_governed_line_to_its_origin() {
        let banner = "generated by components(\"./comp\") at app.vyrn";
        let src = "line1\n//@origin ./comp/Item.vyx:14:9\nrow.push(x)\nmore\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, "");
        // The generated error is on line 3 (`row.push(x)`), governed by the
        // directive on line 2.
        let mut d = diag(banner, 3, 5);
        assert!(maps.remap(&mut d));
        assert_eq!(d.file.as_deref(), Some("comp/Item.vyx"));
        assert_eq!(d.line, 14);
        assert_eq!(d.col, 9);
        assert!(d.note.as_deref().unwrap().contains("generated code"));
    }

    /// A Unix-absolute importer dir must keep its leading `/` through
    /// resolution — losing it (the first-Linux-CI-run bug) makes every origin
    /// key rootless, so `regions_for`/`input_files` never match an LSP URI
    /// path and the `.vyx` editor surface silently dies on Linux/macOS.
    #[test]
    fn unix_absolute_importer_dir_keeps_its_root() {
        assert_eq!(
            resolve_origin_path("/tmp/probe/app", "./comp/Widget.vyx"),
            "/tmp/probe/app/comp/Widget.vyx"
        );
        // Windows drive-letter paths are unaffected either way.
        assert_eq!(
            resolve_origin_path("n:/lang/examples", "./routes/index.vyx"),
            "n:/lang/examples/routes/index.vyx"
        );
        // A relative importer dir stays relative.
        assert_eq!(resolve_origin_path("examples", "./a.vyx"), "examples/a.vyx");

        let banner = "generated by components(\"./comp\") at /tmp/probe/app.vyrn";
        let src = "//@origin ./comp/Widget.vyx:6:8\n<expr>\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, "/tmp/probe");
        assert_eq!(maps.input_files(), vec!["/tmp/probe/comp/Widget.vyx".to_string()]);
        assert_eq!(maps.regions_for("/tmp/probe/comp/Widget.vyx").len(), 1);
    }

    #[test]
    fn end_directive_stops_governing() {
        let banner = "b";
        let src = "//@origin ./a.vyx:1:1\nx\n//@origin end\ny\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, "");
        let mut governed = diag(banner, 2, 1);
        assert!(maps.remap(&mut governed));
        // Line 4 is after `//@origin end` → not remapped.
        let mut ungoverned = diag(banner, 4, 1);
        assert!(!maps.remap(&mut ungoverned));
        assert_eq!(ungoverned.file.as_deref(), Some("b"));
    }

    #[test]
    fn malformed_directive_never_loses_the_diagnostic() {
        let banner = "b";
        let src = "//@origin not-a-valid-directive\nx\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, "");
        let mut d = diag(banner, 2, 1);
        assert!(!maps.remap(&mut d));
        assert_eq!(d.file.as_deref(), Some("b")); // stays at generated location
        assert!(d.note.as_deref().unwrap().contains("malformed"));
    }

    #[test]
    fn resolves_paths_against_the_importer_dir() {
        let banner = "b";
        let src = "//@origin ./ItemRow.vyx:2:3\nx\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, "src/ui");
        let mut d = diag(banner, 2, 1);
        maps.remap(&mut d);
        assert_eq!(d.file.as_deref(), Some("src/ui/ItemRow.vyx"));
    }

    #[test]
    fn inverts_to_regions_for_forward_mapping() {
        let banner = "b";
        let src = "a\n//@origin ./x.vyx:5:2\nb\nc\n//@origin end\nd\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, "");
        let regions = maps.regions_for("x.vyx");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].gen_start_line, 3);
        assert_eq!(regions[0].gen_end_line, 4); // up to the line before `end`
        assert_eq!(regions[0].origin.line, 5);
    }
}
