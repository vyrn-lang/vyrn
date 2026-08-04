//! Reading the symbol map a generator baked into its module (RFC-0073 M3).
//!
//! M1 made every generated module carry its own map: `std/symbolmap` renders an
//! `export fn symbolMap<Slug>() -> String` whose single `return` is the whole
//! document as a JSON string literal, appended last. This is the other end — the
//! Rust side that reads one back, for the CLI (`emit-gen --maps`, which wants the
//! text) and for the LSP (hover, go-to-definition and route lenses, which want
//! the entries).
//!
//! The `<Slug>` is M4's correction to M1: a top-level name in Vyrn is
//! program-wide, so a fixed `symbolMap` made two map-emitting generated modules
//! in one program a name collision. The declaration now carries a slug of the
//! generator call, and this reader matches the PREFIX.
//!
//! Reading is a PARSE, not a run. The map is a string literal, so nothing has to
//! execute for a consumer to hold it, and the map a consumer gets is by
//! construction the one that shipped inside the module beside the code it
//! describes.
//!
//! The parse is over the TAIL of the module rather than the whole of it: a
//! generated client is tens of kilobytes and the LSP reads maps on every
//! keystroke, so `symbolMap` is found textually and only what follows it is
//! lexed. `symbolMapFn` appends the declaration last, so the tail is the
//! function and nothing else.

use crate::ast::{Expr, Stmt};
use crate::schema::{parse_json, Json};

/// One generated symbol and the declaration it stands for.
///
/// `name` is the GENERATED name (`pastesCreate`, `rpcHandlePastesCreate`) and
/// `decl` the declared one (`create`) — routinely different, which is why M1's
/// `Origin` carries both.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedSymbol {
    pub name: String,
    /// The origin file, as the loader keys it — an absolute slash path when the
    /// root that generated the map was itself absolute (the LSP), relative to the
    /// invocation directory otherwise (the CLI).
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub decl: String,
    /// The open `derived` slot, string values only — every fact any generator
    /// writes today is a string, and a consumer that wanted a number would have
    /// to agree with the writer about which key it was.
    pub derived: Vec<(String, String)>,
}

impl MappedSymbol {
    pub fn derived(&self, key: &str) -> Option<&str> {
        self.derived.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// The wire facts as one hover line — `POST /_/pastes/create · convention` —
    /// or `None` for a symbol with nothing routable derived about it (a
    /// re-emitted type).
    pub fn route_line(&self) -> Option<String> {
        let path = self.derived("path")?;
        let method = self.derived("method").unwrap_or("POST");
        match self.derived("source") {
            Some(src) => Some(format!("`{method} {path}` · {src}")),
            None => Some(format!("`{method} {path}`")),
        }
    }
}

/// The JSON document a generated module's `symbolMap()` returns, or `None` for a
/// generator that emits no map.
pub fn json_of(gen_source: &str) -> Option<String> {
    // By PREFIX, not by exact name: a top-level name is program-wide, so the
    // declaration carries a slug of the generator call that emitted it
    // (`symbolMapHttpPastes`) and two generated modules in one program do not
    // collide.
    let start = gen_source.rfind("export fn symbolMap")?;
    let tokens = crate::lexer::lex(&gen_source[start..]).ok()?;
    let (program, _) = crate::parser::parse_accum(tokens);
    let f = program.functions.iter().find(|f| f.name.starts_with("symbolMap"))?;
    match f.body.stmts.first() {
        Some(Stmt::Return { value: Some(Expr::Str(s)), .. }) => Some(s.clone()),
        _ => None,
    }
}

/// Every symbol a generated module maps. Empty for a module with no map, and for
/// a map that will not parse — a consumer's answer to "where does this come
/// from" is then the one it gave before this RFC, never a wrong location.
pub fn read(gen_source: &str) -> Vec<MappedSymbol> {
    let Some(json) = json_of(gen_source) else { return Vec::new() };
    let Ok(doc) = parse_json(&json) else { return Vec::new() };
    let Some(Json::Arr(symbols)) = doc.get("symbols") else { return Vec::new() };
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
            file: origin.get("file").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            line: num(origin.get("line")),
            col: num(origin.get("col")),
            decl: origin.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            derived,
        });
    }
    out
}

/// Whether `file` is the origin file `key` names, comparing as the loader keys
/// modules: slash-normalized, case-insensitively on the first character (a
/// Windows drive letter reaches the LSP in either case).
pub fn same_file(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.replace('\\', "/");
        let mut c = s.chars();
        match c.next() {
            Some(d) => d.to_ascii_lowercase().to_string() + c.as_str(),
            None => s,
        }
    };
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `std/symbolmap` renders, with the tail this reader relies on.
    const GEN: &str = "fn stub() -> Int64 {\n    return 0\n}\n\
/// The RFC-0073 symbol map for this generated module.\n\
export fn symbolMapClientApi() -> String {\n    return \"{\\\"module\\\":\\\"client(./api)\\\",\\\"symbols\\\":[{\\\"name\\\":\\\"pastesCreate\\\",\\\"origin\\\":{\\\"file\\\":\\\"server/api/pastes.vyrn\\\",\\\"line\\\":28,\\\"col\\\":15,\\\"name\\\":\\\"create\\\"},\\\"derived\\\":{\\\"kind\\\":\\\"rpc\\\",\\\"method\\\":\\\"POST\\\",\\\"path\\\":\\\"/_/pastes/create\\\",\\\"source\\\":\\\"convention\\\"}},{\\\"name\\\":\\\"PasteList\\\",\\\"origin\\\":{\\\"file\\\":\\\"shared/wire.vyrn\\\",\\\"line\\\":12,\\\"col\\\":13,\\\"name\\\":\\\"PasteList\\\"}}]}\"\n}\n";

    #[test]
    fn a_baked_map_reads_back_with_its_origins_and_derived_facts() {
        let syms = read(GEN);
        assert_eq!(syms.len(), 2, "{syms:#?}");
        assert_eq!(syms[0].name, "pastesCreate");
        assert_eq!(syms[0].decl, "create");
        assert_eq!((syms[0].line, syms[0].col), (28, 15));
        assert_eq!(syms[0].file, "server/api/pastes.vyrn");
        assert_eq!(syms[0].route_line().as_deref(), Some("`POST /_/pastes/create` · convention"));
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
    }
}
