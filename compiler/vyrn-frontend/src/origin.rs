//! Origin maps. A generator's output may carry
//! `//@origin <path>:<line>:<col>` comments; each governs the generated lines
//! after it, until the next one or `//@origin end`. `path` is relative to the
//! importing module and positions are 1-based. [`OriginMaps::remap`] moves a
//! diagnostic to its origin, and [`OriginMaps::regions_for`] inverts the table
//! for the LSP. A malformed directive never loses a diagnostic: it stays at the
//! generated location with a note.

use crate::diagnostics::{Diagnostic, Severity};
use std::collections::{HashMap, HashSet};

/// What a synthesized module's directives are read under: where they resolve,
/// how far they may reach, and which lines may carry one.
///
/// A generator copies input through verbatim (`std/vyx` does), so a directive
/// counts only on a line where the lexer starts a comment; a string literal is
/// data.
pub struct Context<'a> {
    /// Directory of the module that wrote the generator call; directive paths
    /// resolve against it.
    pub importer_dir: &'a str,
    /// The manifest's directory, or the entry file's when there is none. A
    /// directive naming a file outside it is malformed. Empty only in unit
    /// tests, where only `importer_dir`'s root bounds a path.
    pub project: &'a str,
    /// The 1-based lines where a `//` comment begins. `None` when the text does
    /// not lex: every line is then honoured, because such a module still owes
    /// its errors an origin map.
    comment_lines: Option<HashSet<usize>>,
}

impl<'a> Context<'a> {
    pub fn new(source: &str, importer_dir: &'a str, project: &'a str) -> Self {
        Context {
            importer_dir,
            project,
            comment_lines: comment_lines(source),
        }
    }

    fn honors(&self, line: usize) -> bool {
        match &self.comment_lines {
            Some(lines) => lines.contains(&line),
            None => true,
        }
    }
}

/// Returns the 1-based lines of `source` where a `//` comment begins, or `None`
/// when the text does not lex. Every control-line scan over generated text uses
/// it, so a directive inside a string literal stays data.
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

/// The input position an origin directive points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// A module-resolver key (slash path).
    pub file: String,
    /// 1-based.
    pub line: usize,
    /// 1-based.
    pub col: usize,
}

#[derive(Debug, Clone)]
struct Directive {
    /// The first governed generated line (1-based): the line after the directive.
    gen_line: usize,
    /// `None` for `//@origin end` or a malformed directive.
    origin: Option<Origin>,
    /// Why the directive is malformed. Its region is recorded but not remapped.
    malformed: Option<String>,
}

/// An input-file position mapped to the generated span it governs, for the
/// LSP's forward requests.
#[derive(Debug, Clone)]
pub struct Region {
    /// The synthesized module's banner key.
    pub gen_module: String,
    pub origin: Origin,
    /// 1-based, inclusive.
    pub gen_start_line: usize,
    /// 1-based, inclusive.
    pub gen_end_line: usize,
}

#[derive(Debug, Clone, Default)]
pub struct OriginMaps {
    /// Banner key to its directives, sorted by `gen_line`.
    modules: HashMap<String, Vec<Directive>>,
    /// Generated line count per module, for the last region's end.
    module_lines: HashMap<String, usize>,
}

impl OriginMaps {
    pub fn new() -> Self {
        OriginMaps::default()
    }

    /// Parses the `//@origin` directives of the synthesized module `banner`.
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
            let gen_line = i + 2;
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

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    /// Moves `d` to its origin when it sits on a governed generated line, and
    /// returns whether it moved. Under `//@origin end` or a malformed directive
    /// `d` keeps its generated location; the malformed case adds a note.
    pub fn remap(&self, d: &mut Diagnostic) -> bool {
        let Some(file) = d.file.clone() else {
            return false;
        };
        let Some(dirs) = self.modules.get(&file) else {
            return false;
        };
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
            return false;
        };
        d.note = Some(format!(
            "in generated code {}:{}:{} (see `vyrn emit-gen`)",
            crate::loader::readable(&file),
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

    /// Returns the regions whose origin file is `input_file`.
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
                // The region ends before the next directive's own line
                // (`gen_line - 2`), or at EOF. Back-to-back directives clamp to
                // `end == start`, never a backwards span.
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

    /// Returns the comparison key for a path: slashes, and lower case on
    /// Windows. Compare every path through it, because VS Code sends a
    /// lower-cased drive letter (`n:/lang/...`) and the loader does not.
    pub fn norm_path_key(p: &str) -> String {
        let s = p.replace('\\', "/");
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    }

    /// Returns every input file some directive names, in first-seen order.
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

/// The anchor a `//@diag` line writes when it has no position to give.
pub const NO_POSITION: &str = "-";

/// Returns the `//@diag <error|warning> <anchor> <message>` lines of a
/// synthesized module as diagnostics; `//@warning` is
/// `//@diag warning`. The report rides the output text, so a gen-cache hit
/// keeps it. The anchor uses `//@origin` notation; `-` or an unparseable anchor
/// reports at the generated location, the latter keeping it in the message. An
/// unknown severity becomes a warning holding the whole line, so a newer
/// generator neither fails nor vanishes.
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
                // Unknown severity: the anchor is not parsed.
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
                    "in generated code {}:{} (see `vyrn emit-gen`)",
                    crate::loader::readable(banner),
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

/// Parses `<path>:<line>:<col>`, splitting from the right so the path keeps
/// any interior colon, and resolves the path.
fn parse_origin_body(body: &str, ctx: &Context<'_>) -> Result<Origin, String> {
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

/// Resolves a directive path against the importer into a normalized slash key.
/// An absolute path, a climb out of the importer's root, and a path outside the
/// project are refused as malformed.
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
    let (resolved, climbed) = normalize_slashes(&joined);
    if climbed {
        return Err(format!("path `{path}` climbs out of the importing module"));
    }
    if !ctx.project.is_empty() {
        let base = OriginMaps::norm_path_key(ctx.project);
        let under = OriginMaps::norm_path_key(&resolved);
        // A relative path against an absolute project is inside it, as in
        // `audience::relative_to`.
        let relative_spelling =
            !crate::audience::is_absolute(&resolved) && crate::audience::is_absolute(ctx.project);
        if !relative_spelling && under != base && !under.starts_with(&format!("{base}/")) {
            return Err(format!("path `{path}` names a file outside the project"));
        }
    }
    Ok(resolved)
}

/// Collapses `.` and `..` segments and returns whether a `..` found nothing to
/// pop. A copy of the loader's `normalize`, so `origin` does not depend on it.
///
/// The root (`/` or `n:/`) is split off first, so no `..` can consume it and
/// the climb verdict is the same on every platform. A backslash is a separator:
/// [`OriginMaps::norm_path_key`] folds it before the project check, so a
/// backslash climb must count here too.
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

    /// A context with no project bound, as an in-memory load has.
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
        let mut d = diag(banner, 3, 5);
        assert!(maps.remap(&mut d));
        assert_eq!(d.file.as_deref(), Some("comp/Item.vyx"));
        assert_eq!(d.line, 14);
        assert_eq!(d.col, 9);
        assert!(d.note.as_deref().unwrap().contains("generated code"));
    }

    /// A rootless key would never match an LSP URI path on Unix.
    #[test]
    fn unix_absolute_importer_dir_keeps_its_root() {
        assert_eq!(
            resolved("/tmp/probe/app", "./comp/Widget.vyx").unwrap(),
            "/tmp/probe/app/comp/Widget.vyx"
        );
        assert_eq!(
            resolved("n:/lang/examples", "./routes/index.vyx").unwrap(),
            "n:/lang/examples/routes/index.vyx"
        );
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
        assert_eq!(d.file.as_deref(), Some("b"));
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
        assert_eq!(regions[0].gen_end_line, 4);
        assert_eq!(regions[0].origin.line, 5);
    }

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
        let first = regions
            .iter()
            .find(|r| r.origin.file.ends_with("a.vyx"))
            .unwrap();
        assert_eq!(first.gen_start_line, 3);
        assert_eq!(first.gen_end_line, 3);
    }

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
        assert!(ds[0]
            .note
            .as_deref()
            .unwrap()
            .contains("generated code b:1"));
        assert!(ds[0].from_generated);
    }

    #[test]
    fn the_no_position_marker_is_consumed_not_spoken() {
        let src = "//@warning - `fn old` is deprecated
";
        let ds = diagnostics("b", src, &ctx(src, "app"));
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].message, "`fn old` is deprecated");
        assert_eq!(ds[0].file.as_deref(), Some("b"));
        assert!(!ds[0].from_generated);
    }

    #[test]
    fn a_malformed_position_keeps_the_whole_line_as_the_message() {
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
        let src = "//@diag hint ./x.vyx:1:1 consider a shorter name\n";
        let ds = diagnostics("b", src, &ctx(src, ""));
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].severity, Severity::Warning);
        assert_eq!(ds[0].message, "hint ./x.vyx:1:1 consider a shorter name");
        assert!(ds[0].note.as_deref().unwrap().contains("hint"));
    }

    #[test]
    fn a_directive_inside_a_string_literal_is_data_not_a_control_line() {
        let src = "fn banner() -> String {\n    return \"first\n\
                   //@diag error ./elsewhere.vyx:1:1 injected by a string literal\n\
                   last\"\n}\n";
        assert!(diagnostics("b", src, &ctx(src, "app")).is_empty());

        // Outside the literal the same line is a control line.
        let real = format!("//@diag error ./elsewhere.vyx:1:1 reported by the generator\n{src}");
        let ds = diagnostics("b", &real, &ctx(&real, "app"));
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].severity, Severity::Error);
    }

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
        assert!(maps.regions_for("app/hijacked.vyx").is_empty());
    }

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

    #[test]
    fn an_origin_may_not_name_a_file_outside_the_project() {
        assert!(resolved("n:/proj/app", "/etc/passwd").is_err());
        assert!(resolved("n:/proj/app", "c:/Windows/win.ini").is_err());
        assert!(resolved("n:/proj/app", "../../../../../../../../x.vyx").is_err());
        assert!(resolved("", "../x.vyx").is_err());
        // A sibling of the importer inside the project, as a `.vyx` page mount.
        assert_eq!(
            resolved("n:/proj/client", "../app/routes/index.vyx").unwrap(),
            "n:/proj/app/routes/index.vyx"
        );

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

    #[test]
    fn the_climb_out_verdict_does_not_depend_on_the_platforms_path_shape() {
        let escape = "../../../../../../../../outside.vyx";
        for base in ["n:/proj/app", "/proj/app", "/tmp/probe/escape"] {
            assert!(
                resolved(base, escape).is_err(),
                "climbing out of `{base}` must be refused"
            );
        }
        assert_eq!(
            resolved("n:/proj/client", "../app/x.vyx").unwrap(),
            "n:/proj/app/x.vyx"
        );
        assert_eq!(
            resolved("/proj/client", "../app/x.vyx").unwrap(),
            "/proj/app/x.vyx"
        );
        // Reaching the root is legal; one `..` more is the climb.
        assert_eq!(resolved("/proj", "../x.vyx").unwrap(), "/x.vyx");
        assert!(resolved("/proj", "../../x.vyx").is_err());
        assert_eq!(resolved("n:/proj", "../x.vyx").unwrap(), "n:/x.vyx");
        assert!(resolved("n:/proj", "../../x.vyx").is_err());
    }

    #[test]
    fn a_climb_spelled_with_backslashes_is_the_same_climb() {
        assert!(resolved("n:/proj/app", r"..\..\..\..\outside.vyx").is_err());
        let c = Context::new("", "n:/proj/app", "n:/proj");
        assert!(resolve_origin_path(&c, r"..\..\outside.vyx").is_err());
        assert_eq!(
            resolve_origin_path(&c, r"..\shared\x.vyx").unwrap(),
            "n:/proj/shared/x.vyx"
        );
    }
}
