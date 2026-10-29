//! Origin maps. A generated module's span table says where each span's errors
//! show: a `shown` path, relative to the importing module, and a 1-based
//! position. [`OriginMaps::add_spans`] reads those regions,
//! [`OriginMaps::remap`] moves a diagnostic to its origin, and
//! [`OriginMaps::regions_for`] inverts the table for the LSP. A malformed
//! origin never loses a diagnostic: it stays at the generated location with a
//! note.

use crate::diagnostics::{Diagnostic, Severity};
use std::collections::HashMap;

/// What a generated module's origins resolve under.
pub struct Context<'a> {
    /// Directory of the module that wrote the generator call; origin paths
    /// resolve against it.
    pub importer_dir: &'a str,
    /// The manifest's directory, or the entry file's when there is none. An
    /// origin naming a file outside it is malformed. Empty only in unit tests,
    /// where only `importer_dir`'s root bounds a path.
    pub project: &'a str,
}

/// The input position an origin points at.
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
    /// The first governed generated line (1-based).
    gen_line: usize,
    /// `None` where a region ends, or for a malformed origin.
    origin: Option<Origin>,
    /// Why the origin is malformed. Its region is recorded but not remapped.
    malformed: Option<String>,
}

/// An input-file position mapped to the generated span it governs, for the
/// LSP's forward requests.
#[derive(Debug, Clone)]
pub struct Region {
    /// The generated module's key.
    pub gen_module: String,
    pub origin: Origin,
    /// 1-based, inclusive.
    pub gen_start_line: usize,
    /// 1-based, inclusive.
    pub gen_end_line: usize,
}

#[derive(Debug, Clone, Default)]
pub struct OriginMaps {
    /// Module key to its regions' starts, sorted by `gen_line`.
    modules: HashMap<String, Vec<Directive>>,
    /// Generated line count per module, for the last region's end.
    module_lines: HashMap<String, usize>,
}

impl OriginMaps {
    pub fn new() -> Self {
        OriginMaps::default()
    }

    /// Reads the regions of the generated module `banner` from its span table: a span with a `shown` position governs from the line
    /// it starts on, and the next span without one ends the region after the
    /// last line the region touches. A run of spans with one `shown` position
    /// is one region, as `at` writes it.
    ///
    /// `rows` is sorted by offset (`gen::Written`), so one walk counts each
    /// newline once: linear in the source, not rows times source.
    pub fn add_spans(
        &mut self,
        banner: &str,
        source: &str,
        rows: &[crate::gen::SpanRow],
        ctx: &Context<'_>,
    ) {
        let mut dirs: Vec<Directive> = Vec::new();
        let mut open: Option<&crate::gen::Shown> = None;
        let (mut gen_line, mut counted) = (1, 0);
        for r in rows {
            gen_line += source[counted..r.offset].matches('\n').count();
            counted = r.offset;
            match &r.shown {
                Some(s) if open == Some(s) => {}
                Some(s) => {
                    let (origin, malformed) =
                        match anchor(&format!("{}:{}:{}", s.path, s.at.line, s.at.col), ctx) {
                            Ok(o) => (Some(o), None),
                            Err(reason) => (None, Some(reason)),
                        };
                    dirs.push(Directive {
                        gen_line,
                        origin,
                        malformed,
                    });
                    open = Some(s);
                }
                None if open.is_some() => {
                    let mid_line = !source[..r.offset].ends_with('\n');
                    dirs.push(Directive {
                        gen_line: gen_line + mid_line as usize,
                        origin: None,
                        malformed: None,
                    });
                    open = None;
                }
                None => {}
            }
        }
        self.insert(banner, dirs, source.lines().count());
    }

    /// Records `banner`'s directives, sorted by line, and its line count.
    fn insert(&mut self, banner: &str, mut dirs: Vec<Directive>, total: usize) {
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
    /// returns whether it moved. Past a region's end or under a malformed origin
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
                "malformed origin ({reason}); reported at generated location"
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
                // The region ends on the line before the next start, or at
                // EOF. Back-to-back starts clamp to `end == start`, never a
                // backwards span.
                let end = dirs
                    .get(idx + 1)
                    .map(|n| n.gen_line.saturating_sub(1))
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

    /// Returns every input file some origin names, in first-seen order.
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

/// The anchor a report writes when it has no position to give.
const NO_POSITION: &str = "-";

/// Returns the report `line`, `<error|warning> <anchor> <message>`, that
/// generated module `banner` carries at generated line `gen_line`, as a
/// diagnostic; `None` for a report without a message. The anchor is a
/// `path:line:col` origin; `-` or a malformed anchor reports at the generated
/// location, the latter keeping it in the message. An unknown severity becomes
/// a warning holding the whole line, so a newer generator neither fails nor
/// vanishes.
pub fn report(banner: &str, gen_line: usize, line: &str, ctx: &Context<'_>) -> Option<Diagnostic> {
    let body = line.trim();
    let (severity, rest, unknown) = match body.split_once(char::is_whitespace) {
        Some(("error", tail)) => (Severity::Error, tail.trim(), None),
        Some(("warning", tail)) => (Severity::Warning, tail.trim(), None),
        // Unknown severity: the anchor is not parsed.
        Some((word, _)) => (Severity::Warning, body, Some(word.to_string())),
        None => (Severity::Warning, body, Some(body.to_string())),
    };
    let (pos, message) = match unknown {
        Some(_) => (None, rest),
        None => match rest.split_once(char::is_whitespace) {
            Some((NO_POSITION, tail)) => (None, tail.trim()),
            Some((head, tail)) => match anchor(head, ctx) {
                Ok(origin) => (Some(origin), tail.trim()),
                Err(_) => (None, rest),
            },
            None => (None, rest),
        },
    };
    if message.is_empty() {
        return None;
    }
    let stage = match severity {
        Severity::Error => "generated.error",
        Severity::Warning => "generated.warning",
    };
    let mut d = Diagnostic::error(gen_line, 0, stage, message.to_string());
    d.severity = severity;
    match pos {
        Some(origin) => {
            d.file = Some(origin.file);
            d.line = origin.line;
            d.col = origin.col;
            d.from_generated = true;
            d.note = Some(format!(
                "in generated code {}:{gen_line} (see `vyrn emit-gen`)",
                crate::loader::readable(banner),
            ));
        }
        None => d.file = Some(banner.to_string()),
    }
    if let Some(word) = unknown {
        d.note = Some(format!(
            "unrecognized severity `{word}` in a report; reported as a warning"
        ));
    }
    Some(d)
}

/// Parses an anchor, `<path>:<line>:<col>`, splitting from the right so the
/// path keeps any interior colon, and resolves the path. `Err` says why the
/// anchor is malformed.
pub fn anchor(body: &str, ctx: &Context<'_>) -> Result<Origin, String> {
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

/// Resolves an origin path against the importer into a normalized slash key.
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
    use crate::gen::{Author, Pos, Shown, SpanRow};
    use std::sync::Arc;

    fn diag(file: &str, line: usize, col: usize) -> Diagnostic {
        let mut d = Diagnostic::error(line, col, "check", "boom".to_string());
        d.file = Some(file.to_string());
        d
    }

    /// A context with no project bound, as an in-memory load has.
    fn ctx(importer_dir: &str) -> Context<'_> {
        Context {
            importer_dir,
            project: "",
        }
    }

    fn resolved(importer_dir: &str, path: &str) -> Result<String, String> {
        resolve_origin_path(&ctx(importer_dir), path)
    }

    /// Maps of module `banner`, written as `spans`: each a text and the
    /// `path:line:col` its errors show at, if any.
    fn maps(banner: &str, spans: &[(&str, Option<&str>)], c: &Context<'_>) -> OriginMaps {
        let (mut source, mut rows) = (String::new(), Vec::new());
        for (text, shown) in spans {
            let shown = shown.map(|s| {
                let mut f = s.rsplitn(3, ':');
                let col = f.next().unwrap().parse().unwrap();
                let line = f.next().unwrap().parse().unwrap();
                let at = Pos { line, col };
                let path = Arc::from(f.next().unwrap());
                Shown { path, at }
            });
            let (offset, author) = (source.len(), Author::CallSite);
            rows.push(SpanRow {
                offset,
                author,
                shown,
            });
            source.push_str(text);
        }
        let mut maps = OriginMaps::new();
        maps.add_spans(banner, &source, &rows, c);
        maps
    }

    #[test]
    fn remaps_a_governed_line_to_its_origin() {
        let banner = "generated by components(\"./comp\") at app.vyrn";
        let spans = [
            ("line1\n", None),
            ("row.push(x)\nmore\n", Some("./comp/Item.vyx:14:9")),
        ];
        let maps = maps(banner, &spans, &ctx(""));
        let mut d = diag(banner, 2, 5);
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
        let spans = [("<expr>\n", Some("./comp/Widget.vyx:6:8"))];
        let maps = maps(banner, &spans, &ctx("/tmp/probe"));
        assert_eq!(
            maps.input_files(),
            vec!["/tmp/probe/comp/Widget.vyx".to_string()]
        );
        assert_eq!(maps.regions_for("/tmp/probe/comp/Widget.vyx").len(), 1);
    }

    #[test]
    fn a_span_without_an_origin_ends_the_region() {
        let spans = [("x\n", Some("./a.vyx:1:1")), ("y\n", None)];
        let maps = maps("b", &spans, &ctx(""));
        let mut governed = diag("b", 1, 1);
        assert!(maps.remap(&mut governed));
        let mut ungoverned = diag("b", 2, 1);
        assert!(!maps.remap(&mut ungoverned));
        assert_eq!(ungoverned.file.as_deref(), Some("b"));
    }

    #[test]
    fn malformed_origin_never_loses_the_diagnostic() {
        let spans = [("x\n", Some("../../../../../../../../outside.vyx:1:1"))];
        let maps = maps("b", &spans, &ctx("app"));
        let mut d = diag("b", 1, 1);
        assert!(!maps.remap(&mut d));
        assert_eq!(d.file.as_deref(), Some("b"));
        assert!(d.note.as_deref().unwrap().contains("malformed"));
    }

    #[test]
    fn resolves_paths_against_the_importer_dir() {
        let maps = maps("b", &[("x\n", Some("./ItemRow.vyx:2:3"))], &ctx("src/ui"));
        let mut d = diag("b", 1, 1);
        maps.remap(&mut d);
        assert_eq!(d.file.as_deref(), Some("src/ui/ItemRow.vyx"));
    }

    #[test]
    fn inverts_to_regions_for_forward_mapping() {
        let spans = [
            ("a\n", None),
            ("b\nc\n", Some("./x.vyx:5:2")),
            ("d\n", None),
        ];
        let regions = maps("b", &spans, &ctx("")).regions_for("x.vyx");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].gen_start_line, 2);
        assert_eq!(regions[0].gen_end_line, 3);
        assert_eq!(regions[0].origin.line, 5);
    }

    #[test]
    fn a_positioned_warning_points_at_the_input_file() {
        let line = "warning ./routes/p/[id].vyx:13:1 `fn old` is deprecated";
        let d = report("b", 1, line, &ctx("app")).unwrap();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.file.as_deref(), Some("app/routes/p/[id].vyx"));
        assert_eq!((d.line, d.col), (13, 1));
        assert_eq!(d.message, "`fn old` is deprecated");
        assert!(d.note.as_deref().unwrap().contains("generated code b:1"));
        assert!(d.from_generated);
    }

    #[test]
    fn the_no_position_marker_is_consumed_not_spoken() {
        let d = report("b", 1, "warning - `fn old` is deprecated", &ctx("app")).unwrap();
        assert_eq!(d.message, "`fn old` is deprecated");
        assert_eq!(d.file.as_deref(), Some("b"));
        assert!(!d.from_generated);
    }

    #[test]
    fn a_malformed_position_keeps_the_whole_line_as_the_message() {
        let d = report("b", 1, "warning whoops something is off", &ctx("")).unwrap();
        assert_eq!(d.message, "whoops something is off");
        assert_eq!(d.file.as_deref(), Some("b"));
    }

    #[test]
    fn an_empty_report_says_nothing() {
        assert!(report("b", 1, "", &ctx("")).is_none());
    }

    #[test]
    fn the_severity_is_the_generators_to_choose() {
        let line = "error ./schema/users.tbl:14:3 column `id` is declared twice";
        let d = report("b", 1, line, &ctx("app")).unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.file.as_deref(), Some("app/schema/users.tbl"));
        assert_eq!((d.line, d.col), (14, 3));
        assert_eq!(d.message, "column `id` is declared twice");
        assert!(d.from_generated);
    }

    #[test]
    fn an_unrecognized_severity_neither_escalates_nor_vanishes() {
        let line = "hint ./x.vyx:1:1 consider a shorter name";
        let d = report("b", 1, line, &ctx("")).unwrap();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.message, line);
        assert!(d.note.as_deref().unwrap().contains("hint"));
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
        let c = Context {
            importer_dir: "n:/proj/app",
            project: "n:/proj",
        };
        assert!(resolve_origin_path(&c, "../../other/x.vyx").is_err());
        assert_eq!(
            resolve_origin_path(&c, "../shared/x.vyx").unwrap(),
            "n:/proj/shared/x.vyx"
        );
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
        let c = Context {
            importer_dir: "n:/proj/app",
            project: "n:/proj",
        };
        assert!(resolve_origin_path(&c, r"..\..\outside.vyx").is_err());
        assert_eq!(
            resolve_origin_path(&c, r"..\shared\x.vyx").unwrap(),
            "n:/proj/shared/x.vyx"
        );
    }
}
