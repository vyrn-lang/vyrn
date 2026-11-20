//! The CSS classes a `.vyx` component can name, from every source its app has,
//! in this order: the component's own `<style>` block (local), the stylesheets
//! the app's layouts link with `stylesheet "..."`, and the files the manifest's
//! `stylesheets` globs name (both global). The editor's class completion, hover
//! and definition and the two style-block warnings ([`warnings`]) read these
//! rules; a theme's utility alphabet is the editor's to add.
//!
//! The scan is textual: a rule's selector is the text before its `{`, a class is
//! a `.` and the name after it, and an at-rule's body is searched. It knows no
//! cascade.

use std::path::{Path, PathBuf};

use crate::diagnostics::Diagnostic;

/// The most stylesheets one app's layouts and globs name.
pub const MAX_STYLESHEETS: usize = 64;

/// Where a rule comes from, in the order the sources are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// The component's own style block.
    Local,
    /// A stylesheet an app layout links with `stylesheet "..."`.
    Linked,
    /// A file the manifest's `stylesheets` globs name.
    Globbed,
}

impl Source {
    /// The word the editor shows for the source.
    pub fn label(self) -> &'static str {
        match self {
            Source::Local => "local",
            Source::Linked | Source::Globbed => "global",
        }
    }
}

/// A stylesheet and the byte range of its text that holds rules: the whole file,
/// or a component's style block.
#[derive(Clone, Debug)]
pub struct Sheet {
    pub file: PathBuf,
    pub source: Source,
    pub text: String,
    pub range: (usize, usize),
}

/// One class a rule's selector names, at the 1-based line and column of its `.`
/// in `file`. `text` is the whole rule as written.
#[derive(Clone, Debug)]
pub struct Rule {
    pub class: String,
    pub source: Source,
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
    pub text: String,
}

/// The style block of the `.vyx` text `text` at `file`: the text between the
/// `<style>` after the last `</template>` and its `</style>`. `None` without one.
pub fn local_sheet(file: &Path, text: &str) -> Option<Sheet> {
    let after = text.rfind("</template>")?;
    let open = after + text[after..].find("<style>")? + "<style>".len();
    let close = open + text[open..].find("</style>")?;
    Some(Sheet {
        file: file.to_path_buf(),
        source: Source::Local,
        text: text.to_string(),
        range: (open, close),
    })
}

/// The global stylesheets of the app at `root`, linked first, then globbed, each
/// file once, at most [`MAX_STYLESHEETS`]. A linked URL names the file under the
/// manifest's `public` directory (default `public`); an URL with no such file is
/// skipped, because code serves it. Without a manifest the app has none.
pub fn app_sheet_paths(root: &Path) -> Vec<(PathBuf, Source)> {
    let Ok(Some(manifest)) = crate::manifest::find(root) else {
        return Vec::new();
    };
    let public = match manifest.doc.get("public") {
        Some(crate::schema::Json::Str(p)) => p.clone(),
        _ => "public".to_string(),
    };
    let mut out: Vec<(PathBuf, Source)> = Vec::new();
    let mut layouts = Vec::new();
    crate::manifest::glob_files(root, &["**", "layout.vyx"], MAX_STYLESHEETS, &mut layouts);
    for layout in layouts {
        let Ok(text) = std::fs::read_to_string(&layout) else {
            continue;
        };
        for url in linked_urls(&text) {
            let file = root.join(&public).join(url.trim_start_matches('/'));
            if file.is_file() && !out.iter().any(|(f, _)| *f == file) {
                out.push((file, Source::Linked));
            }
        }
    }
    let mut globbed = Vec::new();
    for g in &manifest.stylesheets {
        let segs = crate::manifest::glob_segs(g);
        crate::manifest::glob_files(root, &segs, MAX_STYLESHEETS, &mut globbed);
    }
    for file in globbed {
        if !out.iter().any(|(f, _)| *f == file) {
            out.push((file, Source::Globbed));
        }
    }
    out.truncate(MAX_STYLESHEETS);
    out
}

/// The stylesheet URLs a layout's `head { .. }` block links: each line whose text
/// starts with `stylesheet "`, as `std/vyx` reads a head entry.
fn linked_urls(layout: &str) -> Vec<String> {
    (layout.lines())
        .filter_map(|l| l.trim().strip_prefix("stylesheet \""))
        .filter_map(|rest| rest.split_once('"').map(|(url, _)| url.to_string()))
        .filter(|url| url.starts_with('/'))
        .collect()
}

/// The sheets of `paths` as read now; a file that cannot be read is skipped.
pub fn read_sheets(paths: Vec<(PathBuf, Source)>) -> Vec<Sheet> {
    (paths.into_iter())
        .filter_map(|(file, source)| {
            let text = std::fs::read_to_string(&file).ok()?;
            let range = (0, text.len());
            Some(Sheet {
                file,
                source,
                text,
                range,
            })
        })
        .collect()
}

/// Every class each sheet's rules name, in sheet order, then file order.
pub fn rules(sheets: &[Sheet]) -> Vec<Rule> {
    let mut out = Vec::new();
    for s in sheets {
        let (from, to) = s.range;
        scan(s, from, to, &mut out);
    }
    out
}

/// Scans the rules of `sheet.text[from..to]`, a sheet or an at-rule body.
fn scan(sheet: &Sheet, from: usize, to: usize, out: &mut Vec<Rule>) {
    let css = &sheet.text;
    let b = css.as_bytes();
    let mut i = from;
    let mut sel = from;
    while i < to {
        // A comment after selector text keeps it (`.a /* x */ {`); a comment
        // standing alone resets the scan, so its words never leak.
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            let started = !css[sel..i].trim().is_empty();
            match css[i + 2..to].find("*/") {
                Some(rel) => i += 2 + rel + 2,
                None => return,
            }
            if !started {
                sel = i;
            }
            continue;
        }
        match b[i] {
            b'{' => {
                let Some(close) = matching_brace(css, i, to) else {
                    return;
                };
                let lead = css[sel..i].len() - css[sel..i].trim_start().len();
                let start = sel + lead;
                if css[start..i].starts_with('@') {
                    scan(sheet, i + 1, close, out);
                } else {
                    let text = css[start..=close].to_string();
                    for (class, at) in selector_classes(&css[start..i]) {
                        let (line, col) = line_col(css, start + at);
                        out.push(Rule {
                            class,
                            source: sheet.source,
                            file: sheet.file.clone(),
                            line,
                            col,
                            text: text.clone(),
                        });
                    }
                }
                i = close + 1;
                sel = i;
            }
            b'}' | b';' => {
                i += 1;
                sel = i;
            }
            _ => i += 1,
        }
    }
}

/// Byte index of the `}` before `to` closing the `{` at `open`, comment-aware.
fn matching_brace(css: &str, open: usize, to: usize) -> Option<usize> {
    let b = css.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < to {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2 + css[i + 2..to].find("*/")? + 2;
            continue;
        }
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Each class `selector` names, unescaped, with the byte offset of its `.`. A
/// `.` inside brackets or a string, as in `[href$=".css"]`, names none.
fn selector_classes(selector: &str) -> Vec<(String, usize)> {
    let b = selector.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote = 0u8;
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == b'\\' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'[' {
            depth += 1;
        } else if c == b']' {
            depth -= 1;
        } else if c == b'.' && depth == 0 {
            let mut name = String::new();
            let mut j = i + 1;
            while j < b.len() {
                let d = b[j];
                if d == b'\\' && j + 1 < b.len() {
                    name.push(b[j + 1] as char);
                    j += 2;
                } else if d.is_ascii_alphanumeric() || d == b'-' || d == b'_' || d >= 0x80 {
                    let ch = selector[j..].chars().next().unwrap_or('-');
                    name.push(ch);
                    j += ch.len_utf8();
                } else {
                    break;
                }
            }
            if !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) {
                out.push((name, i));
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// The 1-based line and character column of byte `at` of `text`.
fn line_col(text: &str, at: usize) -> (usize, usize) {
    let before = &text[..at];
    let line = 1 + before.matches('\n').count();
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, 1 + before[start..].chars().count())
}

/// The two style-block warnings for the components whose class map the
/// generated module `gen_source` holds ([`crate::symbolmap`], the map `std/vyx`
/// writes for each component with a style block). A class the template names
/// that no rule of the component's sources defines is one; a local rule's class
/// the template never names is the other. A themed module checks its classes
/// against its theme, so it gets no first warning; a component with a `:class`
/// the compiler cannot read gets no second. The files are read through `read`.
pub fn warnings(
    gen_source: &str,
    ctx: &crate::origin::Context<'_>,
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<Diagnostic> {
    if !gen_source.contains("export fn symbolMapVyxClasses") {
        return Vec::new();
    }
    let rows = crate::symbolmap::read(gen_source);
    fn kind(r: &crate::symbolmap::MappedSymbol) -> &str {
        r.derived("kind").unwrap_or("")
    }
    let themed = rows.iter().any(|r| kind(r) == "class-theme");
    let place = |r: &crate::symbolmap::MappedSymbol| {
        crate::origin::anchor(&format!("{}:{}:{}", r.file, r.line, r.col), ctx).ok()
    };
    let mut out = Vec::new();
    let mut global: Option<Vec<Rule>> = None;
    for block in rows.iter().filter(|r| kind(r) == "class-block") {
        let Some(at) = place(block) else {
            continue;
        };
        let file = at.file;
        let Some(text) = read(&file) else {
            continue;
        };
        let Some(sheet) = local_sheet(Path::new(&file), &text) else {
            continue;
        };
        let local = rules(std::slice::from_ref(&sheet));
        let mine: Vec<&crate::symbolmap::MappedSymbol> =
            (rows.iter()).filter(|r| r.file == block.file).collect();
        let uses: Vec<&crate::symbolmap::MappedSymbol> = (mine.iter().copied())
            .filter(|r| kind(r) == "class")
            .collect();
        let named = |c: &str| uses.iter().any(|u| u.name.strip_prefix('.') == Some(c));
        for u in uses.iter().filter(|_| !themed) {
            let class = u.name.trim_start_matches('.');
            if local.iter().any(|r| r.class == class) {
                continue;
            }
            let global = global.get_or_insert_with(|| {
                let dir = Path::new(&file).parent().unwrap_or(Path::new("."));
                rules(&read_sheets(app_sheet_paths(&crate::manifest::app_root(
                    dir,
                ))))
            });
            if global.iter().any(|r| r.class == class) {
                continue;
            }
            if let Some(at) = place(u) {
                out.push(warning(
                    &at,
                    format!(
                        "no stylesheet defines the class `{class}`: not this component's style block, a stylesheet a layout links, nor a `stylesheets` glob of the manifest"
                    ),
                ));
            }
        }
        if mine.iter().any(|r| kind(r) == "class-dynamic") {
            continue;
        }
        let mut told: Vec<&str> = Vec::new();
        for r in &local {
            if named(&r.class) || told.contains(&r.class.as_str()) {
                continue;
            }
            told.push(&r.class);
            let at = crate::origin::Origin {
                file: file.clone(),
                line: r.line,
                col: r.col,
            };
            out.push(warning(
                &at,
                format!(
                    "the rule for `.{}` styles nothing: this component's template names no class `{}`",
                    r.class, r.class
                ),
            ));
        }
    }
    out
}

/// A style-block warning at `at`.
fn warning(at: &crate::origin::Origin, message: String) -> Diagnostic {
    let mut d = Diagnostic::warning(at.line, at.col, "vyx.class", message);
    d.file = Some(at.file.clone());
    d.from_generated = true;
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(text: &str) -> Sheet {
        Sheet {
            file: PathBuf::from("a.css"),
            source: Source::Globbed,
            text: text.to_string(),
            range: (0, text.len()),
        }
    }

    fn classes(text: &str) -> Vec<(String, usize, usize)> {
        (rules(&[sheet(text)]).into_iter())
            .map(|r| (r.class, r.line, r.col))
            .collect()
    }

    #[test]
    fn a_selector_list_names_each_class_where_its_dot_stands() {
        assert_eq!(
            classes(".a, li.b:hover > .c-d { x: y; }"),
            [
                ("a".into(), 1, 1),
                ("b".into(), 1, 7),
                ("c-d".into(), 1, 18)
            ]
        );
    }

    #[test]
    fn an_at_rule_body_is_searched_and_a_comment_or_attribute_names_nothing() {
        let css = "/* .no */\n@media (x) {\n  .in { a: b; }\n}\n[href$=\".css\"] { c: d; }\n";
        assert_eq!(classes(css), [("in".into(), 3, 3)]);
    }

    #[test]
    fn a_selector_before_an_inline_comment_still_names_its_class() {
        let r = rules(&[sheet(
            ".plang /* legacy */ { color: red }\n.other { color: blue }\n",
        )]);
        assert_eq!((r[0].class.as_str(), r[0].line), ("plang", 1));
        assert!(r[0].text.contains("color: red"), "{}", r[0].text);
        // A comment standing alone does not leak into the next selector.
        let spaced = ".a { x: 1 }\n/* .plang mention */\n.b { y: 2 }\n";
        assert!(classes(spaced).iter().all(|(c, _, _)| c != "plang"));
    }

    #[test]
    fn an_escaped_variant_unescapes() {
        assert_eq!(classes(".md\\:p-2 { a: b; }"), [("md:p-2".into(), 1, 1)]);
    }

    #[test]
    fn the_local_sheet_is_the_block_after_the_template() {
        let vyx =
            "<template>\n<p><style>x</style></p>\n</template>\n<style>\n.k { a: b; }\n</style>\n";
        let s = local_sheet(Path::new("C.vyx"), vyx).unwrap();
        let r = rules(&[s]);
        assert_eq!((r[0].class.as_str(), r[0].line, r[0].col), ("k", 5, 1));
        assert!(local_sheet(Path::new("C.vyx"), "<template>\n<p></p>\n</template>\n").is_none());
    }

    #[test]
    fn a_layout_links_each_head_stylesheet() {
        let layout = "<script>\nhead {\n    stylesheet \"/a.css\"\n    module \"/m.js\"\n    stylesheet \"https://x/b.css\"\n}\n</script>\n";
        assert_eq!(linked_urls(layout), ["/a.css"]);
    }
}
