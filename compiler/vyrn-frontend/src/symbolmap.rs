//! Reads the symbol map a generator bakes into its module, for the
//! CLI (`emit-gen --maps`, which wants the text) and the LSP (hover,
//! go-to-definition and route lenses, which want the entries).
//!
//! `std/symbolmap` appends `export fn symbolMap<Slug>() -> String`, whose one
//! `return` is the whole document as a JSON string literal. The slug keeps two
//! map-emitting modules in one program from colliding, so this reader matches
//! the prefix. Reading is a parse, not a run, and only the tail after the last
//! `symbolMap` is lexed: a generated client is tens of kilobytes and the LSP
//! reads maps on every keystroke.

use crate::ast::{Expr, Stmt};
use crate::schema::{parse_json, Json};

/// One generated symbol and the declaration it stands for.
///
/// `name` is the generated name (`pastesCreate`) and `decl` the declared one
/// (`create`); they routinely differ.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedSymbol {
    pub name: String,
    /// The origin file as the loader keys it: absolute when the generating root
    /// was absolute (the LSP), relative to the invocation directory otherwise
    /// (the CLI).
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub decl: String,
    /// The open `derived` slot, string values only: every fact a generator writes
    /// is a string.
    pub derived: Vec<(String, String)>,
}

impl MappedSymbol {
    pub fn derived(&self, key: &str) -> Option<&str> {
        self.derived
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Returns the wire facts as one hover line, such as
    /// `POST /_/pastes/create` with its source, or `None` when nothing routable is
    /// derived (a re-emitted type).
    pub fn route_line(&self) -> Option<String> {
        let path = self.derived("path")?;
        let method = self.derived("method").unwrap_or("POST");
        match self.derived("source") {
            Some(src) => Some(format!("`{method} {path}` · {src}")),
            None => Some(format!("`{method} {path}`")),
        }
    }
}

/// Returns the JSON document a generated module's `symbolMap` function
/// returns, or `None` for a generator that emits no map.
pub fn json_of(gen_source: &str) -> Option<String> {
    // By prefix: the name carries a slug of the generator call
    // (`symbolMapHttpPastes`).
    let start = gen_source.rfind("export fn symbolMap")?;
    let tokens = crate::lexer::lex(&gen_source[start..]).ok()?;
    let (program, _) = crate::parser::parse_accum(tokens);
    let f = program
        .functions
        .iter()
        .find(|f| f.name.starts_with("symbolMap"))?;
    match f.body.stmts.first() {
        Some(Stmt::Return {
            value: Some(Expr::Str(s, _)),
            ..
        }) => Some(s.clone()),
        _ => None,
    }
}

/// Returns every symbol a generated module maps. Empty for no map or a map
/// that does not parse, so a consumer never shows a wrong location.
pub fn read(gen_source: &str) -> Vec<MappedSymbol> {
    let Some(json) = json_of(gen_source) else {
        return Vec::new();
    };
    let Ok(doc) = parse_json(&json) else {
        return Vec::new();
    };
    let Some(Json::Arr(symbols)) = doc.get("symbols") else {
        return Vec::new();
    };
    let num = |v: Option<&Json>| match v {
        Some(Json::Num(n)) => *n as usize,
        _ => 0,
    };
    let mut out = Vec::new();
    for s in symbols {
        let (Some(name), Some(origin)) = (s.get("name").and_then(|v| v.as_str()), s.get("origin"))
        else {
            continue;
        };
        let derived = match s.get("derived") {
            Some(Json::Obj(fields)) => fields
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect(),
            _ => Vec::new(),
        };
        out.push(MappedSymbol {
            name: name.to_string(),
            file: origin
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            line: num(origin.get("line")),
            col: num(origin.get("col")),
            decl: origin
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            derived,
        });
    }
    out
}

/// Returns whether `a` and `b` name the same origin file, compared as the
/// loader keys modules: slash-normalized, and on Windows case-insensitive over
/// the whole path. `OriginMaps::norm_path_key` lowercases every LSP key, while
/// loader and `read_dir` paths keep the case on disk.
pub fn same_file(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.replace('\\', "/");
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            let mut c = s.chars();
            match c.next() {
                Some(d) => d.to_ascii_lowercase().to_string() + c.as_str(),
                None => s,
            }
        }
    };
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `std/symbolmap` renders, with the tail this reader relies on.
    const GEN: &str = "fn stub() -> Int64 {\n    return 0\n}\n\
/// The symbol map for this generated module.\n\
export fn symbolMapClientApi() -> String {\n    return \"{\\\"module\\\":\\\"client(./api)\\\",\\\"symbols\\\":[{\\\"name\\\":\\\"pastesCreate\\\",\\\"origin\\\":{\\\"file\\\":\\\"server/api/pastes.vyrn\\\",\\\"line\\\":28,\\\"col\\\":15,\\\"name\\\":\\\"create\\\"},\\\"derived\\\":{\\\"kind\\\":\\\"rpc\\\",\\\"method\\\":\\\"POST\\\",\\\"path\\\":\\\"/_/pastes/create\\\",\\\"source\\\":\\\"convention\\\"}},{\\\"name\\\":\\\"PasteList\\\",\\\"origin\\\":{\\\"file\\\":\\\"shared/wire.vyrn\\\",\\\"line\\\":12,\\\"col\\\":13,\\\"name\\\":\\\"PasteList\\\"}}]}\"\n}\n";

    #[test]
    fn a_baked_map_reads_back_with_its_origins_and_derived_facts() {
        let syms = read(GEN);
        assert_eq!(syms.len(), 2, "{syms:#?}");
        assert_eq!(syms[0].name, "pastesCreate");
        assert_eq!(syms[0].decl, "create");
        assert_eq!((syms[0].line, syms[0].col), (28, 15));
        assert_eq!(syms[0].file, "server/api/pastes.vyrn");
        assert_eq!(
            syms[0].route_line().as_deref(),
            Some("`POST /_/pastes/create` · convention")
        );
        // A re-emitted type keeps its origin and derives nothing.
        assert_eq!(syms[1].decl, "PasteList");
        assert_eq!(syms[1].route_line(), None);
    }

    #[test]
    fn a_module_with_no_map_yields_nothing_rather_than_a_wrong_location() {
        assert!(read("fn stub() -> Int64 {\n    return 0\n}\n").is_empty());
        assert!(read("export fn symbolMap() -> String {\n    return \"{oops\"\n}\n").is_empty());
    }

    #[test]
    fn a_windows_drive_letter_in_either_case_is_the_same_file() {
        assert!(same_file("N:/lang/a.vyrn", "n:/lang/a.vyrn"));
        assert!(same_file("N:\\lang\\a.vyrn", "N:/lang/a.vyrn"));
        assert!(!same_file("N:/lang/a.vyrn", "N:/lang/b.vyrn"));
        // On Windows the whole path folds, as `norm_path_key` does.
        if cfg!(windows) {
            assert!(same_file(
                "n:/dev/myapp/server/api/pastes.vyrn",
                "N:/Dev/MyApp/Server/Api/Pastes.vyrn"
            ));
            assert!(!same_file(
                "n:/dev/myapp/server/api/users.vyrn",
                "N:/Dev/MyApp/Server/Api/Pastes.vyrn"
            ));
        }
    }
}
