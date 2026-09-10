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

use crate::diagnostics::{Diagnostic, Severity};
use std::collections::{HashMap, HashSet};

/// What a synthesized module's directives are read under: where they resolve,
/// how far they may reach, and which lines may carry one at all.
///
/// **Generated text is data until the compiler decides otherwise.** A directive
/// used to be any line whose first non-blank characters were `//@origin` or
/// `//@diag`, found by a scan with no lexical context — so a string literal a
/// component author wrote, copied through verbatim by a trusted generator
/// (RFC-0048 §1 is `std/vyx` doing exactly that), could fail the build at a path
/// outside the project with wording the compiler never wrote. The lexer already
/// knows which lines are comments and which are data; this asks it once.
pub struct Context<'a> {
    /// Directory of the module that wrote the generator call — what a directive
    /// path resolves against (RFC-0033: paths are relative to the importer).
    pub importer_dir: &'a str,
    /// The project directory. RFC-0033 maps generated source to USER source, so
    /// a directive that names a file outside the project is not a map — it is
    /// refused as malformed and the diagnostic keeps its generated location.
    /// The loader passes the manifest's directory, or the entry file's own
    /// directory when there is no manifest. Empty only in unit tests; the
    /// directive is then bounded by `importer_dir`'s own root only.
    pub project: &'a str,
    /// The lines on which the lexer says a `//` comment BEGINS — the only lines
    /// a directive may be read from. `None` when the text does not lex: the
    /// line-scan then honours every line, because a module that cannot lex still
    /// owes its errors an origin map (RFC-0053) and fails the build regardless.
    comment_lines: Option<HashSet<usize>>,
}

impl<'a> Context<'a> {
    /// Read `source`'s lexical structure once, for both the origin table and the
    /// `//@diag` scan.
    pub fn new(source: &str, importer_dir: &'a str, project: &'a str) -> Self {
        Context {
            importer_dir,
            project,
            comment_lines: comment_lines(source),
        }
    }

    /// Whether a directive found on 1-based `line` is a control line and not data
    /// that happens to look like one.
    fn honors(&self, line: usize) -> bool {
        match &self.comment_lines {
            Some(lines) => lines.contains(&line),
            None => true,
        }
    }
}

/// The 1-based lines of `source` on which the lexer says a `//` comment BEGINS,
/// or `None` when the text does not lex.
///
/// The one reading every control-line scan over generated text shares. A scan
/// that reads a line's first characters and nothing else cannot tell a directive
/// from data that looks like one, and a generator copies its input through
/// verbatim — so the lexer decides, once, and every consumer asks it. `None`
/// means there is no lexical structure to read, and the caller then honours every
/// line: a module that does not lex fails the build regardless (RFC-0053).
pub fn comment_lines(source: &str) -> Option<HashSet<usize>> {
    Some(
        crate::lexer::lex_with_trivia(source)
            .ok()?
            .iter()
            .filter(|t| t.kind == crate::lexer::TrivKind::Comment)
            .map(|t| t.start_line)
            .collect(),
    )
}

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
    /// `banner` is the module key; `ctx` says where the paths resolve, how far
    /// they may reach, and which lines are comments rather than data.
    pub fn add_module(&mut self, banner: &str, source: &str, ctx: &Context<'_>) {
        let mut dirs: Vec<Directive> = Vec::new();
        let mut total = 0usize;
        for (i, raw) in source.lines().enumerate() {
            total = i + 1;
            let trimmed = raw.trim_start();
            let Some(rest) = trimmed.strip_prefix("//@origin") else {
                continue;
            };
            if !ctx.honors(i + 1) {
                continue;
            }
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
            match parse_origin_body(rest, ctx) {
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
                // EOF when this is the last directive. Back-to-back directives
                // leave this one no governed lines; the span clamps to empty
                // rather than backwards, which is what a consumer of an
                // inclusive range would read as a negative span.
                let end = dirs
                    .get(idx + 1)
                    .map(|n| n.gen_line.saturating_sub(2))
                    .unwrap_or(total)
                    .max(dir.gen_line);
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

/// The `//@diag` directives in one synthesized module's source, as diagnostics
/// at the severity each one names (RFC-0099).
///
/// ```text
/// //@diag warning ./schema/users.tbl:13:1 column `email` has no length limit
/// //@diag error   ./schema/users.tbl:14:1 column `id` is declared twice
/// ```
///
/// This is a generator's whole reporting surface. Its other convention is an
/// identifier line that fails to parse — fatal by construction, and worded by
/// the parser rather than by the generator — so anything a generator actually
/// wants to SAY rides the generated text as a comment, and the loader lifts it
/// here: inert to lexing, hashing, `fmt` and `emit-gen` exactly like `//@origin`.
///
/// Carrying the report in the OUTPUT TEXT rather than through a side channel is
/// the load-bearing choice. Generated output is content-addressed and cached
/// (RFC-0021), and it is the thing the cross-engine gate compares byte for byte
/// (RFC-0076). A diagnostic reported by a call into the interpreter would
/// disappear on a cache hit and would need its own equality gate; a diagnostic
/// that IS output bytes inherits both properties and can lose neither.
///
/// `//@warning <anchor> <message>` is the RFC-0071 M2b spelling of
/// `//@diag warning <anchor> <message>`, kept because it costs one match arm.
///
/// The anchor is an origin position in the SAME notation `//@origin` uses,
/// parsed by the same [`parse_origin_body`], so a report points at the line the
/// user wrote rather than at generated text they never see. A generator with no
/// position to give writes `-`, and the report lands at the generated location
/// instead — the never-lose rule the origin table already follows. An anchor
/// field that is neither `-` nor a parseable position is kept as part of the
/// message, because dropping text a user might have written is worse than a
/// slightly odd first word. An unrecognized SEVERITY word is reported as a
/// warning with the whole line as its message: a future severity must not fail a
/// build on an older compiler, and must not vanish either.
pub const NO_POSITION: &str = "-";

pub fn diagnostics(banner: &str, source: &str, ctx: &Context<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (i, raw) in source.lines().enumerate() {
        let trimmed = raw.trim_start();
        if !ctx.honors(i + 1) {
            continue;
        }
        let (severity, rest, unknown) = if let Some(body) = trimmed.strip_prefix("//@diag") {
            let body = body.trim();
            match body.split_once(char::is_whitespace) {
                Some(("error", tail)) => (Severity::Error, tail.trim(), None),
                Some(("warning", tail)) => (Severity::Warning, tail.trim(), None),
                // Neither severity word: the fields are shifted, so the anchor is
                // not parsed and the whole line is the message.
                Some((word, _)) => (Severity::Warning, body, Some(word.to_string())),
                None if body.is_empty() => continue,
                None => (Severity::Warning, body, Some(body.to_string())),
            }
        } else if let Some(body) = trimmed.strip_prefix("//@warning") {
            (Severity::Warning, body.trim(), None)
        } else {
            continue;
        };
        let (pos, message) = match unknown {
            Some(_) => (None, rest),
            None => match rest.split_once(char::is_whitespace) {
                // `-` is the explicit "no position" marker. It is consumed rather
                // than kept, or every unpositioned report would read `- \`fn old\`
                // is deprecated`.
                Some((NO_POSITION, tail)) => (None, tail.trim()),
                Some((head, tail)) => match parse_origin_body(head, ctx) {
                    Ok(origin) => (Some(origin), tail.trim()),
                    Err(_) => (None, rest),
                },
                None => (None, rest),
            },
        };
        if message.is_empty() {
            continue;
        }
        let stage = match severity {
            Severity::Error => "generated.error",
            Severity::Warning => "generated.warning",
        };
        let mut d = Diagnostic::error(i + 1, 0, stage, message.to_string());
        d.severity = severity;
        match pos {
            Some(origin) => {
                d.file = Some(origin.file);
                d.line = origin.line;
                d.col = origin.col;
                d.from_generated = true;
                d.note = Some(format!(
                    "in generated code {banner}:{} (see `vyrn emit-gen`)",
                    i + 1
                ));
            }
            None => d.file = Some(banner.to_string()),
        }
        if let Some(word) = unknown {
            d.note = Some(format!(
                "unrecognized severity `{word}` in a `//@diag` directive; reported as a warning"
            ));
        }
        out.push(d);
    }
    out
}

/// Parse a `<path>:<line>:<col>` directive body and resolve `path` against
/// `importer_dir`. The two trailing colon-separated fields are the position, so
/// a path may itself contain colons only if they are not the last two — v1 input
/// paths are plain relative specifiers, so this is exact.
fn parse_origin_body(body: &str, ctx: &Context<'_>) -> Result<Origin, String> {
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
        file: resolve_origin_path(ctx, path)?,
        line,
        col,
    })
}

/// Resolve a directive `path` (relative to the generator's importing module)
/// into a normalized slash key, mirroring the loader's relative-import
/// resolution for local paths.
///
/// **An anchor resolves; it does not roam** (RFC-0099's Containment section, in
/// the words it already used). A map from generated text to user source is only
/// a map while it points at source the user wrote, so three claims are refused
/// rather than followed: an absolute path (a directive path is relative to the
/// importer by definition), a path that climbs out of the importer's own root,
/// and a path that lands outside the project directory. The refusal is a
/// malformed directive, which the never-lose rule already handles: the
/// diagnostic stays at its generated location and says why.
fn resolve_origin_path(ctx: &Context<'_>, path: &str) -> Result<String, String> {
    if crate::audience::is_absolute(path) {
        return Err(format!(
            "path `{path}` is absolute; an origin path is relative to the importing module"
        ));
    }
    let joined = if ctx.importer_dir.is_empty() {
        path.to_string()
    } else {
        format!("{}/{path}", ctx.importer_dir)
    };
    // Normalization reports the climb itself: a `..` with nothing left to pop
    // is the event, and it happens identically on every path shape.
    let (resolved, climbed) = normalize_slashes(&joined);
    if climbed {
        return Err(format!("path `{path}` climbs out of the importing module"));
    }
    if !ctx.project.is_empty() {
        let base = OriginMaps::norm_path_key(ctx.project);
        let under = OriginMaps::norm_path_key(&resolved);
        // A relative path against an absolute project base is the same project
        // spelled from inside it, not a file outside it — the allowance
        // `audience::relative_to` makes for module keys, for the same reason.
        let relative_spelling =
            !crate::audience::is_absolute(&resolved) && crate::audience::is_absolute(ctx.project);
        if !relative_spelling && under != base && !under.starts_with(&format!("{base}/")) {
            return Err(format!("path `{path}` names a file outside the project"));
        }
    }
    Ok(resolved)
}

/// Collapse `.`/`..` segments in a slash path (a local copy of the loader's
/// `normalize`, kept private so `origin` has no loader dependency), and report
/// whether a `..` popped when there was nothing left to pop.
///
/// **The root is not a segment.** `/` and `n:/` are the floor, split off before
/// anything is popped, so they survive resolution and cannot be consumed by a
/// `..`. Both halves of that used to be wrong in a platform-shaped way: a Unix
/// absolute path lost its leading `/` to the split and had it glued back on
/// afterwards (the first-Linux-CI-run hang), and a Windows one let `..` eat `n:`
/// as if it were a directory. The caller then had to GUESS the overshoot from
/// the result's shape — losing the drive on Windows, keeping the restored `/` on
/// Unix — so the same directive was refused on one platform and followed on the
/// other. The overshoot is an event during the walk, and is returned as one.
///
/// A backslash is a separator here, as it already is in the loader's
/// `normalize`. It has to be: [`OriginMaps::norm_path_key`] folds backslashes
/// before the project check compares prefixes, so a climb spelled
/// `..\..\..\x.vyx` used to pass BOTH refusals — one opaque segment that pops
/// nothing, then a folded string that still starts with the project base — and
/// name on Windows exactly the file the check exists to keep out.
fn normalize_slashes(p: &str) -> (String, bool) {
    let p = &p.replace('\\', "/");
    let root_len = if p.starts_with('/') {
        1
    } else if crate::audience::is_absolute(p) {
        p.find('/').map(|i| i + 1).unwrap_or(p.len())
    } else {
        0
    };
    let (root, rest) = p.split_at(root_len);
    let mut out: Vec<&str> = Vec::new();
    let mut climbed = false;
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            // Nothing to pop is the climb, whether the floor is `/`, `n:/`, or
            // the relative base's own first segment.
            ".." => climbed |= out.pop().is_none(),
            s => out.push(s),
        }
    }
    (format!("{root}{}", out.join("/")), climbed)
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

    /// The reading context for a generated text with no project directory to be
    /// bounded by — the shape an in-memory load has.
    fn ctx<'a>(source: &str, importer_dir: &'a str) -> Context<'a> {
        Context::new(source, importer_dir, "")
    }

    fn resolved(importer_dir: &str, path: &str) -> Result<String, String> {
        resolve_origin_path(&ctx("", importer_dir), path)
    }

    #[test]
    fn remaps_a_governed_line_to_its_origin() {
        let banner = "generated by components(\"./comp\") at app.vyrn";
        let src = "line1\n//@origin ./comp/Item.vyx:14:9\nrow.push(x)\nmore\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, &ctx(src, ""));
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
            resolved("/tmp/probe/app", "./comp/Widget.vyx").unwrap(),
            "/tmp/probe/app/comp/Widget.vyx"
        );
        // Windows drive-letter paths are unaffected either way.
        assert_eq!(
            resolved("n:/lang/examples", "./routes/index.vyx").unwrap(),
            "n:/lang/examples/routes/index.vyx"
        );
        // A relative importer dir stays relative.
        assert_eq!(resolved("examples", "./a.vyx").unwrap(), "examples/a.vyx");

        let banner = "generated by components(\"./comp\") at /tmp/probe/app.vyrn";
        let src = "//@origin ./comp/Widget.vyx:6:8\n<expr>\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, &ctx(src, "/tmp/probe"));
        assert_eq!(
            maps.input_files(),
            vec!["/tmp/probe/comp/Widget.vyx".to_string()]
        );
        assert_eq!(maps.regions_for("/tmp/probe/comp/Widget.vyx").len(), 1);
    }

    #[test]
    fn end_directive_stops_governing() {
        let banner = "b";
        let src = "//@origin ./a.vyx:1:1\nx\n//@origin end\ny\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, &ctx(src, ""));
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
        maps.add_module(banner, src, &ctx(src, ""));
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
        maps.add_module(banner, src, &ctx(src, "src/ui"));
        let mut d = diag(banner, 2, 1);
        maps.remap(&mut d);
        assert_eq!(d.file.as_deref(), Some("src/ui/ItemRow.vyx"));
    }

    #[test]
    fn inverts_to_regions_for_forward_mapping() {
        let banner = "b";
        let src = "a\n//@origin ./x.vyx:5:2\nb\nc\n//@origin end\nd\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, &ctx(src, ""));
        let regions = maps.regions_for("x.vyx");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].gen_start_line, 3);
        assert_eq!(regions[0].gen_end_line, 4); // up to the line before `end`
        assert_eq!(regions[0].origin.line, 5);
    }

    /// Back-to-back directives: the first region has no governed lines, so its
    /// inclusive span clamps to empty (`end == start`) — never backwards, which
    /// a forward-mapping consumer would read as a negative range.
    #[test]
    fn adjacent_directives_yield_a_well_formed_empty_region() {
        let banner = "b";
        let src = "\n//@origin ./a.vyx:1:1\n//@origin ./b.vyx:2:2\nx\n";
        let mut maps = OriginMaps::new();
        maps.add_module(banner, src, &ctx(src, ""));
        let mut regions = maps.regions_for("a.vyx");
        regions.extend(maps.regions_for("b.vyx"));
        assert_eq!(regions.len(), 2);
        assert!(
            regions.iter().all(|r| r.gen_end_line >= r.gen_start_line),
            "backwards region: {regions:?}"
        );
        // The first directive (line 2) governs nothing: the next sits on line 3.
        let first = regions
            .iter()
            .find(|r| r.origin.file.ends_with("a.vyx"))
            .unwrap();
        assert_eq!(first.gen_start_line, 3);
        assert_eq!(first.gen_end_line, 3);
    }

    // ---- `//@diag` / `//@warning` -> diagnostics (RFC-0071 M2b, RFC-0099) --

    #[test]
    fn a_positioned_warning_points_at_the_input_file() {
        let src = "//@warning ./routes/p/[id].vyx:13:1 `fn old` is deprecated
fn x() {}
";
        let ds = diagnostics("b", src, &ctx(src, "app"));
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].severity, crate::diagnostics::Severity::Warning);
        assert_eq!(ds[0].file.as_deref(), Some("app/routes/p/[id].vyx"));
        assert_eq!(ds[0].line, 13);
        assert_eq!(ds[0].col, 1);
        assert_eq!(ds[0].message, "`fn old` is deprecated");
        // The generated location is not lost, only demoted to a note.
        assert!(ds[0]
            .note
            .as_deref()
            .unwrap()
            .contains("generated code b:1"));
        assert!(ds[0].from_generated);
    }

    #[test]
    fn the_no_position_marker_is_consumed_not_spoken() {
        // A generator with no source position to give writes `-`. It must not
        // reach the user's screen as the first word of the message.
        let src = "//@warning - `fn old` is deprecated
";
        let ds = diagnostics("b", src, &ctx(src, "app"));
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].message, "`fn old` is deprecated");
        assert_eq!(ds[0].file.as_deref(), Some("b")); // reported where it was generated
        assert!(!ds[0].from_generated);
    }

    #[test]
    fn a_malformed_position_keeps_the_whole_line_as_the_message() {
        // Never lose the notice: an unparseable first field is text, not a bug.
        let src = "//@warning whoops something is off
";
        let ds = diagnostics("b", src, &ctx(src, ""));
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].message, "whoops something is off");
        assert_eq!(ds[0].file.as_deref(), Some("b"));
    }

    #[test]
    fn an_empty_directive_says_nothing() {
        let src = "//@warning\n//@warning\n//@diag\n//@diag\n";
        assert!(diagnostics("b", src, &ctx(src, "")).is_empty());
    }

    #[test]
    fn the_severity_is_the_generators_to_choose() {
        let src = "//@diag error ./schema/users.tbl:14:3 column `id` is declared twice
//@diag warning ./schema/users.tbl:9:3 column `email` has no length limit
";
        let ds = diagnostics("b", src, &ctx(src, "app"));
        assert_eq!(ds.len(), 2);
        assert_eq!(ds[0].severity, Severity::Error);
        assert_eq!(ds[0].file.as_deref(), Some("app/schema/users.tbl"));
        assert_eq!((ds[0].line, ds[0].col), (14, 3));
        assert_eq!(ds[0].message, "column `id` is declared twice");
        assert!(ds[0].from_generated);
        assert_eq!(ds[1].severity, Severity::Warning);
        assert_eq!((ds[1].line, ds[1].col), (9, 3));
    }

    #[test]
    fn the_warning_directive_is_the_warning_severity() {
        // The RFC-0071 M2b spelling and the RFC-0099 one must be one mechanism,
        // or the older emitters quietly mean something else.
        let a = diagnostics(
            "b",
            "//@warning ./x.vyx:1:1 hi\n",
            &ctx("//@warning ./x.vyx:1:1 hi\n", "app"),
        );
        let d = diagnostics(
            "b",
            "//@diag warning ./x.vyx:1:1 hi\n",
            &ctx("//@diag warning ./x.vyx:1:1 hi\n", "app"),
        );
        assert_eq!(a.len(), 1);
        assert_eq!(d.len(), 1);
        assert_eq!(a[0].severity, d[0].severity);
        assert_eq!(a[0].file, d[0].file);
        assert_eq!(a[0].message, d[0].message);
    }

    #[test]
    fn an_unrecognized_severity_neither_escalates_nor_vanishes() {
        // A generator written for a later compiler must not fail a build here,
        // and must not be silently dropped either.
        let src = "//@diag hint ./x.vyx:1:1 consider a shorter name\n";
        let ds = diagnostics("b", src, &ctx(src, ""));
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].severity, Severity::Warning);
        assert_eq!(ds[0].message, "hint ./x.vyx:1:1 consider a shorter name");
        assert!(ds[0].note.as_deref().unwrap().contains("hint"));
    }

    // ---- generated text is data until the compiler decides otherwise --------

    /// A generator copies its input through verbatim (`std/vyx` does, RFC-0048
    /// §1), so a multi-line string literal in a component file can put any text
    /// at the start of a generated line. That text is DATA: it may not fail a
    /// build, choose a severity, or name a file.
    #[test]
    fn a_directive_inside_a_string_literal_is_data_not_a_control_line() {
        let src = "fn banner() -> String {\n    return \"first\n\
                   //@diag error ./elsewhere.vyx:1:1 injected by a string literal\n\
                   last\"\n}\n";
        assert!(diagnostics("b", src, &ctx(src, "app")).is_empty());

        // The same line, moved out of the literal, IS a control line — so the
        // test cannot pass by the scan having stopped working.
        let real = format!("//@diag error ./elsewhere.vyx:1:1 reported by the generator\n{src}");
        let ds = diagnostics("b", &real, &ctx(&real, "app"));
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].severity, Severity::Error);
    }

    /// The same root cause on the origin side: a directive inside string data
    /// used to displace the valid directive above it, attributing a real error
    /// to a file the generator never read.
    #[test]
    fn an_origin_inside_a_string_literal_does_not_hijack_the_map() {
        let src = "//@origin ./real.vyx:3:1\n\
                   fn s() -> String {\n    return \"a\n\
                   //@origin ./hijacked.vyx:7:7\n\
                   b\"\n}\nlet x = nope()\n";
        let mut maps = OriginMaps::new();
        maps.add_module("b", src, &ctx(src, "app"));
        let mut d = diag("b", 7, 1);
        assert!(maps.remap(&mut d));
        assert_eq!(d.file.as_deref(), Some("app/real.vyx"));
        assert_eq!(d.line, 3);
        // The hijacker contributed no region at all.
        assert!(maps.regions_for("app/hijacked.vyx").is_empty());
    }

    /// Text that does not lex has no lexical context to read, and RFC-0053 says
    /// its errors still owe the input file a map — so the scan keeps every line.
    #[test]
    fn text_that_does_not_lex_keeps_its_directives() {
        let src = "//@origin ./x.vyx:2:2\n<not vyrn at all$$$\n";
        assert!(crate::lexer::lex_with_trivia(src).is_err());
        let mut maps = OriginMaps::new();
        maps.add_module("b", src, &ctx(src, ""));
        let mut d = diag("b", 2, 1);
        assert!(maps.remap(&mut d));
        assert_eq!(d.file.as_deref(), Some("x.vyx"));
    }

    /// An anchor resolves; it does not roam (RFC-0099, Containment). Three
    /// claims a map may not make, each degrading to the never-lose rule.
    #[test]
    fn an_origin_may_not_name_a_file_outside_the_project() {
        // Absolute: a directive path is relative to the importing module.
        assert!(resolved("n:/proj/app", "/etc/passwd").is_err());
        assert!(resolved("n:/proj/app", "c:/Windows/win.ini").is_err());
        // Climbing out of the importer's own root — the shape that used to pop
        // a whole absolute prefix and come back as a relative key.
        assert!(resolved("n:/proj/app", "../../../../../../../../x.vyx").is_err());
        assert!(resolved("", "../x.vyx").is_err());
        // Inside the project directory, in a sibling of the importer: legal, and
        // the shape every `.vyx` page mount actually has.
        assert_eq!(
            resolved("n:/proj/client", "../app/routes/index.vyx").unwrap(),
            "n:/proj/app/routes/index.vyx"
        );

        // With a project directory declared, a file elsewhere on the same drive
        // is refused too, and the diagnostic keeps its generated location.
        let c = Context::new("", "n:/proj/app", "n:/proj");
        assert!(resolve_origin_path(&c, "../../other/x.vyx").is_err());
        assert_eq!(
            resolve_origin_path(&c, "../shared/x.vyx").unwrap(),
            "n:/proj/shared/x.vyx"
        );

        let src = "//@origin ../../../../../../../../outside.vyx:1:1\nlet x = nope()\n";
        let mut maps = OriginMaps::new();
        maps.add_module("b", src, &ctx(src, "n:/proj/app"));
        let mut d = diag("b", 2, 1);
        assert!(!maps.remap(&mut d));
        assert_eq!(d.file.as_deref(), Some("b"));
        assert!(d.note.as_deref().unwrap().contains("malformed"));
    }

    /// The same directive, against the two shapes an absolute base has, must get
    /// the same verdict — the rule is about what the path CLAIMS, and a claim
    /// does not change with the machine that reads it.
    ///
    /// It used to. `climbed` was inferred from the result no longer being
    /// absolute, which is a Windows-only trace: popping past `n:` drops the
    /// drive, while popping past `/` leaves a leading `/` restored on the way
    /// out. So the refusal fired on Windows and not on Unix, and a generator
    /// could name `/outside.vyx` on the platforms CI runs (the red main of
    /// 2026-08-14). The overshoot is now recorded where it happens.
    #[test]
    fn the_climb_out_verdict_does_not_depend_on_the_platforms_path_shape() {
        let escape = "../../../../../../../../outside.vyx";
        for base in ["n:/proj/app", "/proj/app", "/tmp/probe/escape"] {
            assert!(
                resolved(base, escape).is_err(),
                "climbing out of `{base}` must be refused"
            );
        }
        // And a `..` that stays inside is still resolved, on both shapes.
        assert_eq!(
            resolved("n:/proj/client", "../app/x.vyx").unwrap(),
            "n:/proj/app/x.vyx"
        );
        assert_eq!(
            resolved("/proj/client", "../app/x.vyx").unwrap(),
            "/proj/app/x.vyx"
        );
        // Exactly at the root is not out of it: the last `..` that has something
        // to pop is legal, the next one is not.
        assert_eq!(resolved("/proj", "../x.vyx").unwrap(), "/x.vyx");
        assert!(resolved("/proj", "../../x.vyx").is_err());
        assert_eq!(resolved("n:/proj", "../x.vyx").unwrap(), "n:/x.vyx");
        assert!(resolved("n:/proj", "../../x.vyx").is_err());
    }

    /// A separator the refusal does not read is a separator the escape can use.
    /// `norm_path_key` folds backslashes before the project check compares
    /// prefixes, so a backslash climb used to survive both refusals: an opaque
    /// segment pops nothing, and the folded key still starts with the project
    /// base. On Windows that named the file outside the project.
    #[test]
    fn a_climb_spelled_with_backslashes_is_the_same_climb() {
        assert!(resolved("n:/proj/app", r"..\..\..\..\outside.vyx").is_err());
        let c = Context::new("", "n:/proj/app", "n:/proj");
        assert!(resolve_origin_path(&c, r"..\..\outside.vyx").is_err());
        // A backslash-spelled path that stays inside resolves, in slash form.
        assert_eq!(
            resolve_origin_path(&c, r"..\shared\x.vyx").unwrap(),
            "n:/proj/shared/x.vyx"
        );
    }
}
