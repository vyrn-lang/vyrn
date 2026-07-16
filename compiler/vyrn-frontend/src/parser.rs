//! Recursive-descent parser with precedence climbing for expressions.

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::lexer::{Tok, Token};

/// Parse a token stream into a [`Program`].
///
/// Returns the *first* parse error (the historical single-error surface). For
/// **all** parse errors recovered in one pass, use [`parse_accum`].
pub fn parse(tokens: Vec<Token>) -> Result<Program, Diagnostic> {
    let (program, errors) = parse_accum(tokens);
    match errors.into_iter().next() {
        Some(d) => Err(d),
        None => Ok(program),
    }
}

/// Parse a token stream into a ([`Program`], `Vec<Diagnostic>`), recovering past
/// bad top-level declarations so **multiple** parse errors are reported in one
/// pass (RFC-0006 recovery). Each declaration's parser stays first-error (its
/// internals use `?`); recovery happens only at the top level: when a
/// `fn`/`type`/`protocol`/`impl`/`logging` declaration fails, the diagnostic is
/// recorded and the cursor synchronizes to the next top-level declaration
/// starter (skipping nested braces) before continuing. The returned `Program`
/// holds whatever declarations *did* parse cleanly; callers that surface
/// diagnostics should treat a non-empty error list as "do not run downstream
/// checks" (a partial program would only produce cascading type errors).
pub fn parse_accum(tokens: Vec<Token>) -> (Program, Vec<Diagnostic>) {
    let (mut program, errors) =
        Parser {
            tokens,
            pos: 0,
            no_struct: false,
            type_params: Vec::new(),
            field_preds: None,
            extra_stmts: Vec::new(),
            errors: Vec::new(),
        }
        .program_accum();
    // The built-in `Value` enum (RFC-0007): the closed set of types a tagged
    // template can interpolate. Injected so every program can name `Array<Value>`
    // and match `IntVal`/`StrVal`/`BoolVal` — the tag surface — without a `use`.
    program.type_decls.push(TypeDecl {
        name: "Value".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Enum(vec![
            EnumVariant { name: "IntVal".to_string(), payload: vec![Type::Int] },
            EnumVariant { name: "StrVal".to_string(), payload: vec![Type::Str] },
            EnumVariant { name: "BoolVal".to_string(), payload: vec![Type::Bool] },
        ]),
        predicate: None,
        line: 0,
    });
    // The built-in `Template` record (RFC-0007): the structured form of an
    // interpolated string — literal `parts` interleaved with boxed `values`.
    // `template"a\{x}b"` yields one of these; any code can read its fields.
    program.type_decls.push(TypeDecl {
        name: "Template".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Record(vec![
            Field { name: "parts".to_string(), ty: Type::Array(Box::new(Type::Str)) },
            Field {
                name: "values".to_string(),
                ty: Type::Array(Box::new(Type::Named("Value".to_string()))),
            },
        ]),
        predicate: None,
        line: 0,
    });
    // The error model (RFC-0009): a structured `Issue` (with an i18n `key`) and a
    // generic `Validation<T>` = `Valid(T) | Invalid([Issue])`. A validator
    // accumulates all failing checks into an issue array and returns `Invalid`,
    // so every problem is reported at once and each carries a translation key.
    program.type_decls.push(TypeDecl {
        name: "Issue".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Record(vec![
            Field { name: "key".to_string(), ty: Type::Str },
            Field { name: "path".to_string(), ty: Type::Str },
            Field { name: "message".to_string(), ty: Type::Str },
        ]),
        predicate: None,
        line: 0,
    });
    program.type_decls.push(TypeDecl {
        name: "Validation".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: vec!["T".to_string()],
        base: Type::Enum(vec![
            EnumVariant { name: "Valid".to_string(), payload: vec![Type::Param("T".to_string())] },
            EnumVariant {
                name: "Invalid".to_string(),
                payload: vec![Type::Array(Box::new(Type::Named("Issue".to_string())))],
            },
        ]),
        predicate: None,
        line: 0,
    });
    // `Schema` (RFC-0003 reflection): the extractable shape of a validated type,
    // produced by `schemaOf(TypeName)` — its name, base spelling, `///` doc,
    // and everything its `where` predicate implies (numeric bounds, multipleOf,
    // string length bounds, regex pattern). Turn it into OpenAPI/JSON in
    // ordinary code.
    program.type_decls.push(TypeDecl {
        name: "Schema".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Record(vec![
            Field { name: "name".to_string(), ty: Type::Str },
            Field { name: "base".to_string(), ty: Type::Str },
            Field { name: "doc".to_string(), ty: Type::Option(Box::new(Type::Str)) },
            Field { name: "min".to_string(), ty: Type::Option(Box::new(Type::Int)) },
            Field { name: "max".to_string(), ty: Type::Option(Box::new(Type::Int)) },
            Field { name: "multipleOf".to_string(), ty: Type::Option(Box::new(Type::Int)) },
            Field { name: "minLength".to_string(), ty: Type::Option(Box::new(Type::Int)) },
            Field { name: "maxLength".to_string(), ty: Type::Option(Box::new(Type::Int)) },
            Field { name: "pattern".to_string(), ty: Type::Option(Box::new(Type::Str)) },
        ]),
        predicate: None,
        line: 0,
    });
    // Module reflection (RFC-0021): `moduleInterface(path)` returns the shape of
    // a module's EXPORTED surface — `schemaOf` generalized from one type to a
    // whole module. Injected like `Schema`/`Issue` so a generator can name them
    // without an import, and filtered out of the LSP by their line-0 origin. The
    // records reference each other and `Schema` by name (resolution is
    // order-independent). A generator consumes these to emit stubs/docs/mocks.
    //   ParamInfo { name, spelling, schema }
    //   FnInfo     { name, params: Array<ParamInfo>, ret, retSchema }
    //   TypeInfo   { name, source, schema }
    //   ModuleInterface { functions: Array<FnInfo>, types: Array<TypeInfo> }
    program.type_decls.push(TypeDecl {
        name: "ParamInfo".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Record(vec![
            Field { name: "name".to_string(), ty: Type::Str },
            Field { name: "spelling".to_string(), ty: Type::Str },
            Field { name: "schema".to_string(), ty: Type::Named("Schema".to_string()) },
        ]),
        predicate: None,
        line: 0,
    });
    program.type_decls.push(TypeDecl {
        name: "FnInfo".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Record(vec![
            Field { name: "name".to_string(), ty: Type::Str },
            Field {
                name: "params".to_string(),
                ty: Type::Array(Box::new(Type::Named("ParamInfo".to_string()))),
            },
            Field { name: "ret".to_string(), ty: Type::Str },
            Field { name: "retSchema".to_string(), ty: Type::Named("Schema".to_string()) },
        ]),
        predicate: None,
        line: 0,
    });
    program.type_decls.push(TypeDecl {
        name: "TypeInfo".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Record(vec![
            Field { name: "name".to_string(), ty: Type::Str },
            Field { name: "source".to_string(), ty: Type::Str },
            Field { name: "schema".to_string(), ty: Type::Named("Schema".to_string()) },
        ]),
        predicate: None,
        line: 0,
    });
    program.type_decls.push(TypeDecl {
        name: "ModuleInterface".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Record(vec![
            Field {
                name: "functions".to_string(),
                ty: Type::Array(Box::new(Type::Named("FnInfo".to_string()))),
            },
            Field {
                name: "types".to_string(),
                ty: Type::Array(Box::new(Type::Named("TypeInfo".to_string()))),
            },
        ]),
        predicate: None,
        line: 0,
    });
    // The server surface (RFC-0016): the `Request` handed to `handle` and the
    // `Response` it returns. Ordinary records (no `where`), injected like
    // `Schema`/`Issue` so every program can name them without a `use` and
    // `vyrn serve` can construct/read them across the FFI-free interpreter
    // boundary. `path` carries the query string as sent; `body` is the raw
    // request body (`""` when absent). Users construct them freely — `main`
    // calling `handle` directly is the parity story.
    program.type_decls.push(TypeDecl {
        name: "Request".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Record(vec![
            Field { name: "method".to_string(), ty: Type::Str },
            Field { name: "path".to_string(), ty: Type::Str },
            Field { name: "body".to_string(), ty: Type::Str },
        ]),
        predicate: None,
        line: 0,
    });
    program.type_decls.push(TypeDecl {
        name: "Response".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: Type::Record(vec![
            Field { name: "status".to_string(), ty: Type::Int },
            Field { name: "contentType".to_string(), ty: Type::Str },
            Field { name: "body".to_string(), ty: Type::Str },
        ]),
        predicate: None,
        line: 0,
    });
    // Flatten each `impl P for T` method into a mangled top-level function
    // (`P__Key__method`), so type checking, monomorphization, and lowering treat
    // it like any function; protocol-method *calls* resolve to these names by the
    // receiver's type. Impls on unsupported targets are left for the checker.
    let mut flat = Vec::new();
    for imp in &program.impls {
        if let Some(key) = crate::types::type_key(&imp.ty) {
            for m in &imp.methods {
                let mut f = m.clone();
                f.name = crate::types::impl_method_name(&imp.protocol, &key, &m.name);
                flat.push(f);
            }
        }
    }
    program.functions.extend(flat);
    (program, errors)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// When true, a bare `Ident {` is NOT a struct literal (so `if x { .. }`
    /// parses `x` as the condition and `{` as the block). Reset inside `( .. )`.
    no_struct: bool,
    /// The current function's generic parameters; a type name matching one of
    /// these parses as [`Type::Param`] rather than a named type.
    type_params: Vec<String>,
    /// Inline per-field refinements collected while parsing a record type
    /// inside a `type` declaration: `(field, predicate)` for each
    /// `field: T where pred`. Drained by `type_decl`, which desugars each into
    /// a synthetic validated type named `Decl.field`. `None` outside a type
    /// declaration (inline `where` is then a parse error — an anonymous record
    /// has no name to hang the refinement on).
    field_preds: Option<Vec<(String, Expr)>>,
    /// Extra statements emitted by a single-statement desugar (RFC-0011
    /// addendum: `a[i].f = v` lowers to a `let mut @tmp = a[i]` / `@tmp.f = v` /
    /// `a[i] = @tmp` idiom). `stmt` returns the first of the sequence and stashes
    /// the rest here; `block` drains them right after, preserving order. Only
    /// ever non-empty for the duration of one `stmt` call (its single caller).
    extra_stmts: Vec<Stmt>,
    /// Diagnostics accumulated by *within-body* statement recovery (RFC-0006):
    /// when a statement inside a block fails to parse, [`Parser::block`] records
    /// the error here and syncs to the next statement boundary instead of
    /// aborting the whole declaration. Merged (and sorted by position) into the
    /// program's error list by [`Parser::program_accum`].
    errors: Vec<Diagnostic>,
}

/// Whether `e` is a field-access chain bottoming out in `a[i]` (i.e. `at(a, i)`),
/// e.g. `a[i].f` or `a[i].f.g`. Used to distinguish a too-deep array-element
/// write-through (`a[i].f.g = v`, rejected) from an ordinary nested record-field
/// write (`a.b.c = v`, handled elsewhere).
fn is_index_field_chain(e: &Expr) -> bool {
    match e {
        Expr::Call { name, args, .. } => name == "at" && args.len() == 2,
        Expr::Field { expr, .. } => is_index_field_chain(expr),
        _ => false,
    }
}

impl Parser {
    // ---- token cursor helpers -------------------------------------------

    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn line(&self) -> usize {
        self.tokens[self.pos].line
    }

    /// Consume a statement/declaration terminator. A `;` is optional — statements
    /// are otherwise terminated by the start of the next one (the expression
    /// grammar is greedy, so a boundary falls where an expression can't extend).
    fn eat_semi(&mut self) {
        if *self.peek() == Tok::Semi {
            self.advance();
        }
    }

    /// Consume consecutive `///` doc-comment tokens and return them joined by
    /// newlines (markdown), or `None` if there were none. Used to attach docs to
    /// the following declaration; elsewhere (inside bodies) the result is ignored,
    /// which simply discards stray doc comments.
    fn take_docs(&mut self) -> Option<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut last_line = 0usize;
        loop {
            let line = self.tokens[self.pos].line;
            match self.peek() {
                Tok::Doc(t) => {
                    // A blank line splits `///` blocks: a detached earlier
                    // block (e.g. a file-header comment) belongs to the file,
                    // not the next declaration — discard it.
                    if !lines.is_empty() && line > last_line + 1 {
                        lines.clear();
                    }
                    lines.push(t.clone());
                    last_line = line;
                    self.advance();
                }
                _ => {
                    // The surviving block must sit DIRECTLY above the
                    // declaration; a gap detaches it.
                    if !lines.is_empty() && line > last_line + 1 {
                        lines.clear();
                    }
                    break;
                }
            }
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// 1-based column of the current token (for diagnostics).
    fn col(&self) -> usize {
        self.tokens[self.pos].col
    }

    fn advance(&mut self) -> Tok {
        let t = self.tokens[self.pos].tok.clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, expected: &Tok) -> Result<(), Diagnostic> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else if *expected == Tok::Gt && *self.peek() == Tok::GtEq {
            // `let x: Array<Int>= []` — the lexer max-munches `>=`; when a `>`
            // is expected (closing a generic argument list), split it into the
            // `>` we consume and an `=` left for the caller.
            self.tokens[self.pos].tok = Tok::Eq;
            self.tokens[self.pos].col += 1;
            Ok(())
        } else {
            Err(Diagnostic::error(
                self.line(),
                self.col(),
                "parse",
                format!("expected {:?}, found {:?}", expected, self.peek()),
            ))
        }
    }

    fn expect_ident(&mut self) -> Result<String, Diagnostic> {
        match self.advance() {
            Tok::Ident(name) => Ok(name),
            other => Err(Diagnostic::error(
                self.line(),
                self.col(),
                "parse",
                format!("expected identifier, found {:?}", other),
            )),
        }
    }

    // ---- grammar --------------------------------------------------------

    /// Top-level: zero or more declarations, with **recovery**. When a
    /// declaration fails to parse, record its diagnostic, synchronize to the
    /// next top-level starter, and continue — so one bad `fn` doesn't hide a
    /// later bad `type`. Returns the partial program plus every diagnostic.
    fn program_accum(&mut self) -> (Program, Vec<Diagnostic>) {
        let mut imports = Vec::new();
        let mut type_decls = Vec::new();
        let mut functions = Vec::new();
        let mut protocols = Vec::new();
        let mut impls = Vec::new();
        let mut globals = Vec::new();
        let mut tests = Vec::new();
        let mut log_level = DEFAULT_LOG_LEVEL;
        let mut log_sink = LogSink::Stderr;
        let mut saw_logging = false;
        let mut errors = Vec::new();
        while *self.peek() != Tok::Eof {
            // A failed `fn f<T>` / `type G<T>` clears its generic params only on
            // the success path — reset here so stale params never leak into the
            // NEXT declaration (where a `T` would wrongly parse as Type::Param).
            self.type_params.clear();
            // Collect any leading `///` doc comments and attach to the next decl.
            let doc = self.take_docs();
            if *self.peek() == Tok::Eof {
                break; // trailing docs at end of file
            }
            // `export` marks the FOLLOWING declaration importable (RFC-0010).
            let exported = if *self.peek() == Tok::Export {
                self.advance();
                // `export extern fn ..` (RFC-0012 M2) is also valid — `extern` is a
                // contextual starter, recognized only when `fn` follows it.
                let is_export_extern = matches!(self.peek(), Tok::Ident(n) if n == "extern")
                    && matches!(self.tokens[self.pos + 1].tok, Tok::Fn);
                // `export gen fn ..` (RFC-0021) — `gen` is a contextual starter too,
                // recognized only when `fn` follows it.
                let is_export_gen = matches!(self.peek(), Tok::Ident(n) if n == "gen")
                    && matches!(self.tokens[self.pos + 1].tok, Tok::Fn);
                if !matches!(self.peek(), Tok::Fn | Tok::Type | Tok::Protocol)
                    && !is_export_extern
                    && !is_export_gen
                {
                    errors.push(Diagnostic::error(
                        self.line(), self.col(), "parse",
                        "`export` must be followed by `fn`, `type`, `protocol`, `extern fn`, or \
                         `gen fn`"
                            .to_string(),
                    ));
                    self.sync_to_decl();
                    continue;
                }
                true
            } else {
                false
            };
            match self.peek() {
                Tok::Import => match self.import_decl() {
                    Ok(i) => imports.push(i),
                    Err(d) => { errors.push(d); self.sync_to_decl(); }
                },
                Tok::Type => match self.type_decl() {
                    Ok(mut ds) => {
                        ds[0].doc = doc; // synthetic field types carry no doc
                        ds[0].exported = exported;
                        type_decls.extend(ds);
                    }
                    Err(d) => { errors.push(d); self.sync_to_decl(); }
                },
                Tok::Fn => match self.function(false) {
                    Ok(mut f) => { f.doc = doc; f.exported = exported; functions.push(f); }
                    Err(d) => { errors.push(d); self.sync_to_decl(); }
                },
                // `gen fn ..` — a compile-time module generator (RFC-0021). `gen`
                // is a contextual starter (a plain identifier elsewhere); recognize
                // it only when `fn` follows, so a variable named `gen` is unharmed.
                Tok::Ident(name)
                    if name == "gen" && matches!(self.tokens[self.pos + 1].tok, Tok::Fn) =>
                {
                    self.advance(); // `gen` (a contextual Ident)
                    match self.function(true) {
                        Ok(mut f) => { f.doc = doc; f.exported = exported; functions.push(f); }
                        Err(d) => { errors.push(d); self.sync_to_decl(); }
                    }
                }
                Tok::Protocol => match self.protocol_decl() {
                    Ok(mut p) => { p.doc = doc; p.exported = exported; protocols.push(p); }
                    Err(d) => { errors.push(d); self.sync_to_decl(); }
                },
                Tok::Impl => match self.impl_block() {
                    Ok(i) => impls.push(i),
                    Err(d) => { errors.push(d); self.sync_to_decl(); }
                },
                // Top-level `let [mut] name [: Type] = init` — module state
                // (RFC-0013). Shares the `let` keyword with body-local bindings;
                // recognized here at brace depth 0. `export let` is rejected by
                // the `export` guard above (module state is not importable in v1).
                Tok::Let => match self.global_decl() {
                    Ok(mut g) => { g.doc = doc; globals.push(g); }
                    Err(d) => { errors.push(d); self.sync_to_decl(); }
                },
                // `extern fn ..` — a JS-interop import (RFC-0012). `extern` is a
                // contextual starter (a plain identifier elsewhere); recognize it
                // only when `fn` follows, so a variable named `extern` is unharmed.
                Tok::Ident(name)
                    if name == "extern"
                        && matches!(self.tokens[self.pos + 1].tok, Tok::Fn) =>
                {
                    // `extern fn` (no `export`) is a body-less JS *import* (M1);
                    // `export extern fn ... { body }` is a Vyrn function *exported*
                    // to JS (M2). The `exported` flag decides which shape is legal.
                    match self.extern_function(exported) {
                        Ok(mut f) => { f.doc = doc; functions.push(f); }
                        Err(d) => { errors.push(d); self.sync_to_decl(); }
                    }
                }
                // `test "name" { body }` — a test declaration (RFC-0015).
                // `test` is a contextual starter (a plain identifier elsewhere);
                // recognize it only when a string literal follows, so a variable
                // named `test` is unharmed.
                Tok::Ident(name)
                    if name == "test" && matches!(self.tokens[self.pos + 1].tok, Tok::Str(_)) =>
                {
                    match self.test_decl() {
                        Ok(mut t) => { t.doc = doc; tests.push(t); }
                        Err(d) => { errors.push(d); self.sync_to_decl(); }
                    }
                }
                // `logging { level: <name> }` — the RFC-0008 config block.
                Tok::Ident(name) if name == "logging" => {
                    let line = self.line();
                    let col = self.col();
                    if saw_logging {
                        errors.push(Diagnostic::error(
                            line, col, "parse",
                            "duplicate `logging` config block".to_string(),
                        ));
                        self.sync_to_decl();
                        continue;
                    }
                    saw_logging = true;
                    match self.logging_config() {
                        Ok((lvl, sink)) => { log_level = lvl; log_sink = sink; }
                        Err(d) => { errors.push(d); self.sync_to_decl(); }
                    }
                }
                other => {
                    errors.push(Diagnostic::error(
                        self.line(), self.col(), "parse",
                        format!(
                            "expected `fn`, `type`, `protocol`, `impl`, `let`, or `logging` at \
                             top level, found {other:?}"
                        ),
                    ));
                    self.advance(); // consume the stray token so progress is guaranteed
                }
            }
        }
        // Fold in the within-body statement-recovery diagnostics (collected in
        // `self.errors` while parsing function/test bodies) and present every
        // parse error in source order — top-level and in-body interleaved.
        let mut errors = errors;
        errors.append(&mut self.errors);
        errors.sort_by_key(|d| (d.line, d.col));
        (Program { imports, type_decls, functions, protocols, impls, globals, tests, log_level, log_sink }, errors)
    }

    /// Recovery sync point: advance until the cursor sits on a top-level
    /// declaration starter (`fn`/`type`/`protocol`/`impl`/`logging`) at brace
    /// depth 0, or at `Eof`. Brace depth is tracked so a `fn`/`type` keyword
    /// accidentally appearing inside an unbalanced body doesn't fool us into
    /// resuming mid-declaration. Always makes progress: the caller has already
    /// consumed at least the starter token of the failed declaration, so `pos`
    /// is strictly past the failure point.
    fn sync_to_decl(&mut self) {
        // Skip the token at the failure point first — it is part of the bad
        // declaration (for `fn`/`type`/… the per-decl parser already consumed the
        // starter, so this skips the rest of the bad decl; for the duplicate
        // `logging` case the starter is still unconsumed, so this is what
        // guarantees forward progress).
        self.advance();
        let mut depth = 0i32;
        while *self.peek() != Tok::Eof {
            match self.peek() {
                Tok::LBrace => { depth += 1; self.advance(); }
                Tok::RBrace => { if depth > 0 { depth -= 1; } self.advance(); }
                Tok::Fn | Tok::Type | Tok::Protocol | Tok::Impl | Tok::Import | Tok::Export
                | Tok::Let
                    if depth == 0 =>
                {
                    return
                }
                Tok::Ident(name) if depth == 0 && name == "logging" => return,
                // `extern fn ..` is a top-level starter (RFC-0012) — resume there.
                // `gen fn ..` likewise (RFC-0021).
                Tok::Ident(name)
                    if depth == 0
                        && (name == "extern" || name == "gen")
                        && matches!(self.tokens[self.pos + 1].tok, Tok::Fn) =>
                {
                    return
                }
                // `test "name" { .. }` is a top-level starter (RFC-0015).
                Tok::Ident(name)
                    if depth == 0
                        && name == "test"
                        && matches!(self.tokens[self.pos + 1].tok, Tok::Str(_)) =>
                {
                    return
                }
                _ => { self.advance(); }
            }
        }
    }

    /// `protocol Name { fn m(self, p: T, ..) -> R; .. }` — a set of method
    /// signatures. The `self` receiver is required and elided from the stored
    /// parameter types.
    fn protocol_decl(&mut self) -> Result<ProtocolDecl, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Protocol)?;
        let name = self.expect_ident()?;
        self.eat(&Tok::LBrace)?;
        let mut methods = Vec::new();
        while *self.peek() != Tok::RBrace {
            self.take_docs(); // method-level docs (not retained yet)
            if *self.peek() == Tok::RBrace {
                break;
            }
            let mline = self.line();
            self.eat(&Tok::Fn)?;
            let mname = self.expect_ident()?;
            self.eat(&Tok::LParen)?;
            self.eat(&Tok::Vself)?;
            let mut params = Vec::new();
            while *self.peek() == Tok::Comma {
                self.advance();
                let _pname = self.expect_ident()?;
                self.eat(&Tok::Colon)?;
                params.push(self.type_()?);
            }
            self.eat(&Tok::RParen)?;
            let ret = if *self.peek() == Tok::Arrow {
                self.advance();
                self.type_()?
            } else {
                Type::Unit
            };
            self.eat_semi();
            methods.push(MethodSig { name: mname, params, ret, line: mline });
        }
        self.eat(&Tok::RBrace)?;
        Ok(ProtocolDecl { exported: false, module: None, name, doc: None, methods, line })
    }

    /// `impl P for T { fn m(self, ..) -> R { .. } .. }` — a type's methods for a
    /// protocol. Each method's `self` receiver is typed to `T`.
    fn impl_block(&mut self) -> Result<ImplBlock, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Impl)?;
        let protocol = self.expect_ident()?;
        self.eat(&Tok::For)?;
        let ty = self.type_()?;
        self.eat(&Tok::LBrace)?;
        let mut methods = Vec::new();
        while *self.peek() != Tok::RBrace {
            self.take_docs(); // method-level docs (not retained yet)
            if *self.peek() == Tok::RBrace {
                break;
            }
            methods.push(self.impl_method(&ty)?);
        }
        self.eat(&Tok::RBrace)?;
        Ok(ImplBlock { protocol, ty, methods, line })
    }

    /// One `fn m(self, ..) -> R { .. }` inside an `impl`. Returns a [`Function`]
    /// whose first parameter is `self`, typed to the implementing type.
    fn impl_method(&mut self, self_ty: &Type) -> Result<Function, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Fn)?;
        let name = self.expect_ident()?;
        self.eat(&Tok::LParen)?;
        self.eat(&Tok::Vself)?;
        let mut params = vec![Param {
            name: "self".to_string(),
            capability: Capability::Read,
            ty: self_ty.clone(),
        }];
        while *self.peek() == Tok::Comma {
            self.advance();
            let pname = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            let capability = self.parse_capability();
            let ty = self.type_()?;
            params.push(Param { name: pname, capability, ty });
        }
        self.eat(&Tok::RParen)?;
        let ret = if *self.peek() == Tok::Arrow {
            self.advance();
            self.type_()?
        } else {
            Type::Unit
        };
        let body = self.block()?;
        Ok(Function {
            exported: false,
            module: None,
            name,
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params,
            ret,
            body,
            line,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
        })
    }

    /// `logging { level: <name>, sink: <dest> }` — comma-separated fields, each
    /// optional. `level` is a threshold name; `sink` is `stderr`, `stdout`, or
    /// `file("path")`. Returns `(threshold ordinal, sink)`.
    fn logging_config(&mut self) -> Result<(usize, LogSink), Diagnostic> {
        self.advance(); // `logging`
        self.eat(&Tok::LBrace)?;
        let mut level = DEFAULT_LOG_LEVEL;
        let mut sink = LogSink::Stderr;
        while *self.peek() != Tok::RBrace {
            let line = self.line();
            let col = self.col();
            let key = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            match key.as_str() {
                "level" => {
                    let name = self.expect_ident()?;
                    level = log_level_ordinal(&name).ok_or_else(|| {
                        Diagnostic::error(
                            line,
                            col,
                            "parse",
                            format!("unknown log level `{name}` (trace/debug/info/warn/error)"),
                        )
                    })?;
                }
                "sink" => sink = self.log_sink()?,
                other => {
                    return Err(Diagnostic::error(
                        line,
                        col,
                        "parse",
                        format!("unknown `logging` field `{other}` (expected `level` or `sink`)"),
                    ))
                }
            }
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok((level, sink))
    }

    /// A logging sink: `stderr`, `stdout`, or `file("path")`.
    fn log_sink(&mut self) -> Result<LogSink, Diagnostic> {
        let line = self.line();
        let col = self.col();
        let name = self.expect_ident()?;
        match name.as_str() {
            "stderr" => Ok(LogSink::Stderr),
            "stdout" => Ok(LogSink::Stdout),
            "file" => {
                self.eat(&Tok::LParen)?;
                let path = match self.advance() {
                    Tok::Str(s) => s,
                    other => {
                        return Err(Diagnostic::error(
                            line,
                            col,
                            "parse",
                            format!("`file(..)` sink needs a string path, found {other:?}"),
                        ))
                    }
                };
                self.eat(&Tok::RParen)?;
                Ok(LogSink::File(path))
            }
            other => Err(Diagnostic::error(
                line,
                col,
                "parse",
                format!("unknown sink `{other}` (expected stderr, stdout, or file(\"..\"))"),
            )),
        }
    }

    /// `type Name = Base [where <predicate>] ;` (validated scalar), or
    /// `type Name = { field: Type, ... } ;` (structural record).
    /// `import { a, b } from "path"` — `from` is contextual (a plain
    /// identifier elsewhere). `import type { .. }` is accepted for JSON Schema
    /// imports; the loader dispatches on the path's extension.
    fn import_decl(&mut self) -> Result<ImportDecl, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Import)?;
        // Namespace import `import * as ns from <source>` (RFC-0027): binds ONE
        // name and pulls none of the target's exports into the flat namespace.
        if *self.peek() == Tok::Star {
            self.advance();
            match self.advance() {
                Tok::Ident(kw) if kw == "as" => {}
                other => {
                    return Err(Diagnostic::error(
                        line,
                        self.col(),
                        "parse",
                        format!("expected `as` after `import *`, found {other:?}"),
                    ))
                }
            }
            let ns = self.expect_ident()?;
            match self.advance() {
                Tok::Ident(kw) if kw == "from" => {}
                other => {
                    return Err(Diagnostic::error(
                        line,
                        self.col(),
                        "parse",
                        format!("expected `from` after `import * as {ns}`, found {other:?}"),
                    ))
                }
            }
            let source = self.import_source(line)?;
            self.eat_semi();
            return Ok(ImportDecl { names: Vec::new(), namespace: Some(ns), source, line });
        }
        // Optional `type` marker (JSON Schema imports read naturally).
        if *self.peek() == Tok::Type {
            self.advance();
        }
        self.eat(&Tok::LBrace)?;
        let mut names = Vec::new();
        while *self.peek() != Tok::RBrace {
            let original = self.expect_ident()?;
            // Optional `as alias` (RFC-0022). `as` is contextual: recognized only
            // between an import name and its comma/`}`, so a variable named `as`
            // elsewhere is unharmed.
            let alias = if matches!(self.peek(), Tok::Ident(kw) if kw == "as") {
                self.advance();
                Some(self.expect_ident()?)
            } else {
                None
            };
            names.push(ImportName { original, alias });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        if names.is_empty() {
            return Err(Diagnostic::error(
                line,
                self.col(),
                "parse",
                "an import must name at least one binding: `import { name } from \"..\"`"
                    .to_string(),
            ));
        }
        match self.advance() {
            Tok::Ident(kw) if kw == "from" => {}
            other => {
                return Err(Diagnostic::error(
                    line,
                    self.col(),
                    "parse",
                    format!("expected `from` after the import list, found {other:?}"),
                ))
            }
        }
        let source = self.import_source(line)?;
        self.eat_semi();
        Ok(ImportDecl { names, namespace: None, source, line })
    }

    /// The right-hand side after `from`: a module path string, or a generator
    /// call `gen(args...)` synthesized at compile time (RFC-0021). Shared by
    /// selective/aliased imports and namespace imports (RFC-0027).
    fn import_source(&mut self, line: usize) -> Result<ImportSource, Diagnostic> {
        match self.peek().clone() {
            Tok::Str(p) => {
                self.advance();
                Ok(ImportSource::Path(p))
            }
            Tok::Ident(gen_name) if matches!(self.tokens[self.pos + 1].tok, Tok::LParen) => {
                let call_line = self.line();
                self.advance(); // the generator name
                self.eat(&Tok::LParen)?;
                let mut args = Vec::new();
                while *self.peek() != Tok::RParen {
                    args.push(self.expr()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
                Ok(ImportSource::Generator { name: gen_name, args, line: call_line })
            }
            other => Err(Diagnostic::error(
                line,
                self.col(),
                "parse",
                format!(
                    "expected a module path string or a generator call after `from`, \
                     found {other:?}"
                ),
            )),
        }
    }

    fn type_decl(&mut self) -> Result<Vec<TypeDecl>, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Type)?;
        let name = self.expect_ident()?;

        // optional generic parameters: `type Box<T> = ...`
        let mut type_params = Vec::new();
        if *self.peek() == Tok::Lt {
            self.advance();
            while *self.peek() != Tok::Gt {
                type_params.push(self.expect_ident()?);
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::Gt)?;
        }
        self.type_params = type_params.clone(); // names parse as Type::Param in the body

        self.eat(&Tok::Eq)?;
        // Collect inline field refinements while the base parses (record
        // declarations only — the collector is what makes `where` legal in
        // field position).
        self.field_preds = Some(Vec::new());
        let base = if *self.peek() == Tok::Pipe {
            self.enum_type()?
        } else {
            self.type_()?
        };
        let field_preds = self.field_preds.take().unwrap_or_default();
        let predicate = if *self.peek() == Tok::Where {
            self.advance();
            Some(self.expr()?)
        } else {
            None
        };
        self.eat_semi();
        self.type_params.clear();

        // Desugar each inline refinement into a synthetic validated type named
        // `Decl.field` (the `.` keeps it out of the user namespace — it shows
        // up only in diagnostics: `validation failed for \`User.age\``). The
        // field's type is rewritten to the synthetic name, so the whole
        // automatic-validation pipeline (boundary checks, traps, jsonSchema)
        // applies unchanged.
        let mut base = base;
        let mut decls = Vec::with_capacity(1 + field_preds.len());
        if !field_preds.is_empty() {
            let Type::Record(fields) = &mut base else {
                unreachable!("field predicates only collect inside a record type")
            };
            for (fname, pred) in field_preds {
                let synthetic = format!("{name}.{fname}");
                let field = fields
                    .iter_mut()
                    .find(|f| f.name == fname)
                    .expect("collected predicate names an existing field");
                decls.push(TypeDecl {
                    exported: false,
                    module: None,
                    name: synthetic.clone(),
                    doc: None,
                    type_params: Vec::new(),
                    base: std::mem::replace(&mut field.ty, Type::Named(synthetic)),
                    predicate: Some(pred),
                    line,
                });
            }
        }
        decls.insert(
            0,
            TypeDecl {
                name,
                exported: false,
                module: None,
                doc: None,
                type_params,
                base,
                predicate,
                line,
            },
        );
        Ok(decls)
    }

    /// Parse an optional capability keyword (`read`/`modify`/`consume`/`share`)
    /// before a parameter's type. Contextual, not reserved: a keyword only counts
    /// as a capability when another identifier (the type) follows it.
    fn parse_capability(&mut self) -> Capability {
        if let Tok::Ident(id) = self.peek() {
            let cap = match id.as_str() {
                "read" => Some(Capability::Read),
                "modify" => Some(Capability::Modify),
                "consume" => Some(Capability::Consume),
                "share" => Some(Capability::Share),
                _ => None,
            };
            if let Some(c) = cap {
                if matches!(self.tokens[self.pos + 1].tok, Tok::Ident(_)) {
                    self.advance();
                    return c;
                }
            }
        }
        Capability::Read
    }

    /// `| Variant(Type) | Variant | ...` — a user-defined enum (sum type).
    /// A leading `|` is required (it disambiguates enums from other type forms).
    fn enum_type(&mut self) -> Result<Type, Diagnostic> {
        let mut variants = Vec::new();
        while *self.peek() == Tok::Pipe {
            self.advance();
            let name = self.expect_ident()?;
            let mut payload = Vec::new();
            if *self.peek() == Tok::LParen {
                self.advance();
                while *self.peek() != Tok::RParen {
                    payload.push(self.type_()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
            }
            variants.push(EnumVariant { name, payload });
        }
        Ok(Type::Enum(variants))
    }

    /// `{ field: Type, field: Type, ... }` — a structural record type. Inside a
    /// `type` declaration, a field may carry an inline refinement
    /// (`age: Int64 where value >= 18`) — Zod/ArkType style — which `type_decl`
    /// desugars into a synthetic validated type named `Decl.field`. The
    /// record-level trailing `where` (after `}`) remains the cross-field
    /// invariant; the two compose.
    fn record_type(&mut self) -> Result<Type, Diagnostic> {
        self.eat(&Tok::LBrace)?;
        // Only the OUTERMOST record of a `type` declaration collects inline
        // refinements (take() blinds nested records, whose fields belong to a
        // different — anonymous — type and would otherwise be misattributed).
        let outer = self.field_preds.take();
        let collecting = outer.is_some();
        let mut local: Vec<(String, Expr)> = Vec::new();
        let mut parse = || -> Result<Vec<Field>, Diagnostic> {
            let mut fields = Vec::new();
            while *self.peek() != Tok::RBrace {
                let name = self.expect_ident()?;
                self.eat(&Tok::Colon)?;
                let ty = self.type_()?;
                if *self.peek() == Tok::Where {
                    let line = self.line();
                    let col = self.col();
                    self.advance();
                    let pred = self.expr()?;
                    if !collecting {
                        return Err(Diagnostic::error(
                            line,
                            col,
                            "parse",
                            "an inline field `where` needs a named record type \
                             (`type T = { field: .. where .. }`); an anonymous record \
                             has no name to attach the refinement to"
                                .to_string(),
                        ));
                    }
                    local.push((name.clone(), pred));
                }
                fields.push(Field { name, ty });
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RBrace)?;
            Ok(fields)
        };
        let result = parse();
        // Restore (and extend) the collector even on the error path, so
        // recovery in `type_decl` sees a consistent state.
        if let Some(mut prev) = outer {
            prev.extend(local);
            self.field_preds = Some(prev);
        }
        Ok(Type::Record(result?))
    }

    /// `[gen] fn name<...>(params) -> Ret { body }`. `is_gen` is set by the
    /// caller when a contextual `gen` modifier preceded `fn` (RFC-0021); the
    /// function parses identically otherwise.
    fn function(&mut self, is_gen: bool) -> Result<Function, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Fn)?;
        let name = self.expect_ident()?;

        // optional generic parameters with bounds: `<T: Ord, U>`
        let mut type_params = Vec::new();
        let mut type_bounds: std::collections::HashMap<String, Vec<String>> = Default::default();
        if *self.peek() == Tok::Lt {
            self.advance();
            while *self.peek() != Tok::Gt {
                let tp = self.expect_ident()?;
                if *self.peek() == Tok::Colon {
                    self.advance();
                    let mut bounds = Vec::new();
                    loop {
                        bounds.push(self.expect_ident()?);
                        if *self.peek() == Tok::Plus {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    type_bounds.insert(tp.clone(), bounds);
                }
                type_params.push(tp);
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::Gt)?;
        }
        // these names parse as Type::Param within this function's signature/body
        self.type_params = type_params.clone();

        self.eat(&Tok::LParen)?;

        let mut params = Vec::new();
        while *self.peek() != Tok::RParen {
            let pname = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            let capability = self.parse_capability();
            let ty = self.type_()?;
            params.push(Param { name: pname, capability, ty });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;

        // optional `-> Type`; absence means Unit
        let ret = if *self.peek() == Tok::Arrow {
            self.advance();
            self.type_()?
        } else {
            Type::Unit
        };

        let body = self.block()?;
        self.type_params.clear();
        Ok(Function { name, exported: false, module: None, doc: None, type_params, type_bounds, params, ret, body, line, is_extern: false, is_export_extern: false, is_gen })
    }

    /// `test "name" { body }` — a test declaration (RFC-0015). `test` is a
    /// contextual starter (a plain identifier elsewhere); the caller has already
    /// confirmed the `test` / string-literal lookahead. The body parses like any
    /// function block; `assert`/`assertEq` become legal inside it (enforced by the
    /// checker, which knows it is in a test).
    fn test_decl(&mut self) -> Result<TestDecl, Diagnostic> {
        let line = self.line();
        self.advance(); // `test` (a contextual Ident)
        let name = match self.advance() {
            Tok::Str(s) => s,
            other => {
                return Err(Diagnostic::error(
                    self.line(),
                    self.col(),
                    "parse",
                    format!("expected a test name string, found {other:?}"),
                ))
            }
        };
        // A test body sees no generic parameters (a test is monomorphic).
        self.type_params.clear();
        let body = self.block()?;
        self.type_params.clear();
        Ok(TestDecl { name, body, doc: None, module: None, line })
    }

    /// `extern fn name(params) -> Ret` — a body-less JS-interop declaration
    /// (RFC-0012). `extern` is a contextual starter (a plain identifier
    /// elsewhere); the caller has already confirmed the `extern` / `fn`
    /// lookahead and tells us via `exported` whether an `export` preceded it.
    /// The two body shapes are exactly opposite by direction:
    ///
    /// - `extern fn f(..)` (`exported == false`) is a JS *import* (M1): the wasm
    ///   host supplies the body, so a body here is an error (`export extern fn`
    ///   is how you supply one).
    /// - `export extern fn f(..) { .. }` (`exported == true`) is a Vyrn function
    ///   *exported* to JS (M2): it is a normal function that must HAVE a body — a
    ///   body-less one is an error (that shape is an import, which `export` on an
    ///   import is not the way to write). The parameter list and return arrow
    ///   parse like an ordinary function; the ABI type domain is enforced later
    ///   by the checker.
    fn extern_function(&mut self, exported: bool) -> Result<Function, Diagnostic> {
        let line = self.line();
        self.advance(); // `extern` (a contextual Ident)
        self.eat(&Tok::Fn)?;
        let name = self.expect_ident()?;
        // No generic parameters on an extern (the ABI is monomorphic).
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        while *self.peek() != Tok::RParen {
            let pname = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            let capability = self.parse_capability();
            let ty = self.type_()?;
            params.push(Param { name: pname, capability, ty });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        let ret = if *self.peek() == Tok::Arrow {
            self.advance();
            self.type_()?
        } else {
            Type::Unit
        };
        let has_body = *self.peek() == Tok::LBrace;
        if exported {
            // `export extern fn` — the exported implementation MUST have a body.
            if !has_body {
                return Err(Diagnostic::error(
                    self.line(),
                    self.col(),
                    "parse",
                    "an exported extern needs a body — a body-less `extern fn` is an import"
                        .to_string(),
                ));
            }
            // No generic type params on an extern; the body parses like any fn.
            self.type_params.clear();
            let body = self.block()?;
            self.type_params.clear();
            return Ok(Function {
                name,
                exported: true,
                module: None,
                doc: None,
                type_params: Vec::new(),
                type_bounds: Default::default(),
                params,
                ret,
                body,
                line,
                is_extern: false,
                is_export_extern: true,
                is_gen: false,
            });
        }
        if has_body {
            return Err(Diagnostic::error(
                self.line(),
                self.col(),
                "parse",
                "an `extern fn` has no body".to_string(),
            ));
        }
        self.eat_semi();
        Ok(Function {
            name,
            exported: false,
            module: None,
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params,
            ret,
            body: Block { stmts: Vec::new() },
            line,
            is_extern: true,
            is_export_extern: false,
            is_gen: false,
        })
    }

    /// A type, possibly an intersection `A & B & ...` (sugar for nested
    /// `Merge<A, B>`, left-associative).
    fn type_(&mut self) -> Result<Type, Diagnostic> {
        let mut t = self.type_atom()?;
        while *self.peek() == Tok::Amp {
            self.advance();
            let rhs = self.type_atom()?;
            t = Type::Merge(Box::new(t), Box::new(rhs));
        }
        Ok(t)
    }

    fn type_atom(&mut self) -> Result<Type, Diagnostic> {
        // An anonymous record type, e.g. in `User & { salary: Int }`.
        if *self.peek() == Tok::LBrace {
            return self.record_type();
        }
        // A function-value type (RFC-0023): `fn(T, U) -> R`, or `fn(T)` /
        // `fn()` for a Unit return. Parsed anywhere a type is; the checker
        // restricts it to a top-level parameter position ("function types are
        // parameter-only in v1").
        if *self.peek() == Tok::Fn {
            self.advance();
            self.eat(&Tok::LParen)?;
            let mut params = Vec::new();
            while *self.peek() != Tok::RParen {
                params.push(self.type_()?);
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RParen)?;
            let ret = if *self.peek() == Tok::Arrow {
                self.advance();
                self.type_()?
            } else {
                Type::Unit
            };
            return Ok(Type::Fn(params, Box::new(ret)));
        }
        let name = self.expect_ident()?;
        // Namespace-qualified type `ns.User` / `ns.Box<T>` (RFC-0027): the dotted
        // name rides `Type::Named`/`Type::App`; the loader verifies `ns` is an
        // in-scope namespace and rewrites it to the plain resolved decl name, so
        // the checker/backends never see a dotted type.
        if *self.peek() == Tok::Dot {
            let mut full = name;
            while *self.peek() == Tok::Dot {
                self.advance();
                full = format!("{full}.{}", self.expect_ident()?);
            }
            if *self.peek() == Tok::Lt {
                self.advance();
                let mut args = Vec::new();
                while *self.peek() != Tok::Gt {
                    args.push(self.type_()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::Gt)?;
                return Ok(Type::App(full, args));
            }
            return Ok(Type::Named(full));
        }
        Ok(match name.as_str() {
            // Every numeric type carries its size in its name. `Int64` is the
            // default integer (what an unannotated literal infers to), but it
            // is always written explicitly — there is no unsized `Int`.
            "Int64" => Type::Int,
            // Sized signed integers.
            "Int8" => Type::IntN { bits: 8, signed: true },
            "Int16" => Type::IntN { bits: 16, signed: true },
            "Int32" => Type::IntN { bits: 32, signed: true },
            // Sized unsigned integers.
            "UInt8" => Type::IntN { bits: 8, signed: false },
            "UInt16" => Type::IntN { bits: 16, signed: false },
            "UInt32" => Type::IntN { bits: 32, signed: false },
            "UInt64" => Type::IntN { bits: 64, signed: false },
            // `Float64` is 64-bit IEEE-754; `Float32` is 32-bit.
            "Float64" => Type::Float,
            "Float32" => Type::Float32,
            // The unsized names are removed — point at the sized spellings.
            "Int" => {
                return Err(Diagnostic::error(
                    self.line(),
                    self.col(),
                    "parse",
                    "`Int` has no size; write `Int64` (or `Int8`/`Int16`/`Int32`, \
                     `UInt8`..`UInt64`)"
                        .to_string(),
                ))
            }
            "Float" => {
                return Err(Diagnostic::error(
                    self.line(),
                    self.col(),
                    "parse",
                    "`Float` has no size; write `Float64` (or `Float32`)".to_string(),
                ))
            }
            "Bool" => Type::Bool,
            "String" => Type::Str,
            "Unit" => Type::Unit,
            // A logger handle (RFC-0008), e.g. `fn f(l: Logger)`.
            "Logger" => Type::Logger,
            // A generational reference to a heap cell (RFC-0004 §4, Path B).
            "Ref" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Ref(Box::new(inner))
            }
            // A concurrent task's result handle.
            "Task" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Task(Box::new(inner))
            }
            // `Array<T>` (growable) or `Array<T, N>` (fixed-size const generic).
            "Array" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                if *self.peek() == Tok::Comma {
                    self.advance();
                    let n = match self.peek() {
                        Tok::Int(n) if *n >= 0 => *n as usize,
                        _ => {
                            return Err(Diagnostic::error(
                                self.line(),
                                self.col(),
                                "parse",
                                "`Array<T, N>` needs a non-negative integer size".to_string(),
                            ))
                        }
                    };
                    self.advance();
                    self.eat(&Tok::Gt)?;
                    Type::ArrayN(Box::new(inner), n)
                } else {
                    self.eat(&Tok::Gt)?;
                    Type::Array(Box::new(inner))
                }
            }
            "Option" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Option(Box::new(inner))
            }
            "Result" => {
                self.eat(&Tok::Lt)?;
                let ok = self.type_()?;
                self.eat(&Tok::Comma)?;
                let err = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Result(Box::new(ok), Box::new(err))
            }
            // Compile-time transformers (RFC-0002 §7).
            "Omit" | "Pick" => {
                self.eat(&Tok::Lt)?;
                let base = self.type_()?;
                let mut keys = Vec::new();
                while *self.peek() == Tok::Comma {
                    self.advance();
                    keys.push(self.expect_ident()?);
                }
                self.eat(&Tok::Gt)?;
                if keys.is_empty() {
                    return Err(Diagnostic::error(
                        self.line(),
                        self.col(),
                        "parse",
                        format!("`{name}` needs at least one field, e.g. `{name}<T, field>`"),
                    ));
                }
                if name == "Omit" {
                    Type::Omit(Box::new(base), keys)
                } else {
                    Type::Pick(Box::new(base), keys)
                }
            }
            "Merge" => {
                self.eat(&Tok::Lt)?;
                let a = self.type_()?;
                self.eat(&Tok::Comma)?;
                let b = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Merge(Box::new(a), Box::new(b))
            }
            "Partial" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                self.eat(&Tok::Gt)?;
                Type::Partial(Box::new(inner))
            }
            // Readonly<T> is the identity here (records are already immutable).
            "Readonly" => {
                self.eat(&Tok::Lt)?;
                let inner = self.type_()?;
                self.eat(&Tok::Gt)?;
                inner
            }
            // A generic parameter of the current function/type.
            other if self.type_params.iter().any(|p| p == other) => Type::Param(other.to_string()),
            // `Name<T, ...>` — an application of a generic named type.
            other if *self.peek() == Tok::Lt => {
                self.advance();
                let mut args = Vec::new();
                while *self.peek() != Tok::Gt {
                    args.push(self.type_()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::Gt)?;
                Type::App(other.to_string(), args)
            }
            // Any other identifier is a named type; the checker verifies it exists.
            other => Type::Named(other.to_string()),
        })
    }

    fn block(&mut self) -> Result<Block, Diagnostic> {
        self.eat(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            self.take_docs(); // discard any stray `///` inside a body
            // Semicolons are optional separators: a stray one (e.g. after an
            // `if { .. };`) is skipped, never a parse error.
            if *self.peek() == Tok::Semi {
                self.advance();
                continue;
            }
            if *self.peek() == Tok::RBrace || *self.peek() == Tok::Eof {
                break;
            }
            // Statement-level recovery (RFC-0006): a bad statement is recorded
            // and dropped, then we synchronize to the next statement boundary
            // and keep parsing this body — so one typo mid-function no longer
            // blanks out the whole declaration's symbols/hover, and several bad
            // statements each get their own diagnostic. Expression-internal
            // errors are unaffected (they surface as the single statement error
            // here). A structural failure (a missing brace) still propagates.
            let start = self.pos;
            match self.stmt() {
                Ok(s) => {
                    stmts.push(s);
                    // A statement desugar (e.g. `a[i].f = v`) leaves its
                    // follow-on statements here; splice them in order right
                    // after the primary one.
                    stmts.append(&mut self.extra_stmts);
                }
                Err(d) => {
                    self.errors.push(d);
                    self.extra_stmts.clear(); // drop any partial desugar
                    // Guarantee forward progress: a statement parser that failed
                    // without consuming anything (e.g. a bad leading token) would
                    // otherwise re-error here forever. One that already advanced
                    // (the common case — it consumed a `let`/name/`=` before
                    // failing) needs no nudge; skipping a token could eat the
                    // block's `}`.
                    if self.pos == start {
                        self.advance();
                    }
                    self.sync_to_stmt();
                }
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok(Block { stmts })
    }

    /// Recovery sync point for a failed *statement* inside a block. Advances
    /// until the cursor sits at the next statement boundary: a token that starts
    /// a new source line at THIS block's brace depth, a `;` separator at that
    /// depth, the block's own closing `}`, or `Eof`. Brace depth is tracked so a
    /// `{ .. }` inside the bad statement (a struct literal, a nested block) does
    /// not fool the "same depth" test. Consumes nothing when already at a
    /// boundary — [`Parser::block`] has already guaranteed progress. The bad
    /// statement's remaining tokens are discarded.
    fn sync_to_stmt(&mut self) {
        let mut depth = 0i32;
        while *self.peek() != Tok::Eof {
            match self.peek() {
                Tok::LBrace => {
                    depth += 1;
                    self.advance();
                }
                Tok::RBrace => {
                    if depth == 0 {
                        return; // the block's closing brace ends this body
                    }
                    depth -= 1;
                    self.advance();
                }
                Tok::Semi if depth == 0 => {
                    self.advance(); // a separator: the next statement follows
                    return;
                }
                _ if depth == 0 && self.line() > self.tokens[self.pos - 1].line => {
                    return; // a token on a fresh line begins the next statement
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// A top-level module-state binding (RFC-0013):
    /// `let [mut] name [: Type] = initializer`. The initializer is REQUIRED —
    /// a bare `let x` / `let x: T` (no `=`) is a parse error (module state has
    /// no default value; `before main` runs every initializer once).
    fn global_decl(&mut self) -> Result<GlobalDecl, Diagnostic> {
        let line = self.line();
        self.eat(&Tok::Let)?;
        let mutable = if *self.peek() == Tok::Mut {
            self.advance();
            true
        } else {
            false
        };
        let name = self.expect_ident()?;
        let ty = if *self.peek() == Tok::Colon {
            self.advance();
            Some(self.type_()?)
        } else {
            None
        };
        if *self.peek() != Tok::Eq {
            return Err(Diagnostic::error(
                self.line(),
                self.col(),
                "parse",
                format!(
                    "module state `{name}` needs an initializer: write `let {name} = <value>` \
                     (top-level `let` has no default value)"
                ),
            ));
        }
        self.eat(&Tok::Eq)?;
        let init = self.expr()?;
        self.eat_semi();
        Ok(GlobalDecl { name, mutable, ty, init, doc: None, module: None, line })
    }

    /// Parse an `if` statement whose `if` token is current (`line` is its
    /// position). After the then-block an `else` may be followed either by a
    /// block (`else { .. }`) or — directly — by another `if`, giving an
    /// `else if` chain (RFC-0022). The chained `if` is parsed recursively and
    /// wrapped as the sole statement of a synthesized else-block, so it is the
    /// ordinary nested form to every backend and carries its own honest line
    /// number (diagnostics point at the real `else if`, not the outer `if`).
    fn if_stmt(&mut self, line: usize) -> Result<Stmt, Diagnostic> {
        self.advance(); // `if`
        let cond = self.cond_expr()?;
        let then_block = self.block()?;
        let else_block = if *self.peek() == Tok::Else {
            self.advance();
            if *self.peek() == Tok::If {
                let else_line = self.line();
                let nested = self.if_stmt(else_line)?;
                Some(Block { stmts: vec![nested] })
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        Ok(Stmt::If { cond, then_block, else_block, line })
    }

    fn stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let line = self.line();
        match self.peek() {
            Tok::Let => {
                self.advance();
                let mutable = if *self.peek() == Tok::Mut {
                    self.advance();
                    true
                } else {
                    false
                };
                let name = self.expect_ident()?;
                let ty = if *self.peek() == Tok::Colon {
                    self.advance();
                    Some(self.type_()?)
                } else {
                    None
                };
                self.eat(&Tok::Eq)?;
                let value = self.expr()?;
                self.eat_semi();
                Ok(Stmt::Let { name, mutable, ty, value, line })
            }
            Tok::Return => {
                self.advance();
                // A value follows unless we're at a terminator: `;`, block end
                // `}`, or EOF (a bare `return` for a Unit function).
                let value = if matches!(self.peek(), Tok::Semi | Tok::RBrace | Tok::Eof) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.eat_semi();
                Ok(Stmt::Return { value, line })
            }
            Tok::If => self.if_stmt(line),
            Tok::While => {
                self.advance();
                let cond = self.cond_expr()?;
                let body = self.block()?;
                Ok(Stmt::While { cond, body, line })
            }
            Tok::For => {
                self.advance();
                let var = self.expect_ident()?;
                self.eat(&Tok::In)?;
                // Parse the iterable in the no-struct context (like a `while`
                // condition) so a bare `{` opens the loop body, not a struct lit.
                let iter = self.cond_expr()?;
                let body = self.block()?;
                Ok(Stmt::ForIn { var, iter, body, line })
            }
            Tok::Region => {
                self.advance();
                let body = self.block()?;
                Ok(Stmt::Region { body, line })
            }
            // `drop name;` — reclaim a heap value explicitly (the rare handoff
            // case; most reclamation is inferred). It consumes `name`.
            Tok::Drop => {
                self.advance();
                let name = self.expect_ident()?;
                self.eat_semi();
                Ok(Stmt::Drop { name, line })
            }
            // assignment `name = expr;` or a bare expression statement
            Tok::Ident(_) if self.tokens[self.pos + 1].tok == Tok::Eq => {
                let name = self.expect_ident()?;
                self.eat(&Tok::Eq)?;
                let value = self.expr()?;
                self.eat_semi();
                Ok(Stmt::Assign { name, value, line })
            }
            // field mutation `name.field = expr;`
            Tok::Ident(_)
                if self.tokens[self.pos + 1].tok == Tok::Dot
                    && matches!(self.tokens[self.pos + 2].tok, Tok::Ident(_))
                    && self.tokens[self.pos + 3].tok == Tok::Eq =>
            {
                let name = self.expect_ident()?;
                self.eat(&Tok::Dot)?;
                let field = self.expect_ident()?;
                self.eat(&Tok::Eq)?;
                let value = self.expr()?;
                self.eat_semi();
                Ok(Stmt::SetField { name, field, value, line })
            }
            _ => {
                let e = self.expr()?;
                // `a[i] = v` — element store into a `mut` array binding
                // (RFC-0011). `a[i]` parsed as `at(a, i)` in `postfix`; a
                // trailing `=` turns that read into an in-place store. The array
                // must be a plain identifier binding (v1 restriction).
                if *self.peek() == Tok::Eq {
                    if let Expr::Call { name, args, .. } = &e {
                        if name == "at" && args.len() == 2 {
                            if let Expr::Var { name: recv, .. } = &args[0] {
                                let recv = recv.clone();
                                let index = args[1].clone();
                                self.advance(); // eat `=`
                                let value = self.expr()?;
                                self.eat_semi();
                                return Ok(Stmt::IndexSet { name: recv, index, value, line });
                            }
                            return Err(Diagnostic::error(
                                line,
                                self.col(),
                                "parse",
                                "the left side of an index assignment `[i] = ..` must be \
                                 a plain array variable"
                                    .to_string(),
                            ));
                        }
                    }
                    // `a[i].f = v` — write-through to a record field of an array
                    // element (RFC-0011 addendum). `a[i].f` parsed as
                    // `Field { at(a, i), f }`; a trailing `=` makes it a
                    // copy-modify-store: load element `i`, set field `f` on the
                    // copy, store it back into slot `i`. Desugars to the exact
                    // idiom `let mut @tmp = a[i]  @tmp.f = v  a[i] = @tmp`, so it
                    // inherits SetField's field/validated-data rules and
                    // IndexSet's bounds-check + coercion unchanged, in all three
                    // backends.
                    if let Expr::Field { expr, field, .. } = &e {
                        if let Expr::Call { name, args, .. } = expr.as_ref() {
                            if name == "at" && args.len() == 2 {
                                if let Expr::Var { name: recv, .. } = &args[0] {
                                    let recv = recv.clone();
                                    let load = (**expr).clone();
                                    let field = field.clone();
                                    self.advance(); // eat `=`
                                    let value = self.expr()?;
                                    self.eat_semi();
                                    // The element copy's binding name. Unspellable
                                    // (contains `[`), so it can't collide with a
                                    // real identifier and is filtered from the
                                    // symbol/completion index; but it reads
                                    // naturally if it surfaces in a SetField
                                    // diagnostic ("record `ps[]` has no field ..").
                                    let tmp = format!("{recv}[]");
                                    // `@tmp.f = v` then `a[i] = @tmp` follow the
                                    // returned `let mut @tmp = a[i]`.
                                    self.extra_stmts.push(Stmt::SetField {
                                        name: tmp.clone(),
                                        field,
                                        value,
                                        line,
                                    });
                                    self.extra_stmts.push(Stmt::IndexSet {
                                        name: recv,
                                        index: args[1].clone(),
                                        value: Expr::Var { name: tmp.clone(), line },
                                        line,
                                    });
                                    return Ok(Stmt::Let {
                                        name: tmp,
                                        mutable: true,
                                        ty: None,
                                        value: load,
                                        line,
                                    });
                                }
                                return Err(Diagnostic::error(
                                    line,
                                    self.col(),
                                    "parse",
                                    "the left side of `[i].field = ..` must be a plain \
                                     array variable"
                                        .to_string(),
                                ));
                            }
                        }
                        // `a[i].f.g = v` (and deeper) is rejected: v1 supports one
                        // level of field write-through only.
                        if is_index_field_chain(expr) {
                            return Err(Diagnostic::error(
                                line,
                                self.col(),
                                "parse",
                                "only a single field write-through is supported: \
                                 `a[i].field = v` (not `a[i].field.field = v`)"
                                    .to_string(),
                            ));
                        }
                    }
                }
                self.eat_semi();
                // A mutating method used as a statement writes back through its
                // receiver variable: `sq.push(x);` desugars to `sq = push(sq, x);`
                // (parsed as `push(sq, x)` above), so the reallocated array sticks.
                if let Expr::Call { name, args, .. } = &e {
                    if name == "push" {
                        if let Some(Expr::Var { name: recv, .. }) = args.first() {
                            return Ok(Stmt::Assign { name: recv.clone(), value: e, line });
                        }
                    }
                }
                Ok(Stmt::Expr(e))
            }
        }
    }

    // ---- expressions (precedence climbing) ------------------------------

    fn expr(&mut self) -> Result<Expr, Diagnostic> {
        self.binary(0)
    }

    /// Parse an expression in a position immediately followed by a `{ .. }`
    /// block (an `if`/`while` condition or a `match` scrutinee), where a bare
    /// `Name {` must be read as "value, then block", not a struct literal.
    fn cond_expr(&mut self) -> Result<Expr, Diagnostic> {
        let saved = self.no_struct;
        self.no_struct = true;
        let e = self.expr();
        self.no_struct = saved;
        e
    }

    /// Binding powers: higher binds tighter.
    fn binop(tok: &Tok) -> Option<(BinOp, u8)> {
        Some(match tok {
            Tok::OrOr => (BinOp::Or, 1),
            Tok::AndAnd => (BinOp::And, 2),
            Tok::EqEq => (BinOp::Eq, 3),
            Tok::NotEq => (BinOp::NotEq, 3),
            Tok::TildeMatch => (BinOp::Match, 3),
            Tok::Lt => (BinOp::Lt, 4),
            Tok::LtEq => (BinOp::LtEq, 4),
            Tok::Gt => (BinOp::Gt, 4),
            Tok::GtEq => (BinOp::GtEq, 4),
            Tok::Plus => (BinOp::Add, 5),
            Tok::Minus => (BinOp::Sub, 5),
            Tok::Star => (BinOp::Mul, 6),
            Tok::Slash => (BinOp::Div, 6),
            Tok::Percent => (BinOp::Rem, 6),
            _ => return None,
        })
    }

    fn binary(&mut self, min_bp: u8) -> Result<Expr, Diagnostic> {
        let mut lhs = self.unary()?;
        while let Some((op, bp)) = Self::binop(self.peek()) {
            if bp < min_bp {
                break;
            }
            let line = self.line();
            self.advance();
            // left-associative: parse rhs with strictly higher binding power
            let rhs = self.binary(bp + 1)?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), line };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, Diagnostic> {
        let line = self.line();
        match self.peek() {
            Tok::Minus => {
                self.advance();
                Ok(Expr::Unary { op: UnOp::Neg, expr: Box::new(self.unary()?), line })
            }
            Tok::Bang => {
                self.advance();
                Ok(Expr::Unary { op: UnOp::Not, expr: Box::new(self.unary()?), line })
            }
            _ => self.postfix(),
        }
    }

    /// Postfix operators `?` and `.field`, binding tighter than unary/binary.
    fn postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut e = self.primary()?;
        loop {
            let line = self.line();
            match self.peek() {
                Tok::Question => {
                    self.advance();
                    e = Expr::Try { expr: Box::new(e), line };
                }
                Tok::Dot => {
                    self.advance();
                    let name = self.expect_ident()?;
                    if *self.peek() == Tok::LParen {
                        // Method call `recv.name(args)` is sugar for the free call
                        // `name(recv, args)` — the receiver becomes the first arg.
                        self.advance();
                        let saved = self.no_struct;
                        self.no_struct = false;
                        let mut args = vec![e];
                        while *self.peek() != Tok::RParen {
                            args.push(self.expr()?);
                            if *self.peek() == Tok::Comma {
                                self.advance();
                            } else {
                                break;
                            }
                        }
                        self.no_struct = saved;
                        self.eat(&Tok::RParen)?;
                        // Method-only builtins map to their internal spellings:
                        // `x.toString()` renders via the `@str` machinery and
                        // `t.join()` awaits via `@join`. The bare free-function
                        // forms (`toString(x)`, `join(t)`) never reach this arm,
                        // so the checker reports them with a migration hint.
                        let name = match name.as_str() {
                            "toString" => "@str".to_string(),
                            "join" => "@join".to_string(),
                            // In-place array mutation (RFC-0011): method-only, so
                            // they map to unspellable internal names — a free
                            // `pop(a)` / `swapRemove(a, i)` never reaches here and
                            // the checker reports it as an unknown call.
                            "pop" => "@pop".to_string(),
                            "swapRemove" => "@swapRemove".to_string(),
                            _ => name,
                        };
                        e = Expr::Call { name, args, line };
                    } else if *self.peek() == Tok::LBrace
                        && !self.no_struct
                        && matches!(&e, Expr::Var { .. })
                    {
                        // Namespace-qualified record construction `ns.Type { .. }`
                        // (RFC-0027). Plain member access cannot represent it (a
                        // `Field` followed by `{` is otherwise a parse error), so
                        // the head must be a bare identifier — a namespace. The
                        // qualifier rides the struct name as `"ns.Type"`; the
                        // loader splits on the dot, verifies `ns` is an in-scope
                        // namespace, and rewrites it to the plain resolved decl,
                        // so the checker/backends never see a dotted name.
                        let Expr::Var { name: ns, .. } = e else { unreachable!() };
                        e = self.struct_lit(format!("{ns}.{name}"), line)?;
                    } else {
                        // Property / field access `recv.name` (e.g. `arr.length`).
                        e = Expr::Field { expr: Box::new(e), field: name, line };
                    }
                }
                Tok::LBracket => {
                    // Index `recv[i]` is sugar for the bounds-checked `at(recv, i)`.
                    self.advance();
                    let saved = self.no_struct;
                    self.no_struct = false;
                    let idx = self.expr()?;
                    self.no_struct = saved;
                    self.eat(&Tok::RBracket)?;
                    e = Expr::Call { name: "at".to_string(), args: vec![e, idx], line };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    /// Parse a lambda literal (RFC-0023): `|x| expr`, `|x, y| { block }`, or the
    /// zero-parameter form `|| expr` (whose empty pipe pair is the single `||`
    /// token). The parameters are untyped names; their types flow from the
    /// expected `fn(..)` type at the checker. A block body uses `return` like a
    /// function; an expression body is the returned value directly.
    fn lambda(&mut self, line: usize) -> Result<Expr, Diagnostic> {
        let mut params = Vec::new();
        if *self.peek() == Tok::OrOr {
            // `||` — an empty parameter list.
            self.advance();
        } else {
            self.eat(&Tok::Pipe)?;
            while *self.peek() != Tok::Pipe {
                params.push(self.expect_ident()?);
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&Tok::Pipe)?;
        }
        let body = if *self.peek() == Tok::LBrace {
            LambdaBody::Block(self.block()?)
        } else {
            // A struct literal is legal again inside a lambda's expression body
            // (the `no_struct` guard is for `if`/`while`/`match` heads only).
            let saved = self.no_struct;
            self.no_struct = false;
            let e = self.expr();
            self.no_struct = saved;
            LambdaBody::Expr(Box::new(e?))
        };
        Ok(Expr::Lambda { params, body, line })
    }

    fn primary(&mut self) -> Result<Expr, Diagnostic> {
        let line = self.line();
        let col = self.col();
        // A lambda literal (RFC-0023). A bare `|` in expression position opens
        // one (there is no bitwise-or operator, so `|` never starts an ordinary
        // expression); an `||` token is the zero-parameter form `|| expr`.
        if matches!(self.peek(), Tok::Pipe | Tok::OrOr) {
            return self.lambda(line);
        }
        match self.advance() {
            Tok::Int(v) => Ok(Expr::Int(v)),
            Tok::Float(v) => Ok(Expr::Float(v)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            // `self` — the receiver inside an `impl` method; an ordinary binding.
            Tok::Vself => Ok(Expr::Var { name: "self".to_string(), line }),
            Tok::TemplateStr { parts, exprs } => self.template(parts, exprs, line, col),
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            Tok::LParen => {
                // Struct literals are allowed again inside parentheses.
                let saved = self.no_struct;
                self.no_struct = false;
                let e = self.expr()?;
                self.no_struct = saved;
                self.eat(&Tok::RParen)?;
                Ok(e)
            }
            // A fixed-size array literal `[a, b, c]`.
            Tok::LBracket => {
                let saved = self.no_struct;
                self.no_struct = false;
                let mut elems = Vec::new();
                while *self.peek() != Tok::RBracket {
                    elems.push(self.expr()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.no_struct = saved;
                self.eat(&Tok::RBracket)?;
                // An empty `[]` is a growable/fixed empty array; its element type
                // comes from the expected type (like `None`). A non-empty literal
                // is a fixed-size `Array<T, N>`.
                Ok(Expr::ArrayLit { elems, line })
            }
            Tok::Match => self.match_expr(line),
            // `spawn f(args)` — a concurrent task over a pure function.
            Tok::Spawn => {
                let name = self.expect_ident()?;
                self.eat(&Tok::LParen)?;
                let saved = self.no_struct;
                self.no_struct = false;
                let mut args = Vec::new();
                while *self.peek() != Tok::RParen {
                    args.push(self.expr()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.no_struct = saved;
                self.eat(&Tok::RParen)?;
                Ok(Expr::Spawn { name, args, line })
            }
            Tok::Ident(name) => {
                // Tagged template `tag"...\{e}..."` (RFC-0007): an identifier
                // directly followed — on the same line — by an interpolated
                // string literal. The same-line requirement keeps a statement
                // ending in a variable from swallowing the next statement's
                // string literal in semicolon-free code.
                let string_adjacent = self.tokens[self.pos].line == line;
                if string_adjacent && matches!(self.peek(), Tok::TemplateStr { .. }) {
                    if let Tok::TemplateStr { parts, exprs } = self.advance() {
                        return self.tagged_template(name, parts, exprs, line, col);
                    }
                }
                if string_adjacent && matches!(self.peek(), Tok::Str(_)) {
                    return Err(Diagnostic::error(
                        line,
                        col,
                        "parse",
                        format!(
                            "a tagged template `{name}\"..\"` needs at least one `\\{{ }}` \
                             interpolation; use a plain string otherwise"
                        ),
                    ));
                }
                // Fallible construction: `Name?(args)`.
                let fallible = *self.peek() == Tok::Question
                    && self.tokens[self.pos + 1].tok == Tok::LParen;
                if fallible {
                    self.advance(); // consume `?`
                }
                if *self.peek() == Tok::LParen {
                    // call / construction
                    self.advance();
                    let saved = self.no_struct;
                    self.no_struct = false;
                    let mut args = Vec::new();
                    while *self.peek() != Tok::RParen {
                        args.push(self.expr()?);
                        if *self.peek() == Tok::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    self.no_struct = saved;
                    self.eat(&Tok::RParen)?;
                    Ok(if fallible {
                        Expr::TryConstruct { name, args, line }
                    } else {
                        Expr::Call { name, args, line }
                    })
                } else if *self.peek() == Tok::LBrace && !self.no_struct {
                    // struct literal: `Name { field: expr, ... }`
                    self.struct_lit(name, line)
                } else {
                    Ok(Expr::Var { name, line })
                }
            }
            other => Err(Diagnostic::error(
                line,
                col,
                "parse",
                format!("unexpected token in expression: {other:?}"),
            )),
        }
    }

    /// Desugar an interpolated string `"a\{e}b"` into a `concat`/`str` chain
    /// producing a `String` — the default (untagged) template of RFC-0007. Each
    /// hole's raw source is re-lexed and parsed as an expression, then rendered
    /// with `str` (which handles Int/Bool/String). `parts.len() == exprs.len()+1`.
    fn template(
        &mut self,
        parts: Vec<String>,
        exprs: Vec<String>,
        line: usize,
        col: usize,
    ) -> Result<Expr, Diagnostic> {
        let mut pieces: Vec<Expr> = Vec::new();
        if !parts[0].is_empty() {
            pieces.push(Expr::Str(parts[0].clone()));
        }
        for (k, src) in exprs.iter().enumerate() {
            let e = self.parse_hole(src, line, col)?;
            // `@str` / `@concat` are the *internal* spellings of the removed
            // `str`/`concat` builtins: the parser produces them for desugaring,
            // but the lexer can never produce a leading `@`, so user source
            // hitting the bare `str`/`concat` names gets the migration hint.
            pieces.push(Expr::Call { name: "@str".to_string(), args: vec![e], line });
            if !parts[k + 1].is_empty() {
                pieces.push(Expr::Str(parts[k + 1].clone()));
            }
        }
        // There is always at least one hole here, so `pieces` is non-empty. Fold
        // left with `@concat`; a lone piece is already a `String`.
        let mut iter = pieces.into_iter();
        let mut acc = iter.next().unwrap();
        for p in iter {
            acc = Expr::Call { name: "@concat".to_string(), args: vec![acc, p], line };
        }
        Ok(acc)
    }

    /// Re-lex and parse one interpolation hole's raw source as an expression,
    /// sharing the enclosing function's generic parameters.
    fn parse_hole(&self, src: &str, line: usize, col: usize) -> Result<Expr, Diagnostic> {
        let toks = crate::lexer::lex(src).map_err(|e| {
            Diagnostic::error(line, col, "parse", format!("in interpolation: {}", e.render()))
        })?;
        let mut sub = Parser {
            tokens: toks,
            pos: 0,
            no_struct: false,
            type_params: self.type_params.clone(),
            field_preds: None,
            extra_stmts: Vec::new(),
            errors: Vec::new(),
        };
        // A sub-parser diagnostic carries line numbers relative to the hole
        // snippet — anchor it at the template and embed the detail, exactly
        // like the lex-error wrapping above.
        let e = sub.expr().map_err(|d| {
            Diagnostic::error(line, col, "parse", format!("in interpolation: {}", d.message))
        })?;
        if *sub.peek() != Tok::Eof {
            return Err(Diagnostic::error(
                line,
                col,
                "parse",
                format!("unexpected tokens after interpolation expression `{}`", src.trim()),
            ));
        }
        Ok(e)
    }

    /// Desugar a **tagged** template `tag"a\{e}b"` into a call
    /// `tag(list([parts..]), list([value(e)..]))` — RFC-0007. The literal parts
    /// and the interpolated values reach the tag as separate arrays; the values
    /// are boxed into the built-in `Value` enum. Requires ≥1 interpolation.
    fn tagged_template(
        &self,
        tag: String,
        parts: Vec<String>,
        exprs: Vec<String>,
        line: usize,
        col: usize,
    ) -> Result<Expr, Diagnostic> {
        let parts_lit = Expr::ArrayLit {
            elems: parts.into_iter().map(Expr::Str).collect(),
            line,
        };
        let mut values = Vec::new();
        for src in &exprs {
            let e = self.parse_hole(src, line, col)?;
            values.push(Expr::Call { name: "value".to_string(), args: vec![e], line });
        }
        let values_lit = Expr::ArrayLit { elems: values, line };
        // `@list` is the internal spelling of the removed `list` builtin (see
        // `@str`/`@concat` above): produced only by desugaring, never lexable.
        let wrap = |e| Expr::Call { name: "@list".to_string(), args: vec![e], line };
        // The built-in `template` tag yields the first-class `Template` record;
        // any other tag is an ordinary function call `tag(parts, values)`.
        if tag == "template" {
            return Ok(Expr::StructLit {
                name: "Template".to_string(),
                fields: vec![
                    ("parts".to_string(), wrap(parts_lit)),
                    ("values".to_string(), wrap(values_lit)),
                ],
                line,
            });
        }
        Ok(Expr::Call { name: tag, args: vec![wrap(parts_lit), wrap(values_lit)], line })
    }

    /// `match scrutinee { pattern => expr, ... }` (the `match` keyword is already
    /// consumed). Arms are single expressions in v0.1.
    fn match_expr(&mut self, line: usize) -> Result<Expr, Diagnostic> {
        let scrutinee = self.cond_expr()?;
        self.eat(&Tok::LBrace)?;
        let mut arms = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            let pattern = self.pattern()?;
            self.eat(&Tok::FatArrow)?;
            let body = self.expr()?;
            arms.push(MatchArm { pattern, body });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok(Expr::Match { scrutinee: Box::new(scrutinee), arms, line })
    }

    /// `Name { field: expr, ... }` — a record literal (the name is consumed).
    fn struct_lit(&mut self, name: String, line: usize) -> Result<Expr, Diagnostic> {
        self.eat(&Tok::LBrace)?;
        // Field values may themselves be struct literals.
        let saved = self.no_struct;
        self.no_struct = false;
        let mut fields = Vec::new();
        while *self.peek() != Tok::RBrace {
            let fname = self.expect_ident()?;
            self.eat(&Tok::Colon)?;
            let value = self.expr()?;
            fields.push((fname, value));
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.no_struct = saved;
        self.eat(&Tok::RBrace)?;
        Ok(Expr::StructLit { name, fields, line })
    }

    /// Parse the `(name)` that binds a pattern's payload.
    fn pattern_binding(&mut self) -> Result<String, Diagnostic> {
        self.eat(&Tok::LParen)?;
        let bind = self.expect_ident()?;
        self.eat(&Tok::RParen)?;
        Ok(bind)
    }

    fn pattern(&mut self) -> Result<Pattern, Diagnostic> {
        let line = self.line();
        let mut name = self.expect_ident()?;
        // Namespace-qualified variant pattern `ns.Color.Red` (RFC-0027): the
        // dotted path rides `Pattern::Variant`'s name; the loader verifies `ns`
        // is an in-scope namespace and reduces it to the bare variant name.
        if *self.peek() == Tok::Dot {
            while *self.peek() == Tok::Dot {
                self.advance();
                name = format!("{name}.{}", self.expect_ident()?);
            }
            let mut binds = Vec::new();
            if *self.peek() == Tok::LParen {
                self.advance();
                while *self.peek() != Tok::RParen {
                    binds.push(self.expect_ident()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
            }
            return Ok(Pattern::Variant(name, binds));
        }
        match name.as_str() {
            "Some" => Ok(Pattern::Some(self.pattern_binding()?)),
            "Ok" => Ok(Pattern::Ok(self.pattern_binding()?)),
            "Err" => Ok(Pattern::Err(self.pattern_binding()?)),
            "None" => Ok(Pattern::None),
            // Any other identifier is a user-enum variant: `V`, `V(x)`, `V(x, y)`.
            _ => {
                let _ = line;
                let mut binds = Vec::new();
                if *self.peek() == Tok::LParen {
                    self.advance();
                    while *self.peek() != Tok::RParen {
                        binds.push(self.expect_ident()?);
                        if *self.peek() == Tok::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    self.eat(&Tok::RParen)?;
                }
                Ok(Pattern::Variant(name, binds))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn parse_src(s: &str) -> Program {
        parse(lex(s).unwrap()).unwrap()
    }

    #[test]
    fn semicolons_are_optional() {
        // No terminators at all — statement boundaries fall where an expression
        // can't extend. Parses to the same shape as the semicolon-terminated form.
        let src = "fn main() -> Int64 {\n\
                   let a = 1\n\
                   let mut b = 2\n\
                   b = b + a\n\
                   return b\n\
                   }";
        let p = parse_src(src);
        let f = &p.functions[0];
        assert_eq!(f.body.stmts.len(), 4);
        assert!(matches!(f.body.stmts[0], Stmt::Let { .. }));
        assert!(matches!(f.body.stmts[2], Stmt::Assign { .. }));
        assert!(matches!(f.body.stmts[3], Stmt::Return { value: Some(_), .. }));
    }

    #[test]
    fn else_if_desugars_to_nested_if_with_honest_lines() {
        // `else if` (RFC-0022) parses to the ordinary nested form: the else-block
        // is a single-statement block whose one statement is the chained `if`,
        // carrying its own line number.
        let src = "fn f() -> Int64 {\n\
                   if a == 1 {\n\
                   return 1\n\
                   } else if a == 2 {\n\
                   return 2\n\
                   } else {\n\
                   return 3\n\
                   }\n\
                   }";
        let p = parse_src(src);
        let f = &p.functions[0];
        let Stmt::If { else_block: Some(eb), .. } = &f.body.stmts[0] else {
            panic!("expected an if with an else");
        };
        // The else-block holds exactly the chained `if`, at its real source line 4.
        assert_eq!(eb.stmts.len(), 1);
        let Stmt::If { line, else_block: Some(inner), .. } = &eb.stmts[0] else {
            panic!("else-block's sole statement is the chained if");
        };
        assert_eq!(*line, 4, "chained if keeps its own line for diagnostics");
        // The innermost else is a plain block (the final `else`).
        assert_eq!(inner.stmts.len(), 1);
        assert!(matches!(inner.stmts[0], Stmt::Return { .. }));
    }

    #[test]
    fn else_if_equals_the_nested_form() {
        // The sugar is exact: `else if` and the hand-written nested `else { if }`
        // produce identical ASTs.
        let sugar = parse_src(
            "fn f() -> Int64 { if a { return 1 } else if b { return 2 } else { return 3 } }",
        );
        let nested = parse_src(
            "fn f() -> Int64 { if a { return 1 } else { if b { return 2 } else { return 3 } } }",
        );
        assert_eq!(sugar.functions[0].body, nested.functions[0].body);
    }

    #[test]
    fn namespace_import_parses() {
        // RFC-0027: `import * as ns from <source>` binds a namespace, no flat names.
        let p = parse_src(
            "import * as api from \"./api\" \
             import * as ui from pages(\"./pages\") \
             fn main() -> Int64 { return 0 }",
        );
        assert_eq!(p.imports[0].namespace.as_deref(), Some("api"));
        assert!(p.imports[0].names.is_empty());
        assert!(matches!(&p.imports[0].source, ImportSource::Path(s) if s == "./api"));
        assert_eq!(p.imports[1].namespace.as_deref(), Some("ui"));
        assert!(matches!(&p.imports[1].source, ImportSource::Generator { name, .. } if name == "pages"));
    }

    #[test]
    fn namespace_qualified_type_and_record_parse() {
        // `ns.User` in a type position and `ns.Req { .. }` record construction.
        let p = parse_src(
            "import * as api from \"./api\" \
             fn main() -> Int64 { let r: api.User = api.Req { id: 1 } return 0 }",
        );
        let Stmt::Let { ty: Some(ty), value, .. } = &p.functions[0].body.stmts[0] else {
            panic!("let with type")
        };
        assert_eq!(*ty, Type::Named("api.User".into()));
        assert!(matches!(value, Expr::StructLit { name, .. } if name == "api.Req"));
    }

    #[test]
    fn gen_fn_parses_as_a_generator_marked_function() {
        // RFC-0021: `gen fn` is an ordinary function with `is_gen` set.
        let p = parse_src(
            "gen fn make(dir: String) -> String { return \"fn x() -> Int64 { return 0 }\" } \
             fn main() -> Int64 { return 0 }",
        );
        let g = p.functions.iter().find(|f| f.name == "make").unwrap();
        assert!(g.is_gen);
        assert!(!g.is_extern);
        let m = p.functions.iter().find(|f| f.name == "main").unwrap();
        assert!(!m.is_gen);
    }

    #[test]
    fn export_gen_fn_parses() {
        let p = parse_src(
            "export gen fn g() -> String { return \"\" } fn main() -> Int64 { return 0 }",
        );
        let g = p.functions.iter().find(|f| f.name == "g").unwrap();
        assert!(g.is_gen && g.exported);
    }

    #[test]
    fn gen_is_still_an_identifier_elsewhere() {
        // `gen` as a variable name is unharmed (contextual: only `gen fn` is special).
        let p = parse_src("fn main() -> Int64 { let gen = 3 return gen }");
        let f = &p.functions[0];
        assert!(matches!(&f.body.stmts[0], Stmt::Let { name, .. } if name == "gen"));
    }

    #[test]
    fn import_from_generator_call_parses() {
        // RFC-0021: `import { .. } from ident(args)`.
        let p = parse_src(
            "import { t, TransKey } from i18n(\"./locales\", 3) \
             fn main() -> Int64 { return 0 }",
        );
        let imp = &p.imports[0];
        assert_eq!(
            imp.names,
            vec![ImportName::bare("t"), ImportName::bare("TransKey")]
        );
        match &imp.source {
            ImportSource::Generator { name, args, .. } => {
                assert_eq!(name, "i18n");
                assert_eq!(args.len(), 2);
                assert!(matches!(&args[0], Expr::Str(s) if s == "./locales"));
                assert!(matches!(&args[1], Expr::Int(3)));
            }
            other => panic!("expected a generator source, got {other:?}"),
        }
    }

    #[test]
    fn import_from_path_string_still_parses() {
        let p = parse_src("import { x } from \"./lib\" fn main() -> Int64 { return 0 }");
        assert!(matches!(&p.imports[0].source, ImportSource::Path(p) if p == "./lib"));
    }

    #[test]
    fn import_aliasing_parses_original_and_alias() {
        // RFC-0022: `original as alias`; bare names keep `alias: None`.
        let p = parse_src(
            "import { getUser as fetchUser, User } from \"./api\" \
             fn main() -> Int64 { return 0 }",
        );
        assert_eq!(
            p.imports[0].names,
            vec![
                ImportName { original: "getUser".into(), alias: Some("fetchUser".into()) },
                ImportName::bare("User"),
            ]
        );
        assert_eq!(p.imports[0].names[0].local(), "fetchUser");
        assert_eq!(p.imports[0].names[1].local(), "User");
    }

    #[test]
    fn as_is_still_an_identifier_elsewhere() {
        // `as` is contextual (only special between an import name and its
        // separator); a variable named `as` is unharmed.
        let p = parse_src("fn main() -> Int64 { let as = 3 return as }");
        let f = &p.functions[0];
        assert!(matches!(&f.body.stmts[0], Stmt::Let { name, .. } if name == "as"));
    }

    #[test]
    fn inline_field_where_desugars_to_synthetic_validated_type() {
        let src = "type User = { name: String where value.length >= 3, age: Int64 } \
                   fn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        // The synthetic `User.name` decl carries the predicate…
        let synth = p.type_decls.iter().find(|t| t.name == "User.name").expect("synthetic decl");
        assert_eq!(synth.base, Type::Str);
        assert!(synth.predicate.is_some());
        // …and the field's type is rewritten to reference it.
        let user = p.type_decls.iter().find(|t| t.name == "User").unwrap();
        let Type::Record(fields) = &user.base else { panic!("record") };
        assert_eq!(fields[0].ty, Type::Named("User.name".into()));
        assert_eq!(fields[1].ty, Type::Int, "unrefined fields untouched");
        assert!(user.predicate.is_none());
    }

    #[test]
    fn inline_field_where_composes_with_cross_field_where() {
        let src = "type R = { a: Int64 where value > 0, b: Int64 } where a < b \
                   fn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        let r = p.type_decls.iter().find(|t| t.name == "R").unwrap();
        assert!(r.predicate.is_some(), "cross-field where stays on the record");
        assert!(p.type_decls.iter().any(|t| t.name == "R.a"), "field where desugars");
    }

    #[test]
    fn inline_where_outside_a_type_decl_is_an_error() {
        // An anonymous record (a parameter's type here) has no name to attach
        // the synthetic refinement type to.
        let src = "fn f(x: { n: Int64 where value > 0 }) -> Int64 { return 0 } \
                   fn main() -> Int64 { return 0 }";
        let e = parse(lex(src).unwrap()).unwrap_err();
        assert!(e.message.contains("named record type"), "{}", e.message);
    }

    #[test]
    fn parses_top_level_let_module_state() {
        // RFC-0013: `let [mut] name [: Type] = init` at the top level.
        let src = "let mut hits: Int64 = 0\n\
                   let banner = \"hi\"\n\
                   fn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        assert_eq!(p.globals.len(), 2);
        assert_eq!(p.globals[0].name, "hits");
        assert!(p.globals[0].mutable);
        assert_eq!(p.globals[0].ty, Some(Type::Int));
        assert_eq!(p.globals[1].name, "banner");
        assert!(!p.globals[1].mutable);
        assert_eq!(p.globals[1].ty, None);
    }

    #[test]
    fn top_level_let_requires_initializer() {
        let src = "let mut hits: Int64\nfn main() -> Int64 { return 0 }";
        let e = parse(lex(src).unwrap()).unwrap_err();
        assert!(e.message.contains("needs an initializer"), "{}", e.message);
    }

    #[test]
    fn top_level_let_doc_comment_attaches() {
        let src = "/// the live counter\nlet mut hits = 0\nfn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        assert_eq!(p.globals[0].doc.as_deref(), Some("the live counter"));
    }

    #[test]
    fn bad_top_level_let_recovers_to_next_decl() {
        // A malformed global (name is not an identifier) must not swallow the
        // following function — recovery syncs to the next top-level `fn`.
        let src = "let 123 = 5\nfn main() -> Int64 { return 0 }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert!(!errors.is_empty(), "expected a parse error for the bad global");
        assert!(p.functions.iter().any(|f| f.name == "main"), "recovered to `main`");
    }

    // ---- RFC-0015 tests ------------------------------------------------

    #[test]
    fn parses_test_declaration() {
        let src = "test \"adds\" {\n\
                   assert(1 + 1 == 2)\n\
                   assertEq(2 + 2, 4)\n\
                   }";
        let p = parse_src(src);
        assert_eq!(p.tests.len(), 1);
        assert_eq!(p.tests[0].name, "adds");
        assert_eq!(p.tests[0].body.stmts.len(), 2);
        assert_eq!(p.tests[0].line, 1);
    }

    #[test]
    fn test_is_only_contextual_before_a_string() {
        // `test` used as an ordinary identifier (not followed by a string) is
        // still a plain variable — it must not steal the keyword slot.
        let src = "fn main() -> Int64 { let test = 5 return test }";
        let p = parse_src(src);
        assert!(p.tests.is_empty());
        assert_eq!(p.functions[0].body.stmts.len(), 2);
    }

    #[test]
    fn test_doc_comment_attaches() {
        let src = "/// checks the happy path\ntest \"ok\" { assert(true) }";
        let p = parse_src(src);
        assert_eq!(p.tests[0].doc.as_deref(), Some("checks the happy path"));
    }

    #[test]
    fn bad_test_recovers_to_next_decl() {
        // A malformed test body must not swallow the following function —
        // recovery syncs to the next top-level starter.
        let src = "test \"broken\" { let = }\nfn main() -> Int64 { return 0 }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert!(!errors.is_empty(), "expected a parse error for the bad test");
        assert!(p.functions.iter().any(|f| f.name == "main"), "recovered to `main`");
    }

    #[test]
    fn stray_semicolon_after_block_statement_is_tolerated() {
        // "Semicolons optional" includes a stray one after `if { .. }`.
        let src = "fn main() -> Int64 { if true { print(1) }; return 0 }";
        let p = parse_src(src);
        assert_eq!(p.functions[0].body.stmts.len(), 2);
    }

    #[test]
    fn gteq_splits_when_closing_a_generic() {
        // `Array<Int>= []` — the lexer max-munches `>=`; the parser splits it.
        let src = "fn main() -> Int64 { let x: Array<Int64>= array() return alen(x) }";
        let p = parse_src(src);
        assert!(matches!(
            p.functions[0].body.stmts[0],
            Stmt::Let { ty: Some(Type::Array(_)), .. }
        ));
    }

    #[test]
    fn identifier_before_next_lines_string_is_not_a_tagged_template() {
        // In semicolon-free code, a statement ending in a variable followed by
        // a statement starting with a string must not raise the tagged-template
        // error (adjacency requires the same line).
        let src = "fn main() -> Int64 {\n\
                   let y = 1\n\
                   let z = y\n\
                   print(\"done\")\n\
                   return 0\n\
                   }";
        let p = parse_src(src);
        assert_eq!(p.functions[0].body.stmts.len(), 4);
    }

    #[test]
    fn hole_parse_errors_are_anchored_at_the_template() {
        // The sub-parser's hole-relative line numbers must not leak.
        let toks = lex("fn main() -> Int64 {\n    let s = \"\\{ 1 + }\"\n    return 0\n}").unwrap();
        let e = parse(toks).unwrap_err();
        assert_eq!(e.line, 2, "{e:?}");
        assert!(e.message.contains("in interpolation"), "{}", e.message);
    }

    #[test]
    fn failed_generic_decl_does_not_leak_type_params() {
        // After a broken `fn bad<T>`, `T` in the NEXT declaration must parse as
        // a named type, not a stale Type::Param.
        let src = "fn bad<T>(x: T -> T { return x } \
                   type T = Int64 \
                   fn ok(x: T) -> T { return x } \
                   fn main() -> Int64 { return ok(1) }";
        let toks = lex(src).unwrap();
        let mut p = Parser {
            tokens: toks,
            pos: 0,
            no_struct: false,
            type_params: Vec::new(),
            field_preds: None,
            extra_stmts: Vec::new(),
            errors: Vec::new(),
        };
        let (prog, errors) = p.program_accum();
        assert!(!errors.is_empty(), "the broken decl must actually fail");
        let ok = prog.functions.iter().find(|f| f.name == "ok").expect("ok parsed");
        assert_eq!(ok.params[0].ty, Type::Named("T".into()), "not a stale Param");
    }

    #[test]
    fn bare_return_before_brace_needs_no_semicolon() {
        let src = "fn f(x: Int64) { if x > 0 { return } print(x) } fn main() -> Int64 { return 0 }";
        let p = parse_src(src);
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        match &f.body.stmts[0] {
            Stmt::If { then_block, .. } => {
                assert!(matches!(then_block.stmts[0], Stmt::Return { value: None, .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn attaches_doc_comments_to_declarations() {
        let src = "/// first line\n/// second line\nfn f() -> Int64 { return 0; }\n\
                   // not a doc\n/// a type doc\ntype T = Int64;\n\
                   //// four slashes: plain comment\nfn main() -> Int64 { return 0; }";
        let p = parse_src(src);
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        assert_eq!(f.doc.as_deref(), Some("first line\nsecond line"));
        let t = p.type_decls.iter().find(|t| t.name == "T").unwrap();
        assert_eq!(t.doc.as_deref(), Some("a type doc"));
        // `//` and `////` are plain comments — `main` has no doc.
        let m = p.functions.iter().find(|f| f.name == "main").unwrap();
        assert_eq!(m.doc, None);
    }

    /// A `///` block separated from the declaration by a blank line is a
    /// file-header comment, not the declaration's doc — it must not be glued
    /// on (observable via LSP hover and `schemaOf(T).doc`).
    #[test]
    fn detached_doc_blocks_do_not_attach() {
        // File header, blank line, then an undocumented function.
        let p = parse_src("/// This file does things.\n/// Extensively.\n\nfn f() -> Int64 { return 0; }\nfn main() -> Int64 { return 0; }");
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        assert_eq!(f.doc, None, "a detached header is not the decl's doc");

        // Header, blank line, then a real doc directly above the decl: only
        // the adjacent block attaches.
        let p = parse_src("/// Header.\n\n/// The real doc.\nfn f() -> Int64 { return 0; }\nfn main() -> Int64 { return 0; }");
        let f = p.functions.iter().find(|f| f.name == "f").unwrap();
        assert_eq!(f.doc.as_deref(), Some("The real doc."));
    }

    #[test]
    fn parses_precedence() {
        // 1 + 2 * 3  ==>  Add(1, Mul(2, 3))
        let p = parse_src("fn main() -> Int64 { return 1 + 2 * 3; }");
        let f = &p.functions[0];
        match &f.body.stmts[0] {
            Stmt::Return { value: Some(Expr::Binary { op: BinOp::Add, rhs, .. }), .. } => {
                assert!(matches!(**rhs, Expr::Binary { op: BinOp::Mul, .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_function_with_params() {
        let p = parse_src("fn add(a: Int64, b: Int64) -> Int64 { return a + b; }");
        let f = &p.functions[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.ret, Type::Int);
    }

    #[test]
    fn parses_region_block() {
        let p = parse_src("fn main() -> Int64 { region { print(1); } return 0; }");
        let f = &p.functions[0];
        match &f.body.stmts[0] {
            Stmt::Region { body, .. } => assert_eq!(body.stmts.len(), 1),
            other => panic!("expected region, got {other:?}"),
        }
    }

    // ---- RFC-0006: within-body statement recovery -----------------------

    #[test]
    fn two_bad_statements_each_report_and_good_ones_survive() {
        let src = "fn main() -> Int64 {\n\
                   let a = ;\n\
                   let good = 1\n\
                   let b = ;\n\
                   return good\n\
                   }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert_eq!(errors.len(), 2, "one diagnostic per bad statement: {errors:?}");
        assert_eq!(errors[0].line, 2);
        assert_eq!(errors[1].line, 4);
        // The good statements between/after the bad ones are kept.
        let body = &p.functions[0].body.stmts;
        assert!(body.iter().any(|s| matches!(s, Stmt::Let { name, .. } if name == "good")));
        assert!(matches!(body.last(), Some(Stmt::Return { .. })));
    }

    #[test]
    fn body_error_does_not_hide_a_later_bad_declaration() {
        // A recovered body error must not swallow a SEPARATE broken declaration
        // that follows — top-level recovery still reports it, and `main` (whose
        // body had the error) is still parsed.
        let src = "fn main() -> Int64 {\n\
                   let x = ;\n\
                   return 0\n\
                   }\n\
                   fn bad<T>(x: T -> T { return x }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert!(errors.len() >= 2, "body error AND decl error both reported: {errors:?}");
        assert_eq!(errors[0].line, 2, "body error comes first in source order");
        assert!(errors.iter().any(|e| e.line >= 5), "the bad decl is also reported: {errors:?}");
        assert!(p.functions.iter().any(|f| f.name == "main"), "main survives");
    }

    #[test]
    fn recovery_inside_a_nested_block() {
        // A bad statement inside an `if` body recovers within that inner block:
        // the good statement after it, and everything after the `if`, survive.
        let src = "fn main() -> Int64 {\n\
                   let mut n = 0\n\
                   if n == 0 {\n\
                   let x = ;\n\
                   n = 1\n\
                   }\n\
                   return n\n\
                   }";
        let (p, errors) = parse_accum(lex(src).unwrap());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].line, 4);
        let body = &p.functions[0].body.stmts;
        assert!(matches!(body.last(), Some(Stmt::Return { .. })), "return survives");
        match body.iter().find(|s| matches!(s, Stmt::If { .. })) {
            Some(Stmt::If { then_block, .. }) => {
                // The bad `let x` is dropped; the good `n = 1` remains.
                assert!(then_block.stmts.iter().any(|s| matches!(s, Stmt::Assign { .. })));
            }
            _ => panic!("the `if` statement survives"),
        }
    }

    // ---- RFC-0011 addendum: `a[i].field = v` write-through --------------

    #[test]
    fn index_field_assign_desugars_to_load_setfield_store() {
        // `a[i].f = v` becomes exactly `let mut a[] = a[i]  a[].f = v  a[i] = a[]`
        // (three statements spliced into the block, in order).
        let p = parse_src(
            "fn main() -> Int64 { let mut a: Array<Int64> = []  a[0].f = 9  return 0 }",
        );
        let stmts = &p.functions[0].body.stmts;
        // let a  |  let mut a[]=a[0]  |  a[].f=9  |  a[0]=a[]  |  return
        assert_eq!(stmts.len(), 5);
        match &stmts[1] {
            Stmt::Let { name, mutable, value: Expr::Call { name: c, args, .. }, .. } => {
                assert_eq!(name, "a[]");
                assert!(mutable, "the element copy must be mut so SetField applies");
                assert_eq!(c, "at");
                assert!(matches!(args[0], Expr::Var { .. }));
            }
            other => panic!("expected `let mut a[] = a[0]`, got {other:?}"),
        }
        match &stmts[2] {
            Stmt::SetField { name, field, .. } => {
                assert_eq!(name, "a[]");
                assert_eq!(field, "f");
            }
            other => panic!("expected SetField on the temp, got {other:?}"),
        }
        match &stmts[3] {
            Stmt::IndexSet { name, value: Expr::Var { name: v, .. }, .. } => {
                assert_eq!(name, "a", "stores back into the real array binding");
                assert_eq!(v, "a[]");
            }
            other => panic!("expected `a[0] = a[]`, got {other:?}"),
        }
    }

    #[test]
    fn nested_index_field_assign_is_rejected() {
        // One level only: `a[i].f.g = v` is a parse error.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []  a[0].f.g = 9  return 0 }";
        let e = parse(lex(src).unwrap()).unwrap_err();
        assert!(e.message.contains("single field write-through"), "{}", e.message);
    }

    #[test]
    fn index_field_assign_on_non_variable_array_is_rejected() {
        // The left side must be a plain array variable, not a call result.
        let src = "fn main() -> Int64 { f()[0].x = 9  return 0 }";
        let e = parse(lex(src).unwrap()).unwrap_err();
        assert!(e.message.contains("plain array variable"), "{}", e.message);
    }

    // ---- function values (RFC-0023) -------------------------------------

    fn only_arg(p: &Program) -> Expr {
        // The single call argument of `f(<arg>)` in `main`'s first statement.
        match &p.functions.last().unwrap().body.stmts[0] {
            Stmt::Let { value: Expr::Call { args, .. }, .. } => args[0].clone(),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_fn_type_in_parameter() {
        let p = parse_src("fn f(g: fn(Int64, Bool) -> Int64) -> Int64 { return 0 }");
        assert_eq!(
            p.functions[0].params[0].ty,
            Type::Fn(vec![Type::Int, Type::Bool], Box::new(Type::Int))
        );
        // `fn()` (no arrow) is a Unit-returning function type.
        let q = parse_src("fn f(g: fn()) -> Int64 { return 0 }");
        assert_eq!(q.functions[0].params[0].ty, Type::Fn(vec![], Box::new(Type::Unit)));
    }

    #[test]
    fn parses_expression_lambda() {
        let p = parse_src("fn f(g: fn(Int64) -> Int64) -> Int64 { return 0 }\n\
                           fn main() -> Int64 { let a = f(|x| x * 2)  return 0 }");
        match only_arg(&p) {
            Expr::Lambda { params, body, .. } => {
                assert_eq!(params, vec!["x".to_string()]);
                assert!(matches!(body, LambdaBody::Expr(_)));
            }
            other => panic!("expected lambda, got {other:?}"),
        }
    }

    #[test]
    fn parses_block_and_multiparam_and_niladic_lambda() {
        let p = parse_src("fn f(g: fn(Int64, Int64) -> Int64) -> Int64 { return 0 }\n\
                           fn main() -> Int64 { let a = f(|x, y| { return x + y })  return 0 }");
        match only_arg(&p) {
            Expr::Lambda { params, body, .. } => {
                assert_eq!(params, vec!["x".to_string(), "y".to_string()]);
                assert!(matches!(body, LambdaBody::Block(_)));
            }
            other => panic!("expected lambda, got {other:?}"),
        }
        // `||` is the zero-parameter lambda.
        let q = parse_src("fn f(g: fn() -> Int64) -> Int64 { return 0 }\n\
                           fn main() -> Int64 { let a = f(|| 7)  return 0 }");
        match only_arg(&q) {
            Expr::Lambda { params, .. } => assert!(params.is_empty()),
            other => panic!("expected niladic lambda, got {other:?}"),
        }
    }

    #[test]
    fn lambda_body_precedence_spans_or() {
        // `|x| a || b` — the body is the whole `a || b`, not just `a`.
        let p = parse_src("fn f(g: fn(Bool) -> Bool) -> Int64 { return 0 }\n\
                           fn main() -> Int64 { let a = f(|x| x || false)  return 0 }");
        match only_arg(&p) {
            Expr::Lambda { body: LambdaBody::Expr(e), .. } => {
                assert!(matches!(*e, Expr::Binary { op: BinOp::Or, .. }));
            }
            other => panic!("expected lambda with Or body, got {other:?}"),
        }
    }
}
