//! Symbol-query API for the LSP, over the parsed [`crate::ast::Program`] and the
//! lexer's token positions. The AST keeps only a line per node, so the name
//! columns and the cursor-to-identifier mapping come from the tokens. [`analyze`]
//! runs the whole pipeline once per document and returns diagnostics, the symbol
//! index and the identifier tokens; the LSP serves hover, go-to-definition and
//! completion from that [`Analysis`].

use std::collections::HashMap;

use crate::ast::{
    self, Capability, EnumVariant, Expr, Function, GlobalDecl, MethodSig, ProtocolDecl, Speech,
    Spellings, Stmt, Type, TypeDecl,
};
use crate::checker;
use crate::diagnostics::Diagnostic;
use crate::lexer::{self, Tok};
use crate::parser;
use crate::schema_reflect::{field_text, variant_arm};
use crate::symbolmap::MappedSymbol;

/// Kind of a top-level symbol or a local binding (returned by [`resolve`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Type,
    Variant,
    Method,
    /// A record field. Member completion only: a field is not a standalone
    /// declaration, so it never appears in the symbol index.
    Field,
    /// A function parameter.
    Param,
    /// A `let` binding or a `for`-in loop variable, local to a function body.
    Local,
    /// A top-level module-state binding: `let [mut] name = init`.
    Global,
}

/// A top-level declaration the LSP can hover / jump to / complete.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// 1-based declaration line.
    pub line: usize,
    /// 1-based name column (0 = unknown; whole-line fallback).
    pub col: usize,
    /// 1-based column just past the name (exclusive); 0 = unknown.
    pub end_col: usize,
    /// Hover / signature text.
    pub detail: String,
    /// The declaration's `///` documentation (markdown), shown beneath the
    /// signature on hover. `None` without a doc comment, and for enum variants
    /// and protocol method signatures, which carry no doc in the AST.
    pub doc: Option<String>,
    /// The module file this symbol was declared in: `None` for the open document,
    /// `Some(path)` for a symbol imported by [`analyze_linked`]. A foreign
    /// symbol has `col == 0`, because its columns belong to the other file.
    pub file: Option<String>,
    /// `let mut` module state; `false` for every other kind.
    pub mutable: bool,
}

/// An identifier token's source range, for cursor-to-token mapping.
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub text: String,
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
}

/// A local binding and its flavour, as the checker records them.
pub use crate::checker::{LocalBinding, LocalKind};

/// Everything the LSP needs for one document, built in a single pass.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub diagnostics: Vec<Diagnostic>,
    pub symbols: Vec<Symbol>,
    pub tokens: Vec<TokenInfo>,
    /// Local bindings (params, lets, for-variables) per function, for variable
    /// hover and go-to-definition.
    pub locals: Vec<LocalBinding>,
    /// What each identifier occurrence the check resolved names, by its
    /// `(line, col)`: the index in [`Self::locals`] of its binding, or `None`
    /// for a name past the locals. A binder names itself. [`local_at`] reads it.
    pub names: HashMap<(usize, usize), Option<usize>>,
    /// Sorted top-level declaration lines (functions, types, protocols, impls),
    /// which bound a function's line range when finding the enclosing function.
    pub decl_lines: Vec<usize>,
    /// The subset of [`Self::decl_lines`] that are function declarations
    /// (functions and impl methods), so a cursor inside a type or protocol decl
    /// is not taken for the preceding function.
    pub fn_lines: Vec<usize>,
    /// User protocol methods per implementing type (`impl P for T` gives `T`'s
    /// methods), for member completion on a concrete receiver. Indexed from the
    /// linked program when there is one, so imported impls count.
    pub impl_members: Vec<(Type, Completion)>,
    /// Each protocol's methods by protocol name, for member completion on a
    /// bounded generic receiver (`x: T` with `T: Show` offers `Show`'s methods).
    pub protocol_members: Vec<(String, Completion)>,
    /// Per-function type-parameter bounds: `(fn decl line, type param, bound
    /// names)`, so a `Named("T")` receiver finds its protocols.
    pub type_param_bounds: Vec<(usize, String, Vec<String>)>,
    /// Record fields by declaring type name, for member completion on a record
    /// receiver. A refined field renders as written
    /// (`age: Int64 where value >= 18`).
    pub record_fields: Vec<(String, Completion)>,
    /// Every finite validated string type with its enumerated language, up to a
    /// cap, for string-literal completion: `t("` offers every
    /// `TransKey`. A type past the cap, infinite or not a regex is absent.
    pub finite_string_types: Vec<(String, Vec<String>)>,
    /// Top-level function name to parameter types, so string-literal completion
    /// finds the expected type at a call argument.
    pub fn_param_types: Vec<(String, Vec<Type>)>,
    /// Every sequence validated string type: an infinite
    /// space-separated sequence over a finite alphabet, such as `Tw`. Maps it to
    /// its single-token alphabet, taken from the DFA the compiler checks against,
    /// for token completion inside `class="..."` and `theme.cls("...")`.
    pub sequence_string_types: Vec<(String, Vec<String>)>,
    /// The theme's utility stylesheet, the constant `std/tw`'s `css()` returns,
    /// so hover on a class token shows its rule or reports it
    /// safelisted. Empty when no theme is linked.
    pub tw_css: String,
    /// Each `import * as ns` binding and the exported symbols it reaches,
    /// so `ns.` completes and `ns.member` hovers and jumps. Filled
    /// only by [`analyze_linked`].
    pub namespaces: Vec<NamespaceInfo>,
    /// The origin tables of every generated module reachable from this document,
    /// for hover, completion and go-to-definition inside generator
    /// input files. Empty without a linker or without `//@origin` directives.
    pub origins: crate::origin::OriginMaps,
    /// Diagnostics relocated to a generator input file such as a `.vyx`.
    /// The LSP publishes them against that file's URI; they are not
    /// in [`Self::diagnostics`], which stays anchored to the open document.
    pub remapped: Vec<Diagnostic>,
    /// Every symbol the reachable generated modules map back to a declaration,
    /// with its derived wire facts. A declaration's own hover reads
    /// them: its file reaches no generator, so only a root that does knows them.
    pub symbol_maps: Vec<crate::symbolmap::MappedSymbol>,
    /// What the ownership analysis decided about every `let` in this document:
    /// reclaimed, or why not, at the cursor. Read from [`Judged::memory`],
    /// never re-derived, so it cannot disagree with the walk that decided. Empty when the checks did not run
    /// or no [`Judge`] was given.
    pub memory: Vec<MemoryNote>,
    /// What each function of this document costs, line by line: the rows of
    /// `vyrn why --cost`, read from [`Judged::cost`]. Empty when the checks did not run, found an
    /// error or no [`Judge`] was given.
    pub cost: Vec<FnCost>,
    /// The hash of every module the document links ([`crate::ast::Program::module_hashes`]),
    /// which stamps a saved profile with the source it ran. Empty when the checks did not run.
    pub module_hashes: std::collections::BTreeMap<String, String>,
    /// How hover, completion and type hints spell a declaration the loader
    /// renamed apart. Empty for a document no load linked.
    pub spellings: std::sync::Arc<Spellings>,
}

/// One binding's memory answer, positioned for the editor: a
/// [`crate::own::MemoryRow`] with a position. The prose is the core's.
#[derive(Debug, Clone)]
pub struct MemoryNote {
    pub name: String,
    /// 1-based line of the `let`.
    pub line: usize,
    /// What happens to the value, in one line ([`crate::own::MemoryRow::text`]).
    pub text: String,
    /// The line where the value stops being live, when there is one: a move or
    /// a `drop`. `None` for a binding that lives to block exit.
    pub last_use: Option<usize>,
    /// What took it, for the inlay hint. `Some` exactly when `last_use` is a move.
    pub moved_into: Option<String>,
}

/// What one function of the document costs: its rows of `vyrn why --cost`, ordered by line and
/// then by verb.
#[derive(Debug, Clone)]
pub struct FnCost {
    pub name: String,
    /// 1-based line of the declaration.
    pub line: usize,
    pub lines: Vec<CostLine>,
}

/// What one verb does on one line of a function, in the words `vyrn why --cost` prints.
#[derive(Debug, Clone)]
pub struct CostLine {
    /// 1-based line.
    pub line: usize,
    /// `copies`, `allocates`, `enters`, `grows` or `check kept`.
    pub verb: &'static str,
    /// The deepest loop nest among the line's facts of this verb.
    pub depth: u32,
    /// How many facts the line states.
    pub count: usize,
    /// Whether a copy is one the reader did not write.
    pub implicit: bool,
    /// What it does, as `why --cost` prints it.
    pub text: String,
    /// For a kept check, the distinct reasons in two words each, comma-joined
    /// (`callee fact, caller fact`); empty for any other verb.
    pub short: String,
}

/// One `import * as ns` binding and the exported declarations it exposes.
/// Each member carries its file and line, like an imported
/// [`Symbol`].
#[derive(Debug, Clone)]
pub struct NamespaceInfo {
    pub name: String,
    pub members: Vec<Symbol>,
}

/// Answer to "what is at this cursor": the declaration it resolves to.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub name: String,
    pub kind: SymbolKind,
    /// Declaration location (for go-to-definition).
    pub target_line: usize,
    pub target_col: usize,
    pub target_end_col: usize,
    /// The file the declaration lives in: `None` for the open document,
    /// `Some(path)` for an imported symbol. The LSP gives no definition for a
    /// remote module key (`github:...`).
    pub target_file: Option<String>,
    /// Detail text (for hover).
    pub hover: String,
    /// The part of `hover` that is a declaration signature, which the editor
    /// shows as code. `None` when `hover` is prose.
    pub signature: Option<String>,
    /// Everything in `hover` after the signature, without the blank line that
    /// separates them. The whole of `hover` when there is no signature.
    pub doc: String,
    /// Whether a source declaration exists to jump to. `false` for a built-in
    /// method such as `push`: it has hover text but no definition site.
    pub definition: bool,
}

/// One completion item (a top-level symbol).
#[derive(Debug, Clone)]
pub struct Completion {
    pub label: String,
    pub kind: SymbolKind,
    pub detail: String,
    /// The item's `///` documentation, when its declaration has one.
    pub doc: Option<String>,
}

/// Lexes, parses, type-checks and indexes `source` in one pass.
///
/// A lex error leaves `symbols`, `tokens` and `locals` empty, with the one lex
/// error in `diagnostics`. The parser recovers between declarations
/// and between statements, so after a parse error the partial program is still
/// indexed and hover, outline and completion keep working. The type check is
/// skipped after any parse error, so `diagnostics` then holds parse errors
/// only. [`analyze_judged`] runs the pipeline `vyrn check` runs instead.
pub fn analyze(source: &str) -> Analysis {
    analyze_inner(source, None, None, None)
}

/// Like [`analyze`], but resolves the document's imports through the module
/// loader, so a multi-file program gets real diagnostics.
///
/// Diagnostics come from the linked program. A problem inside an imported
/// module is reported at line 0 with an `in <file>: ...` prefix, so it shows
/// without being anchored to a wrong line of the open document.
pub fn analyze_linked(
    source: &str,
    root_path: &str,
    opts: &crate::loader::LoadOptions,
    resolver: &dyn crate::loader::ModuleResolver,
    engine: Option<&crate::gen::GenEngine>,
) -> Analysis {
    analyze_inner(source, Some((root_path, opts, resolver)), engine, None)
}

/// The pipeline `vyrn check` runs over a loaded program. It lives in
/// `vyrn-lower`, above this crate, which passes it in (`vyrn_lower::JUDGE`).
#[derive(Clone, Copy)]
pub struct Judge {
    /// Checks and synthesizes the program, judges ownership, and makes the
    /// floor decision the load deferred.
    pub check: fn(
        &mut crate::ast::Program,
        Option<&crate::gen::GenEngine>,
        Option<crate::floor::Pending>,
    ) -> Judged,
}

/// What [`analyze_linked`] links with: the root path, the load options and
/// the resolver.
pub type Linker<'a> = (
    &'a str,
    &'a crate::loader::LoadOptions,
    &'a dyn crate::loader::ModuleResolver,
);

/// What [`Judge::check`] returns.
pub struct Judged {
    /// Every diagnostic, in the order `vyrn check` prints them.
    pub diagnostics: Vec<Diagnostic>,
    /// The root module's bindings, typed.
    pub binders: crate::checker::Binders,
    /// Per function, the placer's memory rows ([`crate::own::Ownership::memory`]);
    /// empty for a program the kernel did not judge.
    pub memory: HashMap<crate::ast::FnId, Vec<crate::own::MemoryRow>>,
    /// The cost of each function of the root file; empty for a program the kernel did not judge.
    pub cost: Vec<FnCost>,
}

/// Like [`analyze_linked`], but runs the pipeline `vyrn check` runs:
/// `judge`'s diagnostics, and its memory rows on hover. With no `linker`, the
/// source loads as `untitled.vyrn` in the working directory, with the default
/// std root, as `vyrn check` would load that file. `engine` runs its
/// generators.
pub fn analyze_judged(
    source: &str,
    linker: Option<Linker<'_>>,
    engine: Option<&crate::gen::GenEngine>,
    judge: &Judge,
) -> Analysis {
    let opts = crate::loader::LoadOptions {
        std_root: crate::manifest::std_root(),
        ..Default::default()
    };
    let linker = linker.unwrap_or(("untitled.vyrn", &opts, &crate::loader::DiskResolver));
    analyze_inner(source, Some(linker), engine, Some(judge))
}

/// Rewrites a foreign-file diagnostic so it shows in the root document without
/// a wrong anchor.
fn adopt_foreign(mut d: Diagnostic) -> Diagnostic {
    if let Some(file) = d.file.take() {
        d.message = format!("in {file}: {}", d.message);
        d.line = 0;
        d.col = 0;
        d.end_col = 0;
    }
    d
}

fn analyze_inner(
    source: &str,
    linker: Option<Linker<'_>>,
    engine: Option<&crate::gen::GenEngine>,
    judge: Option<&Judge>,
) -> Analysis {
    let tokens = match lexer::lex(source) {
        Ok(t) => t,
        Err(d) => return empty_analysis(vec![d]),
    };
    // Cache identifier tokens, for cursor mapping and for each declaration
    // name's column, plus `.` tokens: [`member_completions`] finds the receiver
    // before a `.foo` through them. No name is `.`, so they do not affect
    // [`resolve`] or the name-column search.
    let tok_info: Vec<TokenInfo> = tokens
        .iter()
        .filter_map(|t| match &t.tok {
            Tok::Ident(s) => Some(TokenInfo {
                text: s.clone(),
                line: t.line,
                col: t.col,
                end_col: t.col + s.chars().count(),
            }),
            Tok::Dot => Some(TokenInfo {
                text: ".".to_string(),
                line: t.line,
                col: t.col,
                end_col: t.col + 1,
            }),
            _ => None,
        })
        .collect();
    // First column of each keyword or operator token, per line, captured before
    // the parser takes `tokens`. [`pin_diagnostics`] pins a line-only diagnostic
    // to the keyword it is about.
    let mut kw_cols: std::collections::HashMap<
        usize,
        std::collections::HashMap<String, (usize, usize)>,
    > = std::collections::HashMap::new();
    for t in tokens.iter() {
        if let Some(text) = keyword_text(&t.tok) {
            let width = text.chars().count();
            kw_cols
                .entry(t.line)
                .or_default()
                .entry(text)
                .or_insert((t.col, t.col + width));
        }
    }

    let (program, parse_errors) = parser::parse_accum(tokens);
    // After a parse error the partial program still feeds the index below, but
    // the type and ownership checks are skipped: on a partial AST they would
    // only cascade into bogus diagnostics.
    let parse_failed = !parse_errors.is_empty();
    let mut diags: Vec<Diagnostic> = parse_errors;

    // With a linker, check the linked program, as `vyrn check` does; the
    // parsed root still feeds the index. `pending` is the floor decision the
    // load leaves to the judge. `None` means the check was skipped or the link
    // failed, with its diagnostics already in `diags`. `remapped` holds the
    // diagnostics that belong to a generator input file.
    let mut origins = crate::origin::OriginMaps::default();
    let mut graph: crate::loader::ModuleGraph = Vec::new();
    let mut remapped: Vec<Diagnostic> = Vec::new();
    let mut pending = None;
    let mut checked: Option<crate::ast::Program> = if parse_failed {
        None
    } else {
        match &linker {
            Some((root_path, opts, resolver)) => {
                let (loaded, o, load_warnings, g, p) =
                    crate::loader::load_with_origins(source, root_path, opts, *resolver, engine);
                pending = p;
                graph = g;
                // The origin maps come back even from a failed load,
                // so a `.vyx` whose template stopped lexing still gets its squiggle.
                origins = o;
                // A generator's warning is already positioned in its input file,
                // so it routes like a remapped error.
                for d in load_warnings {
                    if d.from_generated {
                        remapped.push(d);
                    } else {
                        diags.push(adopt_foreign(d));
                    }
                }
                match loaded {
                    Ok(linked) => Some(linked),
                    Err(load_diags) => {
                        // The loader already remapped failures inside a generated
                        // module onto their input file, so those go to `remapped`
                        // and land in the buffer the user is typing in.
                        for d in load_diags {
                            if d.from_generated {
                                remapped.push(d);
                            } else {
                                diags.push(adopt_foreign(d));
                            }
                        }
                        None
                    }
                }
            }
            None => Some(program.clone()),
        }
    };
    // The check returns the diagnostics and every binding it made in the root
    // module, typed, so an unannotated `let x = 5` hovers as `let x: Int64`.
    let mut memory = HashMap::new();
    let mut cost = Vec::new();
    let locals = match &mut checked {
        Some(prog) => {
            let cs = crate::prof::phase("check: the analysis's own");
            let (checked_diags, binders) = checker::with_uses(|| match judge {
                Some(judge) => {
                    let judged = (judge.check)(prog, engine, pending);
                    memory = judged.memory;
                    cost = judged.cost;
                    (judged.diagnostics, judged.binders)
                }
                None => checker::check_accum_recording(prog),
            });
            drop(cs);
            // A diagnostic at an origin-governed line of a generated module moves
            // to its input file and is set aside for that file's URI.
            // Everything else shows in the root at line 0.
            for mut d in checked_diags {
                if !origins.is_empty() && origins.remap(&mut d) {
                    remapped.push(d);
                } else {
                    diags.push(adopt_foreign(d));
                }
            }
            binders
        }
        // A parse error stops the check; the recovered statements still bind
        // names the reader hovers, untyped.
        None => checker::Binders {
            locals: checker::local_index(&program, &Default::default()),
            uses: Default::default(),
        },
    };
    let names = name_index(&locals.locals, locals.uses);
    let locals = locals.locals;
    pin_diagnostics(&mut diags, &kw_cols, &tok_info);

    // The symbol map every generated module bakes in, so a symbol
    // that stands for a declaration resolves to it: file, line, doc and wire
    // facts. A module with no map costs a failed substring search.
    let origin_index = match linker {
        Some((_, _, resolver)) if !graph.is_empty() => OriginIndex::build(&graph, resolver),
        _ => OriginIndex::default(),
    };

    let spellings = checked
        .as_ref()
        .map(|p| p.spellings.clone())
        .unwrap_or_default();
    let decl_lines = decl_lines(&program);
    let fn_lines = fn_lines(&program);
    let mut symbols = index_symbols(&program, &tok_info, &decl_lines, &spellings);
    // Declarations the root imports, indexed from the linked program with their
    // file, so hover and go-to-definition reach the imported module.
    if let Some(linked) = &checked {
        symbols.extend(index_imported_symbols(
            &program,
            linked,
            &graph,
            &origin_index,
        ));
    }
    // Namespace bindings and their exports; needs the linker to map
    // each namespace import to its module.
    let namespaces = index_namespaces(&graph, &program, linker, &origin_index);

    // Member tables for `.foo` completion. Impls and protocols come from the
    // linked program when there is one, since impls are global; bounds come from
    // the root's functions, the only bodies with a cursor.
    let member_src = checked.as_ref().unwrap_or(&program);
    let mut impl_members = Vec::new();
    for imp in &member_src.impls {
        for m in &imp.methods {
            let doc = m
                .doc
                .clone()
                .or_else(|| signature_doc(&member_src.protocols, &imp.protocol, &m.name));
            impl_members.push((
                imp.ty.clone(),
                Completion {
                    label: m.name.clone(),
                    kind: SymbolKind::Method,
                    detail: function_detail(m, &spellings),
                    doc,
                },
            ));
        }
    }
    let mut protocol_members = Vec::new();
    for p in &member_src.protocols {
        for m in &p.methods {
            protocol_members.push((
                p.name.clone(),
                Completion {
                    label: m.name.clone(),
                    kind: SymbolKind::Method,
                    detail: method_sig_detail(m, &spellings),
                    doc: m.doc.clone(),
                },
            ));
        }
    }
    let type_param_bounds = program
        .functions
        .iter()
        .chain(program.impls.iter().flat_map(|i| i.methods.iter()))
        .flat_map(|f| {
            f.type_bounds
                .iter()
                .map(|(tp, bs)| (f.line, tp.clone(), bs.clone()))
        })
        .collect();
    let mut record_fields = Vec::new();
    for t in &member_src.type_decls {
        // Skip synthetic inline-refinement decls (`User.age`); the parent record
        // holds the fields.
        if ast::is_synthetic(&t.name) {
            continue;
        }
        if let Type::Record(fields) = &t.base {
            for f in fields {
                record_fields.push((
                    t.name.clone(),
                    Completion {
                        label: f.name.clone(),
                        kind: SymbolKind::Field,
                        detail: {
                            let say = signature_speech(&spellings, &[&f.ty], &[]);
                            field_text(
                                f,
                                |n| member_src.type_decls.iter().find(|d| d.name == n),
                                &|t| spell(&say, t),
                            )
                        },
                        doc: None,
                    },
                ));
            }
        }
    }

    // Enumerate each finite string type's language once, up to the cap,
    // and record top-level fn parameter types, for string-literal
    // completion.
    const STRING_COMPLETION_CAP: usize = 1000;
    let mut finite_string_types = Vec::new();
    for t in &member_src.type_decls {
        if ast::is_synthetic(&t.name) {
            continue;
        }
        if let Some(domain) = crate::finite::enumerate_type(t, STRING_COMPLETION_CAP) {
            finite_string_types.push((t.name.clone(), domain));
        }
    }
    let fn_param_types = member_src
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.ty.clone()).collect(),
            )
        })
        .collect();

    // The token alphabet of each sequence string type, for class
    // token completion. The cap is higher than the finite path's: a realistic
    // theme's alphabet runs to a couple of thousand tokens.
    const CLASS_ALPHABET_CAP: usize = 8192;
    let mut sequence_string_types = Vec::new();
    for t in &member_src.type_decls {
        if ast::is_synthetic(&t.name) {
            continue;
        }
        if let Some(alphabet) = crate::finite::enumerate_alphabet(t, CLASS_ALPHABET_CAP) {
            sequence_string_types.push((t.name.clone(), alphabet));
        }
    }
    // The theme stylesheet (`std/tw`'s baked `css()`), captured only when a
    // sequence type is present, so an unrelated user `css()` is not picked up.
    let tw_css = if sequence_string_types.is_empty() {
        String::new()
    } else {
        css_constant(member_src).unwrap_or_default()
    };

    // Memory notes only when the checks ran and found no error: the analysis
    // reads a program the checker approved. `remapped` counts too, because a
    // type error at an origin-governed line leaves `diags` empty, and
    // the lowering refuses a body typed `<type error>`.
    let errored =
        |d: &crate::diagnostics::Diagnostic| d.severity == crate::diagnostics::Severity::Error;
    let clean = !diags.iter().any(errored) && !remapped.iter().any(errored);
    let memory = match &checked {
        Some(prog) if clean => memory_notes(prog, &memory),
        _ => Vec::new(),
    };
    let module_hashes = (checked.as_ref())
        .map(|p| p.module_hashes.clone())
        .unwrap_or_default();
    if !clean {
        cost.clear();
    }

    Analysis {
        diagnostics: diags,
        memory,
        cost,
        module_hashes,
        symbols,
        tokens: tok_info,
        locals,
        names,
        decl_lines,
        fn_lines,
        impl_members,
        protocol_members,
        type_param_bounds,
        record_fields,
        finite_string_types,
        fn_param_types,
        sequence_string_types,
        tw_css,
        namespaces,
        origins,
        remapped,
        symbol_maps: origin_index.all,
        spellings,
    }
}

/// [`Analysis::names`] from the bindings and the check's record of uses. A use
/// of a binding the index skips (a `test` body's) gets no row.
fn name_index(
    locals: &[LocalBinding],
    uses: checker::Uses,
) -> HashMap<(usize, usize), Option<usize>> {
    let at: HashMap<(usize, usize), usize> = (locals.iter().enumerate())
        .map(|(i, b)| ((b.line, b.col), i))
        .collect();
    let mut out: HashMap<_, _> = (uses.into_iter())
        .filter_map(|(k, to)| match to {
            Some(p) => Some((k, Some(*at.get(&p)?))),
            None => Some((k, None)),
        })
        .collect();
    out.extend(at.into_iter().map(|(k, i)| (k, Some(i))));
    out
}

/// Every `let` in the root module, with what the ownership analysis decided.
/// A function with no `module` tag belongs to this document, the filter
/// the hover reads.
fn memory_notes(
    program: &crate::ast::Program,
    memory: &HashMap<crate::ast::FnId, Vec<crate::own::MemoryRow>>,
) -> Vec<MemoryNote> {
    let mut out = Vec::new();
    for (i, f) in program.functions.iter().enumerate() {
        if f.module.is_some() || f.is_extern {
            continue;
        }
        let Some(notes) = memory.get(&crate::ast::FnId::nth(i)) else {
            continue;
        };
        for n in notes {
            // A binding whose type owns no heap has nothing to reclaim, so the
            // hover says nothing about it.
            if matches!(n.bucket, crate::own::Bucket::Leaked { heap: false, .. }) {
                continue;
            }
            out.push(MemoryNote {
                name: n.name.clone(),
                line: n.line,
                text: n.text.clone(),
                last_use: n.last_use,
                moved_into: n.moved_into.clone(),
            });
        }
    }
    out
}

/// An `Analysis` with everything but `diagnostics` empty (a lex failure).
fn empty_analysis(diagnostics: Vec<Diagnostic>) -> Analysis {
    Analysis {
        diagnostics,
        symbols: Vec::new(),
        tokens: Vec::new(),
        locals: Vec::new(),
        names: HashMap::new(),
        decl_lines: Vec::new(),
        fn_lines: Vec::new(),
        impl_members: Vec::new(),
        protocol_members: Vec::new(),
        type_param_bounds: Vec::new(),
        record_fields: Vec::new(),
        finite_string_types: Vec::new(),
        fn_param_types: Vec::new(),
        sequence_string_types: Vec::new(),
        tw_css: String::new(),
        namespaces: Vec::new(),
        origins: crate::origin::OriginMaps::default(),
        remapped: Vec::new(),
        symbol_maps: Vec::new(),
        memory: Vec::new(),
        cost: Vec::new(),
        module_hashes: Default::default(),
        spellings: Default::default(),
    }
}

/// The source text of a keyword or operator token, or `None` for an identifier
/// or a literal. Reads [`lexer::token_name_and_text`], the table the `lex()`
/// builtin reads.
fn keyword_text(t: &Tok) -> Option<String> {
    match crate::lexer::token_name_and_text(t) {
        (kind, text) if kind == "keyword" || kind == "punct" => Some(text),
        _ => None,
    }
}

/// The text inside each backtick-quoted span of `msg`, in order:
/// `` `match` is missing variant `B` `` gives `["match", "B"]`. The first one
/// on the diagnostic's line is the pin target.
fn backtick_tokens(msg: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = msg;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        if let Some(close) = rest.find('`') {
            out.push(&rest[..close]);
            rest = &rest[close + 1..];
        } else {
            break;
        }
    }
    out
}

/// Pins line-only checker and move-check diagnostics (`col == 0`) to the token
/// they are about, so the LSP does not squiggle the whole line.
///
/// Nearly every such message backtick-quotes its keyword or name. For each
/// quoted token, this looks for its column on the diagnostic's line: first
/// among identifiers, then among keywords and operators, then the root of a
/// quoted path (`b.xs[0]`). With no match the diagnostic stays line-only. Only
/// positions change, never message text.
fn pin_diagnostics(
    diags: &mut [Diagnostic],
    kw_cols: &std::collections::HashMap<usize, std::collections::HashMap<String, (usize, usize)>>,
    tok_info: &[TokenInfo],
) {
    for d in diags.iter_mut() {
        // Lex and parse diagnostics carry their own column.
        if d.col != 0 {
            continue;
        }
        for target in backtick_tokens(&d.message) {
            // Identifier path: a user name on this line.
            if let Some(t) = tok_info
                .iter()
                .find(|t| t.line == d.line && t.text == target)
            {
                d.col = t.col;
                d.end_col = t.end_col;
                break;
            }
            // Keyword or operator path. Reserved words are never identifiers, so
            // the identifier path cannot have matched them.
            if let Some(&(col, end_col)) = kw_cols.get(&d.line).and_then(|kws| kws.get(target)) {
                d.col = col;
                d.end_col = end_col;
                break;
            }
            // A path is not a token. The kernel words its refusals about places,
            // so the pin is the place's root, which is a token on the line.
            let root = &target[..target.find(['.', '[']).unwrap_or(target.len())];
            if root != target && !root.is_empty() {
                if let Some(t) = tok_info.iter().find(|t| t.line == d.line && t.text == root) {
                    d.col = t.col;
                    d.end_col = t.end_col;
                    break;
                }
            }
        }
    }
}

/// What an identifier token names, in the one order hover, colour and
/// references read: a local binding ([`local_at`]), a namespace's member, a
/// member the caller finds, a namespace, a top-level symbol. Builtins come
/// after, in each caller.
enum Named<'a, M> {
    Local(&'a LocalBinding),
    /// `ns.member`, after an unshadowed namespace. Before top-level symbols, so
    /// a same-named declaration does not capture a qualified member.
    NsMember(&'a str, &'a Symbol),
    Member(M),
    Namespace(&'a NamespaceInfo),
    Symbol(&'a Symbol),
}

/// The first [`Named`] answer for `tok`. A member position (`recv.tok`) names
/// a member, never a local. `member` is the caller's member rule, asked after
/// the namespace's members.
fn resolve_token<'a, M>(
    analysis: &'a Analysis,
    tok: &TokenInfo,
    member: impl FnOnce() -> Option<M>,
) -> Option<Named<'a, M>> {
    if !is_member_position(analysis, tok) {
        if let Some(b) = local_at(analysis, tok) {
            return Some(Named::Local(b));
        }
    }
    if let Some(recv) = receiver_before_dot(analysis, tok.line, tok.col) {
        let ns = (analysis.namespaces.iter()).find(|n| n.name == recv.text);
        if let (None, Some(ns)) = (local_at(analysis, recv), ns) {
            if let Some(m) = ns.members.iter().find(|m| m.name == tok.text) {
                return Some(Named::NsMember(&ns.name, m));
            }
        }
    }
    if let Some(m) = member() {
        return Some(Named::Member(m));
    }
    if let Some(ns) = analysis.namespaces.iter().find(|n| n.name == tok.text) {
        return Some(Named::Namespace(ns));
    }
    top_symbol(analysis, &tok.text).map(Named::Symbol)
}

/// The top-level symbol named `name`. One of the open document (`file: None`)
/// shadows an imported one; among candidates the latest declaration wins.
fn top_symbol<'a>(analysis: &'a Analysis, name: &str) -> Option<&'a Symbol> {
    (analysis.symbols.iter())
        .filter(|s| s.name == name)
        .max_by_key(|s| (s.file.is_none(), s.line))
}

/// Resolves a 1-based `(line, col)` cursor to the declaration it names, in
/// [`resolve_token`]'s order. Its member is the `.`-completion entry of that
/// name on a typed receiver, so hover and completion agree. Then come
/// builtins, which hover with `definition: false` because they have no source.
pub fn resolve(analysis: &Analysis, line: usize, col: usize) -> Option<Resolution> {
    // The identifier token covering the cursor (`col` in `[col, end_col)`).
    let tok = analysis
        .tokens
        .iter()
        .find(|t| t.line == line && col >= t.col && col < t.end_col)?;
    let member = || {
        if !is_member_position(analysis, tok) {
            return None;
        }
        let c = (member_completions(analysis, tok.line, tok.col).into_iter())
            .find(|c| c.label == tok.text)?;
        // A user protocol or impl method is an indexed symbol, so it keeps its
        // declaration site. A field or builtin method hovers without a jump.
        let decl = (analysis.symbols.iter())
            .filter(|s| s.name == c.label && s.kind == SymbolKind::Method)
            .max_by_key(|s| (s.file.is_none(), s.line));
        Some((c, decl))
    };
    let at = |s: &Symbol, h: Hover| Resolution {
        name: s.name.clone(),
        kind: s.kind,
        target_line: s.line,
        target_col: s.col,
        target_end_col: s.end_col,
        target_file: s.file.clone(),
        hover: h.text,
        signature: h.signature,
        doc: h.doc,
        definition: true,
    };
    let nowhere = |name: &str, kind, h: Hover| Resolution {
        name: name.to_string(),
        kind,
        target_line: 0,
        target_col: 0,
        target_end_col: 0,
        target_file: None,
        hover: h.text,
        signature: h.signature,
        doc: h.doc,
        definition: false,
    };
    match resolve_token(analysis, tok, member) {
        Some(Named::Local(b)) => Some(local_resolution(analysis, b)),
        Some(Named::NsMember(ns, m)) => {
            let via = format!("— via namespace `{ns}`");
            let hover = hover_of(&m.detail, &[doc_piece(&m.doc), Some(&via)]);
            Some(Resolution {
                definition: m.file.is_some(),
                ..at(m, hover)
            })
        }
        Some(Named::Member((c, decl))) => {
            let doc = c.doc.clone().or_else(|| decl.and_then(|d| d.doc.clone()));
            let hover = hover_of(&c.detail, &[doc_piece(&doc)]);
            Some(match decl {
                Some(d) => Resolution {
                    name: c.label,
                    kind: c.kind,
                    ..at(d, hover)
                },
                None => nowhere(&c.label, c.kind, hover),
            })
        }
        // The namespace binding itself, a compile-time name, not a value.
        Some(Named::Namespace(ns)) => Some(nowhere(
            &ns.name,
            SymbolKind::Type,
            hover_of(
                &format!(
                    "namespace `{}` — {} exported member(s) (a compile-time name, not a value)",
                    ns.name,
                    ns.members.len()
                ),
                &[],
            ),
        )),
        Some(Named::Symbol(s)) => Some(at(s, hover_of(&s.detail, &[doc_piece(&s.doc)]))),
        // A built-in method or function name (`push`, `info`, `len`), then the
        // ambient `Result` and `Option` and their constructors, imported or not.
        None => match builtin_method(&tok.text) {
            Some(b) => Some(nowhere(b.name, SymbolKind::Method, hover_of(b.detail, &[]))),
            None => builtin_type_or_ctor(&tok.text)
                .map(|(kind, hover)| nowhere(&tok.text, kind, hover_of(&hover, &[]))),
        },
    }
}

/// The ambient `Result` and `Option` types and their constructors, with their
/// module and hover text.
///
/// The one table: hover, completion, `CONSTRUCTOR_BUILTINS` and
/// `loader::builtin_alias_exports` read it. The `SymbolKind` tells a type from
/// a constructor.
pub(crate) static BUILTIN_TYPES_AND_CTORS: &[(&str, &str, SymbolKind, &str)] = &[
    ("Result", "std/result", SymbolKind::Type, "Result<T, E> — the builtin result type (`Ok(T)` | `Err(E)`). Spelled explicitly by `import { Result, Ok, Err } from \"std/result\"`."),
    ("Ok", "std/result", SymbolKind::Variant, "Ok(value: T) -> Result<T, E> — the success variant of the builtin `Result`."),
    ("Err", "std/result", SymbolKind::Variant, "Err(error: E) -> Result<T, E> — the failure variant of the builtin `Result`."),
    ("Option", "std/option", SymbolKind::Type, "Option<T> — the builtin option type (`Some(T)` | `None`). Spelled explicitly by `import { Option, Some, None } from \"std/option\"`."),
    ("Some", "std/option", SymbolKind::Variant, "Some(value: T) -> Option<T> — the present variant of the builtin `Option`."),
    ("None", "std/option", SymbolKind::Variant, "None -> Option<T> — the absent variant of the builtin `Option`."),
];

fn builtin_type_or_ctor(name: &str) -> Option<(SymbolKind, String)> {
    BUILTIN_TYPES_AND_CTORS
        .iter()
        .find(|(n, _, _, _)| *n == name)
        .map(|(_, _, kind, detail)| (*kind, detail.to_string()))
}

/// The function whose line range contains `cursor_line`, if any. The range is
/// `[fn_line, next_decl_line)`, so a cursor in a later type or protocol decl is
/// not attributed to the preceding function.
fn enclosing_fn_line(analysis: &Analysis, cursor_line: usize) -> Option<usize> {
    // The segment the cursor falls in: the greatest decl line at or before it.
    let seg_start = analysis
        .decl_lines
        .iter()
        .rev()
        .find(|&&l| l <= cursor_line)
        .copied()?;
    // The segment is a function exactly when its decl line is a function line.
    analysis
        .fn_lines
        .iter()
        .any(|&l| l == seg_start)
        .then_some(seg_start)
}

/// Whether the 1-based `(line, col)` cursor sits at module scope in `source`,
/// outside every brace-delimited body.
///
/// Contract members `head` and `data` complete only where a declaration can go.
/// The answer is the brace depth of the tokens before the cursor:
/// one lex, no parse, so it works per keystroke on a buffer that does not parse.
pub fn at_module_scope(source: &str, line: usize, col: usize) -> bool {
    let Ok(tokens) = lexer::lex(source) else {
        // A buffer that does not lex has no reliable structure; offer nothing extra.
        return false;
    };
    let mut depth: i64 = 0;
    for t in &tokens {
        if (t.line, t.col) >= (line, col) {
            break;
        }
        match t.tok {
            Tok::LBrace => depth += 1,
            Tok::RBrace => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

/// All top-level symbols as completion items. The client filters by the prefix
/// typed; there is no scope-aware filtering.
pub fn completions(analysis: &Analysis) -> Vec<Completion> {
    let mut out: Vec<Completion> = analysis
        .symbols
        .iter()
        .map(|s| Completion {
            label: s.name.clone(),
            kind: s.kind,
            detail: s.detail.clone(),
            doc: s.doc.clone(),
        })
        .collect();
    // The ambient `Result` and `Option` builtins and constructors are always in
    // scope.
    for (name, _, _, _) in BUILTIN_TYPES_AND_CTORS {
        if let Some((kind, detail)) = builtin_type_or_ctor(name) {
            out.push(Completion {
                label: name.to_string(),
                kind,
                detail,
                doc: None,
            });
        }
    }
    out
}

/// Completions for a `.foo` member access at a cursor on or after a `.`.
///
/// The receiver is the identifier before the dot. A namespace offers its
/// exports. A typed local offers the builtin methods of its type, the `length`
/// or `byteLength` field, a record's fields, the methods of every matching
/// `impl P for T`, and for a bounded generic receiver each bound protocol's
/// methods, as the checker dispatches. Empty when the receiver cannot be typed:
/// not a simple identifier, not in the local index, or of unknown type.
pub fn member_completions(analysis: &Analysis, line: usize, col: usize) -> Vec<Completion> {
    // A receiver naming an unshadowed namespace offers that module's exports.
    if let Some(recv) = receiver_before_dot(analysis, line, col) {
        if local_at(analysis, recv).is_none() {
            let recv = &recv.text;
            if let Some(nsi) = analysis.namespaces.iter().find(|n| &n.name == recv) {
                return nsi
                    .members
                    .iter()
                    .map(|s| Completion {
                        label: s.name.clone(),
                        kind: s.kind,
                        detail: format!("{}\n\n— via namespace `{}`", s.detail, recv),
                        doc: s.doc.clone(),
                    })
                    .collect();
            }
        }
    }
    // A receiver that cannot be typed has nothing to suggest.
    let ty = match resolve_receiver_type(analysis, line, col) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut out: Vec<Completion> = builtin_methods_for(&ty)
        .iter()
        .map(|b| Completion {
            label: b.name.to_string(),
            kind: SymbolKind::Method,
            detail: b.detail.to_string(),
            doc: None,
        })
        .collect();
    // `arr.length`: the read-only element-count field.
    if ty.is_seq() {
        out.push(Completion {
            label: "length".to_string(),
            kind: SymbolKind::Field,
            detail: "length: Int64 — element count (read-only)".to_string(),
            doc: None,
        });
    }
    // `map.length`: the entry-count field.
    if matches!(ty, Type::Map(..)) {
        out.push(Completion {
            label: "length".to_string(),
            kind: SymbolKind::Field,
            detail: "length: Int64 — entry count (read-only)".to_string(),
            doc: None,
        });
    }
    // `str.byteLength`: the UTF-8 byte count. `String` has no
    // `.length`; `.charCount()` counts scalars.
    if matches!(ty, Type::Str) {
        out.push(Completion {
            label: "byteLength".to_string(),
            kind: SymbolKind::Field,
            detail: "byteLength: Int64 — UTF-8 byte count (O(1), read-only)".to_string(),
            doc: None,
        });
    }
    // A named record receiver offers its declaration's fields; an inline
    // structural receiver offers its own.
    match &ty {
        Type::Named(n) => {
            for (tn, c) in &analysis.record_fields {
                if tn == n {
                    out.push(c.clone());
                }
            }
        }
        Type::Record(fields) => {
            for f in fields {
                out.push(Completion {
                    label: f.name.clone(),
                    kind: SymbolKind::Field,
                    detail: format!("{}: {}", f.name, type_to_string(&f.ty, &analysis.spellings)),
                    doc: None,
                });
            }
        }
        _ => {}
    }
    // Concrete receiver: every `impl P for T` whose `T` is the receiver's type
    // (`n: Int64` with `impl Show for Int64` offers `n.show`).
    for (t, c) in &analysis.impl_members {
        if *t == ty {
            out.push(c.clone());
        }
    }
    // Bounded generic receiver: `x: T` in `fn f<T: Show>` offers `Show`'s
    // methods. `Named` is matched with `Param` because the two render alike.
    if let Type::Named(n) | Type::Param(n) = &ty {
        if let Some(fn_line) = enclosing_fn_line(analysis, line) {
            for (fl, tp, bounds) in &analysis.type_param_bounds {
                if *fl == fn_line && tp == n {
                    for b in bounds {
                        for (pn, c) in &analysis.protocol_members {
                            if pn == b {
                                out.push(c.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Completions for the content of a string literal whose expected type is a
/// finite validated string type: `t("` offers every key. Empty when
/// the cursor is not in such a string, or the type is past the cap or infinite.
///
/// The expected type comes from the token stream: a direct argument of a call
/// to a known top-level function, by comma position, or the initializer of an
/// annotated `let name: T = "..."`.
pub fn string_literal_completions(
    analysis: &Analysis,
    source: &str,
    line: usize,
    col: usize,
) -> Vec<Completion> {
    let Some(ty_name) = expected_string_type(analysis, source, line, col) else {
        return Vec::new();
    };
    let Some((_, domain)) = analysis
        .finite_string_types
        .iter()
        .find(|(n, _)| n == &ty_name)
    else {
        return Vec::new();
    };
    domain
        .iter()
        .map(|k| Completion {
            label: k.clone(),
            kind: SymbolKind::Variant,
            detail: format!("{ty_name} — a key of this finite string type"),
            doc: None,
        })
        .collect()
}

/// Completions for a `cls("...")` string whose expected type is a sequence
/// string type such as `Tw`: the theme's whole token alphabet. The
/// caller filters by the token under the cursor. `None` when the string is not
/// such an argument; the caller then tries [`string_literal_completions`].
pub fn class_completions(
    analysis: &Analysis,
    source: &str,
    line: usize,
    col: usize,
) -> Option<Vec<Completion>> {
    // Gate on the `cls(...)` call, the std/tw bridge (`theme.cls` in `.vyrn`,
    // `vyxTheme.cls` in a themed `.vyx`), not on the parameter's declared type:
    // a linked module lowers the `Tw` alias to its `String` base.
    if !cls_call_arg(source, line, col) {
        return None;
    }
    let (ty_name, alphabet) = sequence_type_at(analysis, source, line, col)?;
    Some(
        alphabet
            .iter()
            .map(|c| Completion {
                label: c.clone(),
                kind: SymbolKind::Variant,
                detail: format!("{ty_name} — theme utility/safelist class"),
                doc: None,
            })
            .collect(),
    )
}

/// Whether the string literal under the cursor is a direct argument of a
/// `cls(...)` call, bare or as the last segment of `ns.cls`.
fn cls_call_arg(source: &str, line: usize, col: usize) -> bool {
    let Ok(toks) = lexer::lex(source) else {
        return false;
    };
    let Some(str_idx) = toks.iter().position(|t| {
        if t.line != line {
            return false;
        }
        if let Tok::Str(s) = &t.tok {
            let start = t.col;
            let end = t.col + s.chars().count() + 2;
            col >= start && col <= end
        } else {
            false
        }
    }) else {
        return false;
    };
    let mut depth = 0i32;
    let mut i = str_idx;
    while i > 0 {
        i -= 1;
        match &toks[i].tok {
            Tok::RParen => depth += 1,
            Tok::LParen => {
                if depth == 0 {
                    return i >= 1 && matches!(&toks[i - 1].tok, Tok::Ident(c) if c == "cls");
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    false
}

/// The sequence string type at the cursor's `cls(...)` argument: the one named
/// by the call's declared parameter type when that resolves, so two themes do
/// not blur, else the first enumerated theme. A linked module lowers `Tw` to
/// `String`, so the fallback is often the only answer.
fn sequence_type_at(
    analysis: &Analysis,
    source: &str,
    line: usize,
    col: usize,
) -> Option<(String, Vec<String>)> {
    let declared = expected_string_type(analysis, source, line, col);
    analysis
        .sequence_string_types
        .iter()
        .find(|(n, _)| Some(n) == declared.as_ref())
        .or_else(|| analysis.sequence_string_types.first())
        .cloned()
}

/// The hover of a class token in a `cls("...")` string.
pub struct ClassHover {
    pub text: String,
    /// The class, when the theme safelists it without a rule of its own, so
    /// the host may add the app's rules.
    pub safelisted: Option<String>,
}

/// Hover for the class token under the cursor in a `cls("...")` string: the
/// CSS rule `std/tw`'s `css()` emits for a utility, or "safelisted" for a
/// safelist entry. `None` when the cursor is not on a class token of a linked
/// theme.
pub fn class_token_hover(
    analysis: &Analysis,
    source: &str,
    line: usize,
    col: usize,
) -> Option<ClassHover> {
    if !cls_call_arg(source, line, col) {
        return None;
    }
    let (ty_name, alphabet) = sequence_type_at(analysis, source, line, col)?;
    let token = class_token_at(source, line, col)?;
    if !alphabet.iter().any(|c| c == &token) {
        return None;
    }
    Some(match css_rule_for(&analysis.tw_css, &token) {
        Some(rule) => ClassHover {
            text: format!("**`{token}`** — `{ty_name}` utility class\n\n```css\n{rule}\n```"),
            safelisted: None,
        },
        None => ClassHover {
            text: format!("**`{token}`** — safelisted (app-styled)"),
            safelisted: Some(token),
        },
    })
}

/// The whitespace-delimited token containing the 1-based cursor `col` on `line`:
/// the class word the user is typing.
fn class_token_at(source: &str, line: usize, col: usize) -> Option<String> {
    let line_text = source.lines().nth(line.saturating_sub(1))?;
    let chars: Vec<char> = line_text.chars().collect();
    // 0-based index of the char the cursor sits just after.
    let mut start = col.saturating_sub(1);
    if start > chars.len() {
        start = chars.len();
    }
    let is_word = |c: char| !c.is_whitespace() && c != '"' && c != '\'';
    // Walk left to the token start.
    let mut lo = start;
    while lo > 0 && is_word(chars[lo - 1]) {
        lo -= 1;
    }
    // Walk right to the token end.
    let mut hi = start;
    while hi < chars.len() && is_word(chars[hi]) {
        hi += 1;
    }
    if lo == hi {
        return None;
    }
    Some(chars[lo..hi].iter().collect())
}

/// The CSS rule `css()` emits for `class`, from the captured stylesheet, as the
/// whole `.<selector> {...}` text; `None` for a safelisted name.
fn css_rule_for(css: &str, class: &str) -> Option<String> {
    if css.is_empty() {
        return None;
    }
    let escaped: String = class
        .chars()
        .flat_map(|c| if c == ':' { vec!['\\', ':'] } else { vec![c] })
        .collect();
    // A variant rule escapes the selector's `:` (`.md\:hover\:bg-x :hover {...}`),
    // so match the escaped selector with its leading `.`.
    let selector = format!(".{escaped}");
    // Only where the next char does not extend the class: `.p-2` is not `.p-20`.
    let mut from = 0usize;
    while let Some(rel) = css[from..].find(&selector) {
        let at = from + rel;
        let after = css[at + selector.len()..].chars().next();
        if !matches!(after, Some(c) if c.is_ascii_alphanumeric() || c == '-') {
            let open = css[at..].find('{')?;
            let close = css[at + open..].find('}')?;
            return Some(css[at..at + open + close + 1].trim().to_string());
        }
        from = at + selector.len();
    }
    None
}

/// The string a zero-parameter `css()` returns (`std/tw`'s baked stylesheet), if
/// the program has one. Its body is a single `return "<literal>"`.
fn css_constant(program: &ast::Program) -> Option<String> {
    let f = program
        .functions
        .iter()
        .find(|f| f.name == "css" && f.params.is_empty())?;
    for stmt in &f.body.stmts {
        if let Stmt::Return {
            value: Some(Expr::Str(s, _)),
            ..
        } = stmt
        {
            return Some(s.clone());
        }
    }
    None
}

/// The validated string type expected at the string literal under the cursor,
/// when it is a call argument or an annotated `let`. Re-lexes `source`, because
/// the cached index lacks the literal, `(`, `,`, `:` and `let` tokens.
fn expected_string_type(
    analysis: &Analysis,
    source: &str,
    line: usize,
    col: usize,
) -> Option<String> {
    let toks = lexer::lex(source).ok()?;
    // The string-literal token containing the cursor. Its span is the rendered
    // length plus two quotes, exact for unescaped ASCII keys.
    let str_idx = toks.iter().position(|t| {
        if t.line != line {
            return false;
        }
        if let Tok::Str(s) = &t.tok {
            let start = t.col;
            let end = t.col + s.chars().count() + 2; // + the two quotes
            col >= start && col <= end
        } else {
            false
        }
    })?;

    // Case A: `let name: T = "..."`, so the three tokens before the string are
    // `: T =`.
    if str_idx >= 3 && toks[str_idx - 1].tok == Tok::Eq && toks[str_idx - 3].tok == Tok::Colon {
        if let Tok::Ident(tn) = &toks[str_idx - 2].tok {
            return Some(tn.clone());
        }
    }

    // Case B: a call argument. Scan left to the enclosing `(`, counting top-level
    // commas for the argument index; the callee is the identifier before `(`.
    let mut depth = 0i32;
    let mut arg_index = 0usize;
    let mut i = str_idx;
    while i > 0 {
        i -= 1;
        match &toks[i].tok {
            Tok::RParen => depth += 1,
            Tok::LParen => {
                if depth == 0 {
                    if i >= 1 {
                        if let Tok::Ident(callee) = &toks[i - 1].tok {
                            let params = analysis
                                .fn_param_types
                                .iter()
                                .find(|(n, _)| n == callee)
                                .map(|(_, p)| p)?;
                            if let Some(Type::Named(tn)) = params.get(arg_index) {
                                return Some(tn.clone());
                            }
                        }
                    }
                    return None;
                }
                depth -= 1;
            }
            Tok::Comma if depth == 0 => arg_index += 1,
            _ => {}
        }
    }
    None
}

/// The identifier before the nearest dot at or before the cursor on `line`: the
/// receiver of a `.foo` access (namespaces use it too).
fn receiver_before_dot(analysis: &Analysis, line: usize, col: usize) -> Option<&TokenInfo> {
    let dot = analysis
        .tokens
        .iter()
        .filter(|t| t.line == line && t.text == "." && t.col <= col)
        .max_by_key(|t| t.col)?;
    let recv = analysis
        .tokens
        .iter()
        .filter(|t| t.line == line && t.text != "." && t.end_col <= dot.col)
        .max_by_key(|t| t.end_col)?;
    Some(recv)
}

/// The type of the receiver before the dot at the cursor. Only a local is a
/// typed receiver here.
fn resolve_receiver_type(analysis: &Analysis, line: usize, col: usize) -> Option<Type> {
    local_at(analysis, receiver_before_dot(analysis, line, col)?)?
        .ty
        .clone()
}

/// The local binding the identifier `tok` names. The check's record answers
/// ([`Analysis::names`]). A token it has no row for, in a buffer that does
/// not parse or a statement that did not type, takes the latest same-named
/// binding at or before its line in its function, blind to where a block ends.
fn local_at<'a>(analysis: &'a Analysis, tok: &TokenInfo) -> Option<&'a LocalBinding> {
    if let Some(named) = analysis.names.get(&(tok.line, tok.col)) {
        return named.map(|i| &analysis.locals[i]);
    }
    let fn_line = enclosing_fn_line(analysis, tok.line)?;
    (analysis.locals.iter())
        .filter(|b| b.fn_line == fn_line && b.name == tok.text && b.line <= tok.line)
        .max_by_key(|b| b.line)
}

/// Every top-level declaration line, sorted. It bounds a variant name search to
/// its `type` declaration and a function's range to the next declaration.
fn decl_lines(program: &ast::Program) -> Vec<usize> {
    let mut v: Vec<usize> = Vec::new();
    for f in &program.functions {
        v.push(f.line);
    }
    for t in &program.type_decls {
        v.push(t.line);
    }
    for p in &program.protocols {
        v.push(p.line);
    }
    for i in &program.impls {
        v.push(i.line);
    }
    for g in &program.globals {
        v.push(g.line);
    }
    v.sort_unstable();
    v
}

/// The subset of [`decl_lines`] that are function declarations (functions and
/// impl methods). A protocol method has no body, so it holds no locals.
fn fn_lines(program: &ast::Program) -> Vec<usize> {
    let mut v: Vec<usize> = Vec::new();
    for f in &program.functions {
        v.push(f.line);
    }
    for imp in &program.impls {
        for m in &imp.methods {
            v.push(m.line);
        }
    }
    v.sort_unstable();
    v
}

/// The column range of the first identifier `name` on `line`; `(0, 0)` if none.
fn name_col_on_line(tok_info: &[TokenInfo], name: &str, line: usize) -> (usize, usize) {
    tok_info
        .iter()
        .find(|t| t.text == name && t.line == line)
        .map(|t| (t.col, t.end_col))
        .unwrap_or((0, 0))
}

/// What a [`Decl`] declares, so an index takes the kinds it shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Of {
    Function,
    Global,
    ImplMethod,
    Protocol,
    Type,
    Test,
    Bench,
}

/// One declaration as a [`Symbol`] at column 0 with no `file`, and the
/// members an index takes with it: a protocol's methods or an enum's
/// variants. A variant's line is its type's.
struct Decl {
    of: Of,
    module: Option<String>,
    sym: Symbol,
    members: Vec<Symbol>,
}

/// The declarations of `program` that `keep` admits, given their kind, name,
/// module and `export`, in outline order: functions, module state, impl
/// methods, protocols, types, tests, benches. A parser-injected type (line 0)
/// and a synthetic refinement type (`User.age`) are not declarations. `keep`
/// runs before any detail is rendered, so an index pays only for what it shows.
fn decl_symbols(
    program: &ast::Program,
    sp: &Spellings,
    keep: impl Fn(Of, &str, &Option<String>, bool) -> bool,
) -> Vec<Decl> {
    let sym = |name: &str, kind, line, detail, doc: Option<String>| Symbol {
        name: name.to_string(),
        kind,
        line,
        col: 0,
        end_col: 0,
        detail,
        doc,
        file: None,
        mutable: false,
    };
    let one = |of, module: &Option<String>, sym| Decl {
        of,
        module: module.clone(),
        sym,
        members: Vec::new(),
    };
    let mut out = Vec::new();
    for f in &program.functions {
        if keep(Of::Function, &f.name, &f.module, f.exported) {
            let detail = function_detail(f, sp);
            out.push(one(
                Of::Function,
                &f.module,
                sym(&f.name, SymbolKind::Function, f.line, detail, f.doc.clone()),
            ));
        }
    }
    for g in &program.globals {
        if keep(Of::Global, &g.name, &g.module, false) {
            let mut s = sym(
                &g.name,
                SymbolKind::Global,
                g.line,
                global_detail(g, sp),
                g.doc.clone(),
            );
            s.mutable = g.mutable;
            out.push(one(Of::Global, &g.module, s));
        }
    }
    for imp in &program.impls {
        for m in imp
            .methods
            .iter()
            .filter(|m| keep(Of::ImplMethod, &m.name, &m.module, false))
        {
            let doc = (m.doc.clone())
                .or_else(|| signature_doc(&program.protocols, &imp.protocol, &m.name));
            let detail = function_detail(m, sp);
            out.push(one(
                Of::ImplMethod,
                &m.module,
                sym(&m.name, SymbolKind::Method, m.line, detail, doc),
            ));
        }
    }
    for p in &program.protocols {
        if keep(Of::Protocol, &p.name, &p.module, p.exported) {
            let detail = protocol_detail(p, sp);
            let mut d = one(
                Of::Protocol,
                &p.module,
                sym(&p.name, SymbolKind::Type, p.line, detail, p.doc.clone()),
            );
            d.members = (p.methods.iter())
                .map(|m| {
                    sym(
                        &m.name,
                        SymbolKind::Method,
                        m.line,
                        method_sig_detail(m, sp),
                        m.doc.clone(),
                    )
                })
                .collect();
            out.push(d);
        }
    }
    for t in &program.type_decls {
        if t.line == 0
            || ast::is_synthetic(&t.name)
            || !keep(Of::Type, &t.name, &t.module, t.exported)
        {
            continue;
        }
        let detail = type_decl_detail(t, &program.type_decls, sp);
        let mut d = one(
            Of::Type,
            &t.module,
            sym(&t.name, SymbolKind::Type, t.line, detail, t.doc.clone()),
        );
        d.members = (crate::types::declared_variants(&t.base)
            .into_iter()
            .flatten())
        .map(|v| {
            sym(
                &v.name,
                SymbolKind::Variant,
                t.line,
                variant_detail(&t.name, v, sp),
                None,
            )
        })
        .collect();
        out.push(d);
    }
    for (of, word, blocks) in [
        (Of::Test, "test", &program.tests),
        (Of::Bench, "bench", &program.benches),
    ] {
        for b in blocks
            .iter()
            .filter(|b| keep(of, &b.name, &b.module, false))
        {
            let detail = format!("{word} {:?}", b.name);
            out.push(one(
                of,
                &b.module,
                sym(&b.name, SymbolKind::Method, b.line, detail, b.doc.clone()),
            ));
        }
    }
    out
}

/// The root's declarations, each at its name token. A variant is the first
/// token of its name between its type's line and the next declaration's. A
/// test or bench anchors at its keyword, because its name is a string.
fn index_symbols(
    program: &ast::Program,
    tok_info: &[TokenInfo],
    lines: &[usize],
    sp: &Spellings,
) -> Vec<Symbol> {
    let mut out = Vec::new();
    for d in decl_symbols(program, sp, |_, _, _, _| true) {
        let mut s = d.sym;
        let anchor = match d.of {
            Of::Test => "test",
            Of::Bench => "bench",
            _ => &s.name,
        };
        (s.col, s.end_col) = name_col_on_line(tok_info, anchor, s.line);
        let next = lines
            .iter()
            .find(|&&l| l > s.line)
            .copied()
            .unwrap_or(usize::MAX);
        out.push(s);
        for mut m in d.members {
            match m.kind {
                SymbolKind::Variant => {
                    let at = (tok_info.iter())
                        .find(|t| t.text == m.name && t.line >= m.line && t.line < next);
                    if let Some(t) = at {
                        (m.line, m.col, m.end_col) = (t.line, t.col, t.end_col);
                    }
                }
                _ => (m.col, m.end_col) = name_col_on_line(tok_info, &m.name, m.line),
            }
            out.push(m);
        }
    }
    out
}

/// Indexes the declarations the root imports, from the linked program. Only
/// names the root's imports bring into scope are indexed, with an imported
/// enum's variants and an imported protocol's methods. Columns are 0, since the
/// foreign file's tokens are not at hand; `file` names the source module.
/// `graph` is the load's, which resolves each root import to its module.
fn index_imported_symbols(
    root: &ast::Program,
    linked: &ast::Program,
    graph: &crate::loader::ModuleGraph,
    origins: &OriginIndex,
) -> Vec<Symbol> {
    let sp = &*linked.spellings;
    let Some((_, targets, _)) = graph.iter().find(|(k, _, _)| *k == sp.root) else {
        return Vec::new();
    };
    // Each imported (module, original name) mapped to the local name the root
    // uses (the alias, or the original). A symbol is keyed by the local name,
    // to line up with the root's tokens, and an alias notes its original in
    // hover. The module is in the key because the linker renames one of two
    // modules' declarations of one name, and either may be the root's.
    let mut imported: std::collections::HashMap<(&str, &str), &str> =
        std::collections::HashMap::new();
    for (imp, target) in root.imports.iter().zip(targets) {
        for n in &imp.names {
            imported.insert((target.as_str(), n.original.as_str()), n.local());
        }
    }
    if imported.is_empty() {
        return Vec::new();
    }
    let local_of = |module: &Option<String>, linked_name: &str| {
        let module = module.as_deref()?;
        imported.get(&(module, sp.written(linked_name))).copied()
    };
    let keep = |of, name: &str, module: &Option<String>, _| {
        matches!(of, Of::Function | Of::Protocol | Of::Type) && local_of(module, name).is_some()
    };
    let mut out = Vec::new();
    for d in decl_symbols(linked, sp, keep) {
        let mut s = d.sym;
        let local = local_of(&d.module, &s.name).expect("`keep` admitted it");
        let original = sp.written(&s.name).to_string();
        if local != original {
            s.detail = format!("{}\n\n— alias of `{original}`", s.detail);
        }
        s.name = local.to_string();
        s.file = d.module.clone();
        out.push(s);
        out.extend(d.members.into_iter().map(|m| Symbol {
            file: d.module.clone(),
            ..m
        }));
    }

    // A name imported from a generated module carries a banner as its file.
    // Where the module's map claims the symbol, it stands for a real
    // declaration, looked up by original name, since the map is keyed
    // by what the generator emitted.
    let original_of: std::collections::HashMap<&str, &str> = imported
        .iter()
        .map(|((_, orig), local)| (*local, *orig))
        .collect();
    for s in &mut out {
        if let Some(module) = s.file.clone() {
            let generated = original_of
                .get(s.name.as_str())
                .copied()
                .unwrap_or(s.name.as_str())
                .to_string();
            origins.apply(&module, &generated, s);
        }
    }

    out
}

/// Indexes the document's `import * as ns` bindings and the exported
/// declarations each exposes. Each target is parsed on its own, so members show
/// original names, not the loader's collision renames, with their real lines.
fn index_namespaces(
    graph: &crate::loader::ModuleGraph,
    root: &ast::Program,
    linker: Option<Linker<'_>>,
    origins: &OriginIndex,
) -> Vec<NamespaceInfo> {
    let Some((root_path, _, resolver)) = linker else {
        return Vec::new();
    };
    if !root.imports.iter().any(|i| i.namespace.is_some()) {
        return Vec::new();
    }
    // The root's resolved import targets in `root.imports` order, with each
    // module's generated source: a namespace may name a generated module whose
    // banner key no resolver can read. The graph is the one `analyze_inner`'s
    // load built; rebuilding it would run a second load per keystroke.
    let root_key = crate::loader::normalize(root_path);
    let Some((_, targets, _)) = graph.iter().find(|(k, _, _)| *k == root_key) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (imp, target) in root.imports.iter().zip(targets) {
        if let Some(ns) = &imp.namespace {
            let gen = graph
                .iter()
                .find(|(k, _, _)| k == target)
                .and_then(|(_, _, g)| g.as_deref());
            let mut members = namespace_members(target, resolver, gen);
            for m in &mut members {
                let generated = m.name.clone();
                origins.apply(target, &generated, m);
            }
            out.push(NamespaceInfo {
                name: ns.clone(),
                members,
            });
        }
    }
    out
}

/// The exported declarations of `target` as [`Symbol`]s, parsed from the
/// target's own source, so a collision rename in the linked program never shows.
/// An unreadable target without `gen_source` yields none.
///
/// `gen_source` is the generated text when `target` is a generated module. Its
/// members hover, with docs, but carry no `file`: there is nothing on disk to
/// jump to.
fn namespace_members(
    target: &str,
    resolver: &dyn crate::loader::ModuleResolver,
    gen_source: Option<&str>,
) -> Vec<Symbol> {
    let text = match gen_source {
        Some(g) => g.to_string(),
        None => match resolver.read(target) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        },
    };
    let file = gen_source.is_none().then(|| target.to_string());
    let Ok(tokens) = lexer::lex(&text) else {
        return Vec::new();
    };
    let (program, _errs) = parser::parse_accum(tokens);
    let exported = |of, _: &str, _: &Option<String>, exported| {
        exported && matches!(of, Of::Function | Of::Protocol | Of::Type)
    };
    let mut out = Vec::new();
    for d in decl_symbols(&program, &Spellings::default(), exported) {
        out.push(Symbol {
            file: file.clone(),
            ..d.sym
        });
        // A protocol's methods are not reached through the namespace.
        if d.of == Of::Type {
            out.extend(d.members.into_iter().map(|m| Symbol {
                file: file.clone(),
                ..m
            }));
        }
    }
    out
}

/// The symbol maps of every generated module the linked program reached,
/// plus the `///` on each declaration they name.
///
/// A generated stub has no source file or doc of its own; its "file" is a
/// banner. The map supplies both. Docs are read once per origin file: a
/// client's stubs come from a few api modules, and `client()` is server-blind,
/// so those modules are not in the linked program.
#[derive(Default)]
pub(crate) struct OriginIndex {
    /// Generated module banner to its mapped symbols, by generated name.
    maps: std::collections::HashMap<String, std::collections::HashMap<String, MappedSymbol>>,
    /// `(origin file, declaration name)` to its `///`.
    docs: std::collections::HashMap<(String, String), String>,
    /// Every mapped symbol, flattened, as [`Analysis::symbol_maps`] exposes it.
    all: Vec<MappedSymbol>,
}

impl OriginIndex {
    fn build(
        graph: &crate::loader::ModuleGraph,
        resolver: &dyn crate::loader::ModuleResolver,
    ) -> Self {
        let mut out = OriginIndex::default();
        for (key, _, gen) in graph {
            let Some(src) = gen else { continue };
            let syms = crate::symbolmap::read(src);
            if syms.is_empty() {
                continue;
            }
            let mut by_name = std::collections::HashMap::new();
            for s in syms {
                out.all.push(s.clone());
                by_name.insert(s.name.clone(), s);
            }
            out.maps.insert(key.clone(), by_name);
        }
        let mut files: Vec<&str> = out.all.iter().map(|s| s.file.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        for file in files {
            let Ok(text) = resolver.read(file) else {
                continue;
            };
            let Ok(tokens) = lexer::lex(&text) else {
                continue;
            };
            let (program, _) = parser::parse_accum(tokens);
            for f in &program.functions {
                if let Some(d) = &f.doc {
                    out.docs
                        .insert((file.to_string(), f.name.clone()), d.clone());
                }
            }
            for t in &program.type_decls {
                if let Some(d) = &t.doc {
                    out.docs
                        .insert((file.to_string(), t.name.clone()), d.clone());
                }
            }
        }
        out
    }

    fn get(&self, module: &str, name: &str) -> Option<&MappedSymbol> {
        self.maps.get(module)?.get(name)
    }

    /// Rewrites a generated module's [`Symbol`] to stand for the declaration it
    /// was generated from: the jump target, the borrowed doc, and the wire facts.
    fn apply(&self, module: &str, generated_name: &str, sym: &mut Symbol) {
        let Some(m) = self.get(module, generated_name) else {
            return;
        };
        sym.line = m.line;
        sym.col = m.col;
        sym.end_col = if m.col == 0 {
            0
        } else {
            m.col + m.decl.chars().count()
        };
        sym.file = Some(m.file.clone());
        // The note goes in the doc, not the detail, so a hover reads signature,
        // then the declaration's own doc, then where it is and what it is
        // mounted at.
        let borrowed = sym
            .doc
            .clone()
            .or_else(|| self.docs.get(&(m.file.clone(), m.decl.clone())).cloned());
        sym.doc = Some(match borrowed {
            Some(d) => format!("{}\n\n{}", d.trim_end(), origin_note(m)),
            None => origin_note(m),
        });
    }
}

/// The hover lines a mapped symbol adds: its derived wire facts, and where the
/// declaration it stands for is written.
pub(crate) fn origin_note(m: &MappedSymbol) -> String {
    let mut out = String::new();
    if let Some(route) = m.route_line() {
        out.push_str(&route);
        out.push_str("\n\n");
    }
    let at = if m.line == 0 {
        short_path(&m.file)
    } else {
        format!("{}:{}", short_path(&m.file), m.line)
    };
    out.push_str(&format!("— generated from `{}` in {at}", m.decl));
    out
}

/// The last three segments of a module path, so a hover fits on a line.
///
// ponytail: three segments fits `<app>/server/api/x.vyrn`; a project-relative
// path needs the project root threaded down here.
pub(crate) fn short_path(file: &str) -> String {
    let parts: Vec<&str> = file.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        return file.to_string();
    }
    format!("…/{}", parts[parts.len() - 3..].join("/"))
}

/// A hover, and the part of it the editor shows as code.
struct Hover {
    text: String,
    signature: Option<String>,
    doc: String,
}

/// A declaration's `///` doc (markdown, verbatim) as a hover paragraph; `None`
/// when it has none.
fn doc_piece(doc: &Option<String>) -> Option<&str> {
    doc.as_deref()
        .filter(|d| !d.trim().is_empty())
        .map(str::trim_end)
}

/// The hover text: `detail`, then each `tail` paragraph, blank-line separated.
/// The signature is the first paragraph of `detail` when it opens with a
/// declaration keyword, or, for a builtin's one-line detail, the part before
/// the em dash. Anything else has no signature.
fn hover_of(detail: &str, tail: &[Option<&str>]) -> Hover {
    const DECL: &[&str] = &[
        "fn ",
        "gen fn ",
        "mut fn ",
        "type ",
        "let ",
        "protocol ",
        "impl ",
        "contract ",
    ];
    let tail = || tail.iter().flatten().copied();
    let text = std::iter::once(detail)
        .chain(tail())
        .collect::<Vec<_>>()
        .join("\n\n");
    let (head, more) = match detail.split_once("\n\n") {
        Some((head, more)) => (head, Some(more)),
        None => (detail, None),
    };
    let (signature, lead) = match head.split_once(" — ") {
        _ if DECL.iter().any(|d| head.starts_with(d)) => (Some(head), None),
        Some((sig, doc)) if !sig.contains('\n') && sig.contains('(') && sig.contains("->") => {
            (Some(sig), Some(doc))
        }
        _ => (None, None),
    };
    let doc = match signature {
        Some(_) => (lead.into_iter().chain(more).chain(tail()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        None => text.clone(),
    };
    Hover {
        text,
        signature: signature.map(str::to_string),
        doc,
    }
}

/// The hover text: the signature, then the declaration's `///` doc.
fn with_doc(detail: &str, doc: &Option<String>) -> String {
    hover_of(detail, &[doc_piece(doc)]).text
}

/// The declaration of the user record or enum `name`, shown beneath a value's
/// hover: its one-line declaration and doc, never a recursive
/// expansion. `None` for a builtin or a type the root does not import.
fn type_structure(analysis: &Analysis, name: &str) -> Option<String> {
    let sym = analysis
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Type && s.name == name)
        .max_by_key(|s| (s.file.is_none(), s.line))?;
    // Only a structural declaration adds information a value's hover lacks; a
    // protocol is not a value's type.
    if !sym.detail.starts_with("type ") {
        return None;
    }
    Some(with_doc(&format!("```vyrn\n{}\n```", sym.detail), &sym.doc))
}

/// The user type a binding's hover expands: its named type, or the element type
/// of an array or map of one.
fn structural_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Named(n) => Some(n.as_str()),
        Type::Array(inner) => structural_name(inner),
        Type::ArrayN(inner, _) => structural_name(inner),
        Type::SmallArray(inner, _) => structural_name(inner),
        Type::Map(_, v) => structural_name(v),
        _ => None,
    }
}

fn local_resolution(analysis: &Analysis, b: &LocalBinding) -> Resolution {
    let structure =
        (b.ty.as_ref().and_then(structural_name)).and_then(|n| type_structure(analysis, n));
    // Bindings of one shape can have opposite memory outcomes, and the source
    // does not say which. Matched on the declaration line, so it is this binding.
    let memory = (analysis.memory.iter())
        .find(|m| m.name == b.name && m.line == b.line)
        .map(|m| format!("memory: {}", m.text));
    let h = hover_of(
        &local_detail(b, &analysis.spellings),
        &[structure.as_deref(), memory.as_deref()],
    );
    Resolution {
        name: b.name.clone(),
        kind: match b.kind {
            LocalKind::Param => SymbolKind::Param,
            LocalKind::Let { .. } | LocalKind::ForVar => SymbolKind::Local,
        },
        target_line: b.line,
        target_col: b.col,
        target_end_col: b.end_col,
        target_file: None,
        hover: h.text,
        signature: h.signature,
        doc: h.doc,
        definition: true,
    }
}

/// How hover spells one signature: declarations as their module wrote them,
/// with the module's import path where the signature shows two of one
/// spelling. `types` and `names` are what the signature shows.
fn signature_speech<'a>(sp: &'a Spellings, types: &[&Type], names: &[&str]) -> Speech<'a> {
    static ROOT: Option<String> = None;
    sp.speech(&ROOT).sentence(types, names)
}

/// Hover text for a local binding: `name: Type` for a param, `let [mut] name:
/// Type` for a let, `for name: Type` for a loop variable; no type when none is
/// known.
fn local_detail(b: &LocalBinding, sp: &Spellings) -> String {
    let ty = |t: &Type| type_to_string(t, sp);
    match b.kind {
        LocalKind::Param => format!("{}: {}", b.name, ty(b.ty.as_ref().unwrap())),
        LocalKind::Let { mutable } => match (&b.ty, mutable) {
            (Some(t), true) => format!("let mut {}: {}", b.name, ty(t)),
            (Some(t), false) => format!("let {}: {}", b.name, ty(t)),
            (None, true) => format!("let mut {}", b.name),
            (None, false) => format!("let {}", b.name),
        },
        LocalKind::ForVar => match &b.ty {
            Some(t) => format!("for {}: {}", b.name, ty(t)),
            None => format!("for {}", b.name),
        },
    }
}

fn function_detail(f: &Function, sp: &Spellings) -> String {
    let mut types: Vec<&Type> = f.params.iter().map(|p| &p.ty).collect();
    types.push(&f.ret);
    let say = signature_speech(sp, &types, &[&f.name]);
    // A capability shows where it was written: before the type of a parameter
    // (`iss: modify Array<Issue>`), before `self` for a receiver
    // (`modify self: Tally`). It is the call's whole contract. The receiver
    // keeps its type, which the source never states.
    let params = f
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let word = match p.capability {
                Capability::Read => "",
                Capability::Modify => "modify ",
                Capability::Consume => "consume ",
            };
            let ty = spell(&say, &p.ty);
            // A clause is part of the contract: the call checks it.
            let rule = (p.clause.as_ref())
                .map(|e| format!(" where {}", crate::checker::pred_summary(e)))
                .unwrap_or_default();
            if i == 0 && p.name == "self" {
                format!("{word}self: {ty}")
            } else {
                format!("{}: {word}{ty}{rule}", p.name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let tp = if f.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", f.type_params.join(", "))
    };
    // `extern fn`, `export extern fn` and `mut fn` show
    // their markers: the boundary crossing and its direction, and what makes a
    // procedure a Mutation.
    let kw = if f.is_export_extern {
        "export extern fn"
    } else if f.is_extern {
        "extern fn"
    } else if f.is_mut {
        "mut fn"
    } else {
        "fn"
    };
    format!(
        "{} {}{}({}) -> {}",
        kw,
        say.name(&f.name),
        tp,
        params,
        spell(&say, &f.ret)
    )
}

/// Hover text for a module-state binding, such as
/// `let mut hits: Int64`. The type is the annotation, else inferred from a
/// literal initializer, else omitted.
fn global_detail(g: &GlobalDecl, sp: &Spellings) -> String {
    let kw = if g.mutable { "let mut" } else { "let" };
    let ty = g.ty.clone().or_else(|| infer_literal_type(&g.init));
    match ty {
        Some(t) => {
            let say = signature_speech(sp, &[&t], &[&g.name]);
            format!("{} {}: {}", kw, say.name(&g.name), spell(&say, &t))
        }
        None => format!(
            "{} {}",
            kw,
            signature_speech(sp, &[], &[&g.name]).name(&g.name)
        ),
    }
}

/// A type for a literal initializer, for hover only; the checker is
/// authoritative. Covers scalars and homogeneous array literals of scalars.
fn infer_literal_type(e: &Expr) -> Option<Type> {
    match e {
        Expr::Int(_, _) => Some(Type::Int),
        // A byte literal defaults to `UInt8`.
        Expr::Byte(_, _) => Some(Type::IntN {
            bits: 8,
            signed: false,
        }),
        Expr::Float(_, _) => Some(Type::Float),
        Expr::Bool(_, _) => Some(Type::Bool),
        Expr::Str(_, _) => Some(Type::Str),
        Expr::Unary { expr, .. } => infer_literal_type(expr),
        Expr::ArrayLit { elems, .. } => elems
            .first()
            .and_then(infer_literal_type)
            .map(|t| Type::Array(Box::new(t))),
        _ => None,
    }
}

/// The `///` on the protocol signature that `method` implements, if any. It
/// documents every implementation, so hover and completion borrow it for an
/// impl method without its own doc.
fn signature_doc(protocols: &[ProtocolDecl], protocol: &str, method: &str) -> Option<String> {
    protocols
        .iter()
        .find(|p| p.name == protocol)
        .and_then(|p| p.methods.iter().find(|m| m.name == method))
        .and_then(|m| m.doc.clone())
}

/// The signature of a protocol method as `say` spells it. The parser keeps
/// only parameter types; `self` is prepended. Capabilities show, because they
/// are the call's whole discipline.
fn method_sig(m: &MethodSig, say: &Speech) -> String {
    let cap = |c: Capability, t: String| match c {
        Capability::Read => t,
        Capability::Modify => format!("modify {t}"),
        Capability::Consume => format!("consume {t}"),
    };
    let mut ps = vec![cap(m.recv, "self".to_string())];
    ps.extend(m.params.iter().enumerate().map(|(i, t)| {
        cap(
            m.param_caps.get(i).copied().unwrap_or(Capability::Read),
            spell(say, t),
        )
    }));
    format!("fn {}({}) -> {}", m.name, ps.join(", "), spell(say, &m.ret))
}

fn method_types(m: &MethodSig) -> impl Iterator<Item = &Type> {
    m.params.iter().chain([&m.ret])
}

fn method_sig_detail(m: &MethodSig, sp: &Spellings) -> String {
    let types: Vec<&Type> = method_types(m).collect();
    method_sig(m, &signature_speech(sp, &types, &[]))
}

fn protocol_detail(p: &ProtocolDecl, sp: &Spellings) -> String {
    let types: Vec<&Type> = p.methods.iter().flat_map(method_types).collect();
    let say = signature_speech(sp, &types, &[&p.name]);
    let ms = (p.methods.iter())
        .map(|m| method_sig(m, &say))
        .collect::<Vec<_>>()
        .join("; ");
    let name = say.name(&p.name);
    if ms.is_empty() {
        format!("protocol {name}")
    } else {
        format!("protocol {name} {{ {ms} }}")
    }
}

fn type_decl_detail(t: &TypeDecl, all: &[TypeDecl], sp: &Spellings) -> String {
    let say = signature_speech(sp, &[&t.base], &[&t.name]);
    let name = say.name(&t.name);
    match &t.base {
        // A declared variant list. An alias of a built-in sum spells itself
        // `Option<T>` or `Result<T, E>`, as the module wrote it.
        Type::Enum(vs) if !crate::types::is_sum_alias(&t.base) => {
            let arms = (vs.iter())
                .map(|v| variant_arm(v, &|t| spell(&say, t)))
                .collect::<Vec<_>>()
                .join(" | ");
            format!("type {name} = {arms}")
        }
        Type::Record(fields) => {
            let fs = fields
                .iter()
                .map(|f| field_text(f, |n| all.iter().find(|d| d.name == n), &|t| spell(&say, t)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("type {name} = {{ {fs} }}")
        }
        _ => {
            let s = spell(&say, &t.base);
            if t.predicate.is_some() {
                format!("type {name} = {s} (validated)")
            } else {
                format!("type {name} = {s}")
            }
        }
    }
}

fn variant_detail(enum_name: &str, v: &EnumVariant, sp: &Spellings) -> String {
    let types: Vec<&Type> = v.payload.iter().collect();
    let say = signature_speech(sp, &types, &[enum_name]);
    format!(
        "variant of {}: {}",
        say.name(enum_name),
        variant_arm(v, &|t| spell(&say, t))
    )
}

/// `ty` as `say` spells it. The AST's `Display` spells every type but an
/// enum, which renders its variant arms.
fn spell(say: &Speech, ty: &Type) -> String {
    match ty {
        Type::Enum(vs) if !crate::types::is_sum_alias(ty) => {
            let arms = vs
                .iter()
                .map(|v| variant_arm(v, &|t| spell(say, t)))
                .collect::<Vec<_>>();
            format!("{{ {} }}", arms.join(" | "))
        }
        other => say.ty(other).to_string(),
    }
}

/// The user-facing spelling of a type, as hover writes it: each declaration as
/// its module wrote it (`sp`). Public because the LSP's inlay type hints render
/// the same string, so a hint and a hover agree.
pub fn type_to_string(ty: &Type, sp: &Spellings) -> String {
    spell(&signature_speech(sp, &[ty], &[]), ty)
}

/// One exported declaration to document: its signature line, as hover
/// renders it, and its `///` block verbatim.
#[derive(Debug, Clone)]
pub struct DocExport {
    pub name: String,
    pub kind: SymbolKind,
    /// 1-based declaration line, the sort key.
    pub line: usize,
    /// The rendered signature: `fn route(req: Request) -> Response`,
    /// `type Paste = { .. }`, `protocol Show { fn show(self) -> String }`.
    pub signature: String,
    /// The declaration's `///` documentation (markdown), verbatim.
    pub doc: Option<String>,
    /// `(signature, doc)` per protocol method that carries a `///` block. Empty
    /// for everything else: an undocumented member adds nothing the protocol's
    /// signature does not say.
    pub members: Vec<(String, String)>,
}

/// A module's documentation model for `vyrn doc`: the detached
/// file-header `///` block, then every export in declaration order. Built from
/// the parse alone, so a module with unresolved imports still documents.
#[derive(Debug, Clone)]
pub struct ModuleDoc {
    /// The leading `///` block, when a blank line detaches it from the first
    /// declaration.
    pub header_doc: Option<String>,
    pub exports: Vec<DocExport>,
}

/// The module-header doc: the first run of `///` lines at the top of the file,
/// only when a blank line detaches it from what follows or nothing follows. A
/// block directly above a declaration belongs to it. Mirrors the parser's
/// `take_docs` rule.
fn module_header_doc(tokens: &[lexer::Token]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut last_line = 0usize;
    for t in tokens {
        match &t.tok {
            lexer::Tok::Doc(s) => {
                // A gap inside the leading run closes the first block, which is
                // then detached: the header.
                if !lines.is_empty() && t.line > last_line + 1 {
                    return Some(lines.join("\n"));
                }
                lines.push(s.clone());
                last_line = t.line;
            }
            lexer::Tok::Eof => break,
            _ => {
                if lines.is_empty() {
                    return None; // code or an import leads the file: no header
                }
                // The header only when a blank line detaches it from this first
                // real token.
                return if t.line > last_line + 1 {
                    Some(lines.join("\n"))
                } else {
                    None
                };
            }
        }
    }
    // The file is nothing but a leading `///` block.
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Builds the documentation model for one module's source: every
/// exported function, protocol and type, with its hover signature and `///`
/// block, sorted by declaration line. The parse recovers, as in the LSP.
pub fn module_doc(source: &str) -> ModuleDoc {
    let Ok(tokens) = lexer::lex(source) else {
        return ModuleDoc {
            header_doc: None,
            exports: Vec::new(),
        };
    };
    let header_doc = module_header_doc(&tokens);
    let (program, _errs) = parser::parse_accum(tokens);
    let mut exports: Vec<DocExport> = Vec::new();
    for f in &program.functions {
        if f.exported {
            exports.push(DocExport {
                name: f.name.clone(),
                kind: SymbolKind::Function,
                line: f.line,
                signature: function_detail(f, &Spellings::default()),
                doc: f.doc.clone(),
                members: Vec::new(),
            });
        }
    }
    for p in &program.protocols {
        if p.exported {
            exports.push(DocExport {
                name: p.name.clone(),
                kind: SymbolKind::Type,
                line: p.line,
                signature: protocol_detail(p, &Spellings::default()),
                doc: p.doc.clone(),
                members: p
                    .methods
                    .iter()
                    .filter_map(|m| {
                        m.doc
                            .clone()
                            .map(|d| (method_sig_detail(m, &Spellings::default()), d))
                    })
                    .collect(),
            });
        }
    }
    for t in &program.type_decls {
        // Skip synthetic decls: line-0 records and dotted inline-refinement types.
        if t.line == 0 || ast::is_synthetic(&t.name) || !t.exported {
            continue;
        }
        exports.push(DocExport {
            name: t.name.clone(),
            kind: SymbolKind::Type,
            line: t.line,
            signature: type_decl_detail(t, &program.type_decls, &Spellings::default()),
            doc: t.doc.clone(),
            members: Vec::new(),
        });
    }
    // Declaration order; a tie (synthetic only) falls back to the name.
    exports.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
    ModuleDoc {
        header_doc,
        exports,
    }
}

/// The kind an identifier resolves to for semantic highlighting. The
/// server maps it one to one onto the LSP's standard token types, so a function
/// call colours apart from a variable, which TextMate cannot do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemKind {
    /// An `import * as ns` binding, or a `ns.` qualifier.
    Namespace,
    /// A type or enum name, imported ones included.
    Type,
    /// An enum variant or option-result constructor (`Circle`, `Some`, `Ok`).
    EnumMember,
    /// A function parameter.
    Parameter,
    /// A `let` or `for` local, or module state.
    Variable,
    /// A record field accessed as a member.
    Property,
    /// A function: definition, call or import.
    Function,
    /// A protocol or builtin method.
    Method,
    /// A compiler builtin free function (`toJson`, `bytes`, `print`), coloured
    /// apart from user calls.
    Macro,
}

/// Semantic-token modifiers; the server encodes them as an LSP bitset.
/// `declaration` marks the defining occurrence, `readonly` a non-`mut` binding,
/// `default_library` std and builtins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemMods {
    pub declaration: bool,
    pub readonly: bool,
    pub default_library: bool,
    /// The occurrence where the binding's value stops being live: the move or
    /// `drop` that takes it. Locals only.
    pub last_use: bool,
}

/// One classified identifier occurrence: its 1-based position, its character
/// length, its [`SemKind`] and its [`SemMods`].
#[derive(Debug, Clone)]
pub struct SemToken {
    pub line: usize,
    pub col: usize,
    pub len: usize,
    pub kind: SemKind,
    pub mods: SemMods,
}

/// Whether `name` is a builtin free function, coloured `macro` apart from user
/// functions: a row with a contract, no method spelling and no lending. The
/// `x.method()` builtins are [`builtin_method`]'s.
fn is_macro_builtin(name: &str) -> bool {
    crate::prelude::builtin(name).is_some_and(|b| b.sig.is_some() && b.method.is_none())
        && !crate::prelude::lends(name)
}

/// Whether `name` is a builtin `Option` or `Result` constructor, which colours
/// as `enumMember` like a user variant. A filter over
/// [`BUILTIN_TYPES_AND_CTORS`].
fn is_constructor_builtin(name: &str) -> bool {
    BUILTIN_TYPES_AND_CTORS
        .iter()
        .any(|(n, _, kind, _)| *n == name && matches!(kind, SymbolKind::Variant))
}

fn sem_of_symbol_kind(k: SymbolKind) -> SemKind {
    match k {
        SymbolKind::Function => SemKind::Function,
        SymbolKind::Type => SemKind::Type,
        SymbolKind::Variant => SemKind::EnumMember,
        SymbolKind::Method => SemKind::Method,
        SymbolKind::Field => SemKind::Property,
        SymbolKind::Param => SemKind::Parameter,
        SymbolKind::Local => SemKind::Variable,
        SymbolKind::Global => SemKind::Variable,
    }
}

/// Whether an imported symbol's file is a std module, which earns the
/// `defaultLibrary` modifier.
fn is_std_file(file: &Option<String>) -> bool {
    file.as_deref()
        .is_some_and(|f| f.contains("/std/") || f.starts_with("std/"))
}

/// Classifies every identifier in the document for semantic highlighting, from
/// the built [`Analysis`] without a reparse. An import specifier takes its
/// declaration's kind, as go-to-definition resolves it. A token that resolves to
/// nothing (a keyword, an unknown name) is left to the TextMate grammar.
pub fn semantic_tokens(analysis: &Analysis) -> Vec<SemToken> {
    let mut out = Vec::new();
    for t in &analysis.tokens {
        if t.text == "." {
            continue;
        }
        if let Some((kind, mods)) = classify_token(analysis, t) {
            out.push(SemToken {
                line: t.line,
                col: t.col,
                len: t.end_col.saturating_sub(t.col),
                kind,
                mods,
            });
        }
    }
    out
}

/// One inlay hint: a label the editor draws inside the line, without editing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHint {
    /// 1-based position the label is drawn at.
    pub line: usize,
    pub col: usize,
    pub label: String,
}

/// Move hints for a document: at the occurrence that moves a value, a label
/// `-> f(..)` says where it went. A move is the one memory event with its own
/// source position. Pure over the cached [`Analysis`].
pub fn inlay_hints(analysis: &Analysis) -> Vec<InlayHint> {
    let mut out = Vec::new();
    for m in &analysis.memory {
        let (Some(at), Some(into)) = (m.last_use, m.moved_into.as_ref()) else {
            continue;
        };
        // Anchor at the last occurrence of the name on the move line, since the
        // move is the last use (`pack(a, g(a))` moves the second `a`), skipping
        // member positions. If the source moved on since the analysis, no hint.
        let Some(tok) = analysis
            .tokens
            .iter()
            .filter(|t| t.line == at && t.text == m.name && !is_member_position(analysis, t))
            .last()
        else {
            continue;
        };
        // `into` is worded for a diagnostic, with backticks; a hint drawn in the
        // code has none.
        let into = into.trim_matches('`');
        out.push(InlayHint {
            line: at,
            col: tok.end_col,
            label: format!("→ {into}"),
        });
    }
    out.sort_by_key(|h| (h.line, h.col));
    out.dedup();
    out
}

/// Classifies the identifier token covering 1-based `(line, col)`, if any. The
/// `.vyx` forward-mapper classifies template tokens with it. It follows
/// [`resolve`]'s precedence, so a colour agrees with hover.
pub fn classify_at(analysis: &Analysis, line: usize, col: usize) -> Option<(SemKind, SemMods)> {
    let tok = analysis
        .tokens
        .iter()
        .find(|t| t.text != "." && t.line == line && col >= t.col && col < t.end_col)?;
    classify_token(analysis, tok)
}

/// One reference to a binding, for `textDocument/documentHighlight`.
/// `write` marks the declaration; every use is a read.
#[derive(Debug, Clone)]
pub struct RefRange {
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
    pub write: bool,
}

/// Whether `tok` sits in member position, directly after a `.` on its line
/// (`recv.tok`). Such a token names a member, not a binding of that name.
///
/// The test is the preceding token, not a zero-gap column check: `u . age` is
/// the same member access as `u.age`.
fn is_member_position(analysis: &Analysis, tok: &TokenInfo) -> bool {
    let mut prev_dot = false;
    for t in &analysis.tokens {
        if t.line == tok.line && t.col == tok.col && t.end_col == tok.end_col {
            return prev_dot;
        }
        prev_dot = t.text == "." && t.line == tok.line;
    }
    false
}

/// References to the binding under the 1-based `(line, col)` cursor, resolved as
/// hover resolves, never by word match. A local highlights only its in-scope
/// uses, a top-level symbol its references where no local shadows it, a
/// namespace its qualifiers. Comments are not tokens, so they never appear.
///
/// Empty means nothing resolves here; the server sends an empty list, so the
/// editor does not fall back to word-matching.
pub fn references(analysis: &Analysis, line: usize, col: usize) -> Vec<RefRange> {
    let Some(tok) = analysis
        .tokens
        .iter()
        .find(|t| t.text != "." && t.line == line && col >= t.col && col < t.end_col)
    else {
        return Vec::new();
    };
    let name = tok.text.clone();
    let cursor_member = is_member_position(analysis, tok);

    // A member occurrence: the same member through the same-named receiver, so
    // unrelated same-named members elsewhere stay out.
    if cursor_member {
        let recv = receiver_before_dot(analysis, tok.line, tok.col).map(|r| &r.text);
        let mut out = Vec::new();
        for t in &analysis.tokens {
            if t.text != name || !is_member_position(analysis, t) {
                continue;
            }
            if receiver_before_dot(analysis, t.line, t.col).map(|r| &r.text) == recv {
                out.push(RefRange {
                    line: t.line,
                    col: t.col,
                    end_col: t.end_col,
                    write: false,
                });
            }
        }
        return dedup_refs(out);
    }

    let read = |t: &TokenInfo| RefRange {
        line: t.line,
        col: t.col,
        end_col: t.end_col,
        write: false,
    };
    match resolve_token(analysis, tok, || None::<()>) {
        // A local binding shadows everything: only the uses that name it.
        Some(Named::Local(target)) => {
            let at = (target.line, target.col);
            let uses = (analysis.tokens.iter()).filter(|t| {
                t.text == name
                    && !is_member_position(analysis, t)
                    && local_at(analysis, t).is_some_and(|b| (b.line, b.col) == at)
            });
            let out = uses.map(|t| RefRange {
                write: (t.line, t.col) == at,
                ..read(t)
            });
            dedup_refs(out.collect())
        }
        // A namespace binding: the bare `ns` tokens no local shadows.
        Some(Named::Namespace(_)) => {
            let bare = (analysis.tokens.iter()).filter(|t| {
                t.text == name
                    && !is_member_position(analysis, t)
                    && local_at(analysis, t).is_none()
            });
            dedup_refs(bare.map(read).collect())
        }
        // A top-level symbol: its references, except where an in-scope local of
        // the same name shadows it. A bare token after an unrelated `ns.` on its
        // line is the top-level symbol here, as before any namespace member.
        Some(Named::Symbol(_) | Named::NsMember(..)) => {
            let Some(sym) = top_symbol(analysis, &name) else {
                return Vec::new();
            };
            let mut out = references_to(analysis, &name, &[]);
            if sym.file.is_none() {
                for r in &mut out {
                    r.write = r.line == sym.line && r.col == sym.col;
                }
            }
            out
        }
        // Unresolved: an empty list, which suppresses the editor's word-match.
        Some(Named::Member(())) | None => Vec::new(),
    }
}

/// Every occurrence of the top-level `name` in this document, for a caller with
/// no cursor on it: the cross-file half of a rename.
///
/// `qualifiers` are the namespace bindings that also reach the name, so
/// `store.listPastes` counts when `store` names the declaring module and
/// `other.listPastes` does not. A bare occurrence in a function that binds the
/// name locally is the local, as in [`references`]. The occurrences are lexer
/// tokens, so comments, longer identifiers and string contents never match;
/// that makes it safe over a file this server has not linked.
pub fn references_to(analysis: &Analysis, name: &str, qualifiers: &[String]) -> Vec<RefRange> {
    let mut out = Vec::new();
    for t in &analysis.tokens {
        if t.text != name {
            continue;
        }
        if is_member_position(analysis, t) {
            let recv = receiver_before_dot(analysis, t.line, t.col);
            if !recv.is_some_and(|r| qualifiers.iter().any(|q| *q == r.text)) {
                continue;
            }
        } else if local_at(analysis, t).is_some() {
            continue;
        }
        out.push(RefRange {
            line: t.line,
            col: t.col,
            end_col: t.end_col,
            write: false,
        });
    }
    dedup_refs(out)
}

/// Drops duplicate ranges, keeping the first, so a `write` flag wins over a
/// read at the same spot.
fn dedup_refs(mut refs: Vec<RefRange>) -> Vec<RefRange> {
    refs.sort_by_key(|r| (r.line, r.col, !r.write));
    refs.dedup_by_key(|r| (r.line, r.col));
    refs
}

/// The specifier text when the 1-based `(line, col)` cursor is inside an
/// import's source string: a plain specifier (`"./store"`) or a
/// string argument of a generator import (`i18n("../strings")`). `None`
/// elsewhere. An import can span lines, so the string belongs to an import when
/// the greatest import-or-declaration line at or above it is an import.
pub fn import_spec_at(source: &str, line: usize, col: usize) -> Option<String> {
    let toks = lexer::lex(source).ok()?;
    // The string-literal token whose span contains the cursor.
    let (str_line, spec) = toks.iter().find_map(|t| {
        if t.line != line {
            return None;
        }
        if let Tok::Str(s) = &t.tok {
            let start = t.col;
            let end = t.col + s.chars().count() + 2; // + the two quotes
            if col >= start && col <= end {
                return Some((t.line, s.clone()));
            }
        }
        None
    })?;

    let (program, _errs) = parser::parse_accum(toks);
    // Statement-start lines: every top-level declaration and every import. The
    // greatest one at or above the string's line owns it.
    let mut stmt_lines: Vec<(usize, bool)> = decl_lines(&program)
        .into_iter()
        .map(|l| (l, false))
        .collect();
    for imp in &program.imports {
        stmt_lines.push((imp.line, true));
    }
    stmt_lines.sort_by_key(|(l, _)| *l);
    let is_import = stmt_lines
        .iter()
        .rev()
        .find(|(l, _)| *l <= str_line)
        .map(|(_, is_imp)| *is_imp)
        .unwrap_or(false);
    is_import.then_some(spec)
}

/// Resolves an identifier token to a [`SemKind`] and [`SemMods`] in
/// [`resolve_token`]'s order. Its member is a record field on a typed local
/// receiver, a `property`, as member completion finds it. Builtins come last:
/// free functions colour `macro`, option and result constructors
/// `enumMember`, method builtins (`push`, `info`) `method`.
fn classify_token(analysis: &Analysis, tok: &TokenInfo) -> Option<(SemKind, SemMods)> {
    let field = || {
        receiver_before_dot(analysis, tok.line, tok.col)?;
        let is_field = match resolve_receiver_type(analysis, tok.line, tok.col)? {
            Type::Named(n) => {
                (analysis.record_fields.iter()).any(|(tn, c)| *tn == n && c.label == tok.text)
            }
            Type::Record(fields) => fields.iter().any(|f| f.name == tok.text),
            _ => false,
        };
        is_field.then_some(())
    };
    let plain = SemMods::default();
    match resolve_token(analysis, tok, field) {
        Some(Named::Local(b)) => {
            let kind = match b.kind {
                LocalKind::Param => SemKind::Parameter,
                LocalKind::Let { .. } | LocalKind::ForVar => SemKind::Variable,
            };
            let declaration = b.line == tok.line && b.col == tok.col;
            // The occurrence where an owning value stops being live, a move or a
            // `drop`, is marked so a reader need not infer it.
            let last_use = !declaration
                && (analysis.memory.iter())
                    .any(|m| m.name == b.name && m.line == b.line && m.last_use == Some(tok.line));
            let mods = SemMods {
                declaration,
                readonly: matches!(
                    b.kind,
                    LocalKind::Let { mutable: false } | LocalKind::ForVar
                ),
                default_library: false,
                last_use,
            };
            Some((kind, mods))
        }
        Some(Named::NsMember(_, m)) => {
            let mods = SemMods {
                default_library: is_std_file(&m.file),
                ..plain
            };
            Some((sem_of_symbol_kind(m.kind), mods))
        }
        Some(Named::Member(())) => Some((SemKind::Property, plain)),
        Some(Named::Namespace(_)) => Some((SemKind::Namespace, plain)),
        // Import specifiers get their real kind here, since imported decls are
        // indexed with their file.
        Some(Named::Symbol(s)) => {
            let mods = SemMods {
                declaration: s.file.is_none() && s.line == tok.line && s.col == tok.col,
                readonly: s.kind == SymbolKind::Global && !s.mutable,
                default_library: is_std_file(&s.file),
                last_use: false,
            };
            Some((sem_of_symbol_kind(s.kind), mods))
        }
        None if is_macro_builtin(&tok.text) => Some((SemKind::Macro, mods_default_lib())),
        None if is_constructor_builtin(&tok.text) => {
            Some((SemKind::EnumMember, mods_default_lib()))
        }
        None if builtin_method(&tok.text).is_some() => Some((SemKind::Method, mods_default_lib())),
        None => None,
    }
}

/// `SemMods` with only `default_library` set, the builtin shape.
fn mods_default_lib() -> SemMods {
    SemMods {
        declaration: false,
        readonly: false,
        default_library: true,
        last_use: false,
    }
}

/// A built-in method or function the checker handles inline, such as
/// `Array.push` or `Logger.info`: a [`crate::prelude::Builtin`] row with hover
/// text, named by its method spelling. It has no definition site.
#[derive(Clone, Copy)]
struct BuiltinMethod {
    name: &'static str,
    detail: &'static str,
}

/// Every builtin with hover text, in the table's order.
fn builtin_methods() -> impl Iterator<Item = (BuiltinMethod, &'static [crate::prelude::Shape])> {
    crate::prelude::builtins().iter().filter_map(|b| {
        let m = BuiltinMethod {
            name: b.method.unwrap_or(b.name),
            detail: b.hover.as_deref()?,
        };
        Some((m, b.on))
    })
}

/// Hover text for a built-in call name, if `name` is one: [`resolve`]'s last
/// fallback.
fn builtin_method(name: &str) -> Option<BuiltinMethod> {
    builtin_methods().map(|(m, _)| m).find(|m| m.name == name)
}

/// The built-in methods valid on a receiver of type `ty`, for
/// [`member_completions`].
fn builtin_methods_for(ty: &Type) -> Vec<BuiltinMethod> {
    let Some(shape) = crate::prelude::Shape::of(ty) else {
        return Vec::new();
    };
    builtin_methods()
        .filter(|(_, on)| on.contains(&shape))
        .map(|(m, _)| m)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The colouring filter answers for the constructors only: `Result` and
    /// `Option` are types, and must not colour as variants.
    #[test]
    fn only_the_constructors_colour_as_variants() {
        let ctors: Vec<&str> = BUILTIN_TYPES_AND_CTORS
            .iter()
            .filter(|(n, _, _, _)| is_constructor_builtin(n))
            .map(|(n, _, _, _)| *n)
            .collect();
        assert_eq!(ctors, ["Ok", "Err", "Some", "None"]);
        assert!(!is_constructor_builtin("Result"));
        assert!(!is_constructor_builtin("Option"));
        assert!(!is_constructor_builtin("Somewhere"));
    }

    /// Every method the parser accepts after a dot has an editor entry.
    #[test]
    fn every_method_builtin_the_parser_routes_has_an_entry() {
        let missing: Vec<&str> = crate::prelude::builtins()
            .iter()
            .filter(|b| b.hover.is_none())
            .filter_map(|b| b.method)
            .collect();
        assert!(
            missing.is_empty(),
            "method builtins with no hover or completion: {}",
            missing.join(", ")
        );
    }

    /// Every entry says something: a blank hover reads as a broken editor.
    #[test]
    fn every_builtin_method_entry_describes_itself() {
        for (b, _) in builtin_methods() {
            assert!(
                b.detail.contains("—") && b.detail.len() > 20,
                "`{}` has no readable detail: {:?}",
                b.name,
                b.detail
            );
        }
    }

    /// A sequence (`Tw`) string type's alphabet is enumerated;
    /// `class_completions` offers it at a `cls("...")` argument, and
    /// `class_token_hover` shows the utility's CSS or "safelisted".
    #[test]
    fn tw_alphabet_completion_and_hover() {
        let src = "type TwClass = String where value =~ \"(a-1|a-2|flex)\"\n\
                   type Tw = String where value =~ \"(a-1|a-2|flex)( (a-1|a-2|flex))*\"\n\
                   fn cls(c: Tw) -> Int64 { return 0 }\n\
                   fn css() -> String { return \".a-1 {color:red}\\n.flex {display:flex}\" }\n\
                   fn main() -> Int64 { return cls(\"flex a-1\") }\n";
        let a = analyze(src);
        // The alphabet is enumerated, in any order.
        let (_, alpha) = a
            .sequence_string_types
            .iter()
            .find(|(n, _)| n == "Tw")
            .expect("Tw seq");
        for m in ["a-1", "a-2", "flex"] {
            assert!(alpha.iter().any(|s| s == m), "alphabet has {m}: {alpha:?}");
        }
        // A finite type is not a sequence type; the whole-domain path owns it.
        assert!(!a.sequence_string_types.iter().any(|(n, _)| n == "TwClass"));

        // `return cls("flex a-1")` is on line 5.
        let line = 5;
        let col = src.lines().nth(4).unwrap().find("flex a-1").unwrap() + 1 + 1; // inside 'flex'
        let items = class_completions(&a, src, line, col).expect("class completions");
        assert!(items.iter().any(|c| c.label == "flex"));
        assert!(items.iter().any(|c| c.label == "a-2"));

        // Hover on `flex` shows its CSS rule; on `a-1`, its rule too.
        let hv = (class_token_hover(&a, src, line, col).expect("hover on class token")).text;
        assert!(hv.contains("display:flex"), "utility CSS: {hv}");
        let col_a1 = src.lines().nth(4).unwrap().find("a-1\")").unwrap() + 1 + 1;
        let hv2 = (class_token_hover(&a, src, line, col_a1).expect("hover a-1")).text;
        assert!(hv2.contains("color:red"), "a-1 rule: {hv2}");
    }

    /// `css_rule_for` finds base and variant rules, and `.p-2` is not satisfied
    /// by `.p-20`.
    #[test]
    fn css_rule_lookup() {
        let css = ".p-2 {padding:0.5rem}\n.p-20 {padding:5rem}\n\
                   .md\\:hover\\:bg-x:hover {background:#000}";
        assert_eq!(
            css_rule_for(css, "p-2").as_deref(),
            Some(".p-2 {padding:0.5rem}")
        );
        assert_eq!(
            css_rule_for(css, "p-20").as_deref(),
            Some(".p-20 {padding:5rem}")
        );
        assert_eq!(
            css_rule_for(css, "md:hover:bg-x").as_deref(),
            Some(".md\\:hover\\:bg-x:hover {background:#000}")
        );
        assert_eq!(css_rule_for(css, "missing"), None);
    }

    #[test]
    fn module_state_is_indexed_with_hover_detail() {
        // Globals are in the symbol index. The annotated one shows its
        // type; the unannotated one infers it from its literal.
        let src = "let mut hits = 0\n\
                   let banner: String = \"hi\"\n\
                   fn main() -> Int64 { return hits }";
        let a = analyze(src);
        let hits = a
            .symbols
            .iter()
            .find(|s| s.name == "hits")
            .expect("hits symbol");
        assert_eq!(hits.kind, SymbolKind::Global);
        assert_eq!(hits.detail, "let mut hits: Int64");
        assert_eq!(hits.line, 1);
        assert!(hits.col > 0, "has a name column for go-to-def");

        let banner = a
            .symbols
            .iter()
            .find(|s| s.name == "banner")
            .expect("banner symbol");
        assert_eq!(banner.kind, SymbolKind::Global);
        assert_eq!(banner.detail, "let banner: String");
    }

    #[test]
    fn tests_appear_in_the_symbol_index() {
        // A `test "name"` block is in the outline as a Method with a
        // `test "name"` detail, on its declaration line.
        let src = "test \"adds up\" { assert(1 + 1 == 2) }\n\
                   fn main() -> Int64 { return 0 }";
        let a = analyze(src);
        let t = a
            .symbols
            .iter()
            .find(|s| s.name == "adds up")
            .expect("test symbol");
        assert_eq!(t.kind, SymbolKind::Method);
        assert_eq!(t.detail, "test \"adds up\"");
        assert_eq!(t.line, 1);
        assert!(t.col > 0, "anchored at the `test` keyword for go-to");
    }

    #[test]
    fn benches_appear_in_the_symbol_index() {
        // A `bench "name"` block is in the outline like a test.
        let src = "bench \"hot path\" { blackBox(1) }\n\
                   fn main() -> Int64 { return 0 }";
        let a = analyze(src);
        let b = a
            .symbols
            .iter()
            .find(|s| s.name == "hot path")
            .expect("bench symbol");
        assert_eq!(b.kind, SymbolKind::Method);
        assert_eq!(b.detail, "bench \"hot path\"");
        assert_eq!(b.line, 1);
        assert!(b.col > 0, "anchored at the `bench` keyword for go-to");
    }

    #[test]
    fn analyze_linked_indexes_an_aliased_import() {
        // An aliased import is indexed under the local name, hover notes its
        // original, and go-to-def points at the foreign decl.
        use crate::loader::{LoadOptions, MapResolver};
        let files: std::collections::HashMap<String, String> = [(
            "api.vyrn".to_string(),
            "export fn getUser(id: Int64) -> Int64 { return id }".to_string(),
        )]
        .into_iter()
        .collect();
        let resolver = MapResolver(files);
        let root = "import { getUser as fetchUser } from \"./api\"\n\
                    fn main() -> Int64 { return fetchUser(1) }";
        let a = analyze_linked(root, "main.vyrn", &LoadOptions::default(), &resolver, None);
        assert!(a.diagnostics.is_empty(), "diags: {:?}", a.diagnostics);
        let sym = a
            .symbols
            .iter()
            .find(|s| s.name == "fetchUser" && s.file.is_some())
            .expect("aliased import indexed under the local name");
        assert!(
            sym.detail.contains("alias of `getUser`"),
            "hover detail: {}",
            sym.detail
        );
        assert_eq!(
            sym.file.as_deref(),
            Some("api.vyrn"),
            "go-to-def jumps to the source module"
        );
        // The original name is not indexed as an imported symbol.
        assert!(
            !a.symbols
                .iter()
                .any(|s| s.name == "getUser" && s.file.is_some()),
            "original name is hidden by the alias"
        );
    }

    #[test]
    fn an_imported_name_indexes_the_module_it_came_from() {
        // `z` imports another module's `pick`, so the linker renames one of
        // the two; the root's `pick` is still `n`'s.
        use crate::loader::{LoadOptions, MapResolver};
        let files = [
            (
                "n.vyrn",
                "/// N pick.\nexport fn pick() -> Int64 { return 1 }",
            ),
            (
                "m.vyrn",
                "/// M pick.\nexport fn pick() -> Int64 { return 2 }",
            ),
            (
                "z.vyrn",
                "import * as nn from \"./n\"\nimport { pick } from \"./m\"\n\
                 export fn zed() -> Int64 { return nn.pick() * 10 + pick() }",
            ),
        ];
        let files = files.map(|(k, v)| (k.to_string(), v.to_string()));
        let resolver = MapResolver(files.into_iter().collect());
        let root = "import { pick } from \"./n\"\nimport { zed } from \"./z\"\n\
                    fn main() -> Int64 { return pick() + zed() }";
        let a = analyze_linked(root, "main.vyrn", &LoadOptions::default(), &resolver, None);
        assert!(a.diagnostics.is_empty(), "diags: {:?}", a.diagnostics);
        let pick = |s: &&Symbol| s.name == "pick" && s.file.is_some();
        let picks: Vec<_> = (a.symbols.iter().filter(pick))
            .map(|s| (s.file.as_deref(), s.doc.as_deref(), &*s.detail))
            .collect();
        let detail = "fn pick() -> Int64";
        assert_eq!(picks, [(Some("n.vyrn"), Some("N pick."), detail)]);
    }

    #[test]
    fn a_name_after_a_block_is_the_outer_binding() {
        let src = "fn helper() -> Int64 { return 7 }\n\
                   fn main() -> Int64 {\n\
                   let mut x = 1\n\
                   if x > 0 {\n\
                   let x = \"s\"\n\
                   let helper = 3\n\
                   print(x)\n\
                   print(\"\\{helper}\")\n\
                   }\n\
                   x = x + helper()\n\
                   return x\n\
                   }";
        let a = analyze(src);
        assert!(a.diagnostics.is_empty(), "diags: {:?}", a.diagnostics);
        for col in [1, 5] {
            let r = resolve(&a, 10, col).expect("`x` resolves");
            assert_eq!((r.kind, r.target_line), (SymbolKind::Local, 3), "col {col}");
        }
        let r = resolve(&a, 10, 9).expect("`helper` resolves");
        assert_eq!((r.kind, r.target_line), (SymbolKind::Function, 1));
    }

    #[test]
    fn an_imported_impl_binds_nothing_in_the_root() {
        use crate::loader::{LoadOptions, MapResolver};
        let api = "export type Box = { n: Int64 }\n\
                   protocol Size {\n\
                   fn size(self) -> Int64\n\
                   }\n\
                   impl Size for Box {\n\
                   fn size(self) -> Int64 {\n\
                   let stray = self.n\n\
                   return stray\n\
                   }\n\
                   }";
        let resolver = MapResolver([("api.vyrn".to_string(), api.to_string())].into());
        let root = "import { Box } from \"./api\"\nfn main() -> Int64 { return 0 }";
        let a = analyze_linked(root, "main.vyrn", &LoadOptions::default(), &resolver, None);
        assert!(a.diagnostics.is_empty(), "diags: {:?}", a.diagnostics);
        assert!(
            !a.locals.iter().any(|b| b.name == "stray"),
            "{:?}",
            a.locals
        );
    }

    #[test]
    fn analyze_linked_indexes_namespace_members() {
        // `import * as ns` indexes the module's exports for `ns.` completion and
        // `ns.member` hover and go-to-definition.
        use crate::loader::{LoadOptions, MapResolver};
        let files: std::collections::HashMap<String, String> = [(
            "api.vyrn".to_string(),
            "export type User = { id: Int64 }\n\
             export fn getUser(id: Int64) -> User { return User { id: id } }"
                .to_string(),
        )]
        .into_iter()
        .collect();
        let resolver = MapResolver(files);
        let root = "import * as api from \"./api\"\n\
                    fn main() -> Int64 { let u = api.getUser(1) return u.id }";
        let a = analyze_linked(root, "main.vyrn", &LoadOptions::default(), &resolver, None);
        assert!(a.diagnostics.is_empty(), "diags: {:?}", a.diagnostics);

        // The namespace binding and its members are recorded.
        let nsi = a
            .namespaces
            .iter()
            .find(|n| n.name == "api")
            .expect("namespace `api` indexed");
        assert!(
            nsi.members.iter().any(|m| m.name == "getUser"),
            "getUser member"
        );
        assert!(nsi.members.iter().any(|m| m.name == "User"), "User member");

        // Columns are 1-based; line 2 is the `fn main` body.
        let body = root.lines().nth(1).unwrap();
        let getuser_col = body.find("getUser").unwrap() + 1;

        // Completion at the `getUser` member position offers the module's exports.
        let comps = member_completions(&a, 2, getuser_col);
        assert!(
            comps.iter().any(|c| c.label == "getUser"),
            "completions: {comps:?}"
        );
        assert!(
            comps
                .iter()
                .all(|c| c.detail.contains("via namespace `api`")),
            "via-namespace note"
        );

        // Go-to-definition on the `getUser` in `api.getUser` jumps into api.vyrn.
        let r = resolve(&a, 2, getuser_col).expect("resolve api.getUser");
        assert_eq!(r.name, "getUser");
        assert_eq!(
            r.target_file.as_deref(),
            Some("api.vyrn"),
            "cross-file go-to-def"
        );
        assert!(
            r.hover.contains("via namespace `api`"),
            "hover note: {}",
            r.hover
        );

        // Hovering the `api` binding shows the namespace hover (not a value).
        let acol = body.find("api.").unwrap() + 1;
        let rn = resolve(&a, 2, acol).expect("resolve namespace name");
        assert!(
            rn.hover.contains("namespace `api`"),
            "namespace hover: {}",
            rn.hover
        );
    }

    #[test]
    fn map_receiver_completes_its_method_surface() {
        // `.` on a Map-typed local offers `has`/`remove`/`keys` and `length`.
        let src = "fn main() -> Int64 {\n\
                   let mut m: Map<String, Int64> = [:]\n\
                   let x = m.has(\"a\")\n\
                   return 0 }";
        let a = analyze(src);
        let line = src.lines().nth(2).unwrap();
        // Cursor just after `m.` (1-based column).
        let col = line.find("m.").unwrap() + 3;
        let comps = member_completions(&a, 3, col);
        let labels: Vec<&str> = comps.iter().map(|c| c.label.as_str()).collect();
        for want in ["has", "remove", "keys", "length"] {
            assert!(labels.contains(&want), "expected `{want}` in {labels:?}");
        }
        // Array-only shrinking ops are NOT offered on a Map.
        assert!(
            !labels.contains(&"pop"),
            "map must not offer `pop`: {labels:?}"
        );
    }

    /// `copy` completes on a receiver that owns heap, and its hover
    /// states the rule.
    #[test]
    fn copy_completes_and_hovers_on_an_owning_receiver() {
        let src = "fn main() -> Int64 {\n\
                   let s = \"a\" + \"b\"\n\
                   let t = s.copy()\n\
                   return t.byteLength }";
        let a = analyze(src);
        let line = src.lines().nth(2).unwrap();
        let col = line.find("s.").unwrap() + 3;
        let labels: Vec<String> = member_completions(&a, 3, col)
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert!(labels.iter().any(|l| l == "copy"), "{labels:?}");
        let hover = builtin_method("copy").expect("`copy` hovers").detail;
        assert!(hover.contains("shares no heap"), "{hover}");
        // A scalar receiver does not offer it: there it is the identity.
        let scalar = "fn main() -> Int64 {\n\
                      let n = 5\n\
                      let m = n.toString()\n\
                      return 0 }";
        let a2 = analyze(scalar);
        let l2 = scalar.lines().nth(2).unwrap();
        let c2 = l2.find("n.").unwrap() + 3;
        let labels2: Vec<String> = member_completions(&a2, 3, c2)
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert!(!labels2.iter().any(|l| l == "copy"), "{labels2:?}");
    }

    const TRANSKEY: &str =
        "type TransKey = String where value =~ \"nav\\\\.(home|about)\\\\.label\"\n";

    #[test]
    fn finite_string_type_is_enumerated_in_analysis() {
        let a = analyze(&format!("{TRANSKEY}fn main() -> Int64 {{ return 0 }}"));
        let (_, domain) = a
            .finite_string_types
            .iter()
            .find(|(n, _)| n == "TransKey")
            .expect("TransKey enumerated");
        assert_eq!(
            domain,
            &vec!["nav.about.label".to_string(), "nav.home.label".to_string()]
        );
    }

    #[test]
    fn string_literal_completion_at_a_call_argument() {
        let src = format!(
            "{TRANSKEY}fn t(key: TransKey) -> Int64 {{ return 0 }}\n\
             fn main() -> Int64 {{ return t(\"\") }}"
        );
        // The `""` opens at col 31 on line 3; the cursor is at col 32.
        let items = string_literal_completions(&analyze(&src), &src, 3, 32);
        let labels: Vec<&str> = items.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["nav.about.label", "nav.home.label"]);
    }

    #[test]
    fn string_literal_completion_at_an_annotated_let() {
        let src = format!("{TRANSKEY}fn main() -> Int64 {{ let k: TransKey = \"\"  return 0 }}");
        // Line 2: the `""` opens at col 40.
        let items = string_literal_completions(&analyze(&src), &src, 2, 41);
        let labels: Vec<&str> = items.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["nav.about.label", "nav.home.label"]);
    }

    #[test]
    fn over_cap_or_infinite_type_offers_nothing() {
        // An infinite regex string type is never enumerated, so it offers no
        // completions.
        let src = "type Any = String where value =~ \"[a-z]+\"\n\
                   fn f(x: Any) -> Int64 { return 0 }\n\
                   fn main() -> Int64 { return f(\"\") }";
        let a = analyze(src);
        assert!(a.finite_string_types.iter().all(|(n, _)| n != "Any"));
        assert!(string_literal_completions(&a, src, 3, 32).is_empty());
    }

    #[test]
    fn string_literal_completion_outside_a_typed_context_is_empty() {
        // A plain `String` argument has no finite domain.
        let src = format!(
            "{TRANSKEY}fn g(s: String) -> Int64 {{ return 0 }}\n\
             fn main() -> Int64 {{ return g(\"\") }}"
        );
        assert!(string_literal_completions(&analyze(&src), &src, 3, 32).is_empty());
    }

    #[test]
    fn module_doc_lists_only_exports_in_declaration_order() {
        let src = "export fn b() -> Int64 { return 0 }\n\
                   fn priv() -> Int64 { return 0 }\n\
                   export type A = { x: Int64 }\n";
        let d = module_doc(src);
        let names: Vec<&str> = d.exports.iter().map(|e| e.name.as_str()).collect();
        // `priv` is not exported; the order follows source lines.
        assert_eq!(names, ["b", "A"]);
        assert_eq!(d.exports[0].signature, "fn b() -> Int64");
        assert_eq!(d.exports[1].signature, "type A = { x: Int64 }");
    }

    #[test]
    fn a_mut_fn_shows_its_marker_in_the_signature() {
        // The marker decides whether a projection calls the procedure a
        // mutation, so hover and `vyrn doc` show it.
        let src = "export mut fn create(x: Int64) -> Int64 { return x }\n\
                   export fn read() -> Int64 { return 0 }\n";
        let d = module_doc(src);
        assert_eq!(d.exports[0].signature, "mut fn create(x: Int64) -> Int64");
        assert_eq!(d.exports[1].signature, "fn read() -> Int64");
    }

    #[test]
    fn module_doc_attaches_a_declaration_doc_but_not_a_detached_one() {
        // A `///` directly above a decl attaches; one split off by a blank line
        // does not, so `area` carries no doc.
        let src = "/// attaches to foo\n\
                   export fn foo() -> Int64 { return 0 }\n\
                   \n\
                   /// detached from area by a blank line\n\
                   \n\
                   export fn area() -> Int64 { return 0 }\n";
        let d = module_doc(src);
        assert_eq!(d.exports[0].doc.as_deref(), Some("attaches to foo"));
        assert_eq!(d.exports[1].doc, None);
    }

    #[test]
    fn module_doc_carries_documented_protocol_methods_only() {
        let src = "/// the protocol\n\
                   export protocol P {\n\
                   /// what show does\n\
                   fn show(self) -> String\n\
                   fn debug(self) -> String\n\
                   }\n";
        let d = module_doc(src);
        assert_eq!(d.exports[0].doc.as_deref(), Some("the protocol"));
        assert_eq!(
            d.exports[0].members,
            vec![(
                "fn show(self) -> String".to_string(),
                "what show does".to_string()
            )]
        );
    }

    /// A protocol method's symbol carries its `///`, as a free function's does.
    #[test]
    fn a_protocol_method_symbol_carries_its_doc() {
        let src = "export protocol P {\n\
                   /// what show does\n\
                   fn show(self) -> String\n\
                   }\n";
        let a = analyze(src);
        let m = a.symbols.iter().find(|s| s.name == "show").unwrap();
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.doc.as_deref(), Some("what show does"));
    }

    /// A call site (`r.show()`) resolves through the impl, which has no `///`;
    /// the hover shows the protocol signature's doc.
    #[test]
    fn hovering_a_method_call_shows_the_protocol_signatures_doc() {
        let src = "type R = { x: Int64 }\n\
protocol P {\n\
/// what show does\n\
fn show(self) -> String\n\
}\n\
impl P for R {\n\
fn show(self) -> String { return \"r\" }\n\
}\n\
fn main() -> Int64 {\n\
let r = R { x: 1 }\n\
let s = r.show()\n\
return 0\n\
}\n";
        let a = analyze(src);
        let r = resolve(&a, 11, 11).expect("show at (11,11) should resolve");
        assert_eq!(r.hover, "fn show(self: R) -> String\n\nwhat show does");
        // Go-to-definition still lands on the impl, not the signature.
        assert_eq!(r.target_line, 7);
    }

    #[test]
    fn module_doc_header_is_the_detached_leading_block() {
        // A leading block split from the first decl by a blank line is the file
        // header; without the gap it belongs to the declaration.
        let detached = "/// file header\n\
                        /// second line\n\
                        \n\
                        export fn f() -> Int64 { return 0 }\n";
        let d = module_doc(detached);
        assert_eq!(d.header_doc.as_deref(), Some("file header\nsecond line"));
        assert_eq!(d.exports[0].doc, None);

        let attached = "/// belongs to f\n\
                        export fn f() -> Int64 { return 0 }\n";
        let d = module_doc(attached);
        assert_eq!(d.header_doc, None);
        assert_eq!(d.exports[0].doc.as_deref(), Some("belongs to f"));
    }

    #[test]
    fn module_doc_preserves_a_fenced_block_verbatim() {
        let src = "/// A diagram.\n\
                   ///\n\
                   /// ```mermaid\n\
                   /// flowchart LR\n\
                   ///   a --> b\n\
                   /// ```\n\
                   export fn f() -> Int64 { return 0 }\n";
        let d = module_doc(src);
        assert_eq!(
            d.exports[0].doc.as_deref(),
            Some("A diagram.\n\n```mermaid\nflowchart LR\n  a --> b\n```")
        );
    }

    /// One body with every memory outcome a binding can have.
    const MEM_SRC: &str = r#"fn take(s: consume String) -> Int64 { return 1 }
fn main() -> Int64 {
    let a = "x" + "y"
    let n = take(a)
    let b = "p" + "q"
    let c = b.copy()
    drop c
    print(b)
    return n
}
"#;

    /// `MEM_SRC`'s memory answers, as the core states them. This crate installs
    /// no placer, so the tests here check only the adapter: given these answers,
    /// what the editor shows. `vyrn-lsp`'s suite and `tests/memory.rs` check
    /// the answers end to end.
    fn mem_notes() -> Vec<MemoryNote> {
        vec![
            MemoryNote {
                name: "a".into(),
                line: 3,
                text: "moved at line 4 into `take(..)`".into(),
                last_use: Some(4),
                moved_into: Some("`take(..)`".into()),
            },
            MemoryNote {
                name: "b".into(),
                line: 5,
                text: "reclaimed at block exit — freeing the String buffer".into(),
                last_use: None,
                moved_into: None,
            },
            MemoryNote {
                name: "c".into(),
                line: 6,
                text: "reclaimed by `drop` at line 7".into(),
                last_use: Some(7),
                moved_into: None,
            },
        ]
    }

    /// `MEM_SRC` analysed, with the core's answers filled in.
    fn mem_analysis() -> Analysis {
        let mut a = analyze(MEM_SRC);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        a.memory = mem_notes();
        a
    }

    #[test]
    fn a_binding_hover_carries_its_memory_answer() {
        let a = mem_analysis();
        // The `a` in `let a = ..` on line 3.
        let r = resolve(&a, 3, 9).expect("a resolves");
        assert!(
            r.hover.contains("memory: moved at line 4 into `take(..)`"),
            "{}",
            r.hover
        );
    }

    #[test]
    fn a_move_gets_an_inlay_hint_where_the_value_goes() {
        let a = mem_analysis();
        let hints = inlay_hints(&a);
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert_eq!(hints[0].line, 4);
        assert_eq!(hints[0].label, "→ take(..)");
    }

    #[test]
    fn the_last_use_of_an_owning_binding_is_marked() {
        let a = mem_analysis();
        let marked: Vec<(usize, usize)> = semantic_tokens(&a)
            .into_iter()
            .filter(|t| t.mods.last_use)
            .map(|t| (t.line, t.col))
            .collect();
        // The `a` handed to `take` (line 4) and the `c` a `drop` takes (line 7).
        // A declaration is never the last use, and `b` lives to block exit.
        assert_eq!(marked.len(), 2, "{marked:?}");
        assert_eq!(marked[0].0, 4);
        assert_eq!(marked[1].0, 7);
    }

    #[test]
    fn a_document_that_does_not_check_gets_no_memory_answer() {
        // No memory answer for a body the checker refused.
        let a = analyze("fn main() -> Int64 { let s = nope() return 0 }");
        assert!(!a.diagnostics.is_empty());
        assert!(a.memory.is_empty());
    }

    #[test]
    fn a_member_token_hover_and_classify_answer_for_the_field_not_a_local() {
        // `u.age` is a field access although a local `age` shadows the name, so
        // hover and classification answer for the field.
        let src = "type User = { age: Int64 }\n\
                   fn f(u: User) -> Int64 {\n\
                   \x20   let age = 3\n\
                   \x20   return u.age\n\
                   }\n\
                   fn main() -> Int64 { return 0 }";
        let a = analyze(src);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        // Line 4, cursor on `age` in `u.age`.
        let col = src.lines().nth(3).unwrap().find("u.age").unwrap() + 1 + 2;
        let r = resolve(&a, 4, col).expect("field access resolves");
        assert_eq!(r.kind, SymbolKind::Field, "{}", r.hover);
        assert!(
            !r.hover.starts_with("let age"),
            "member captured by the same-named local: {}",
            r.hover
        );
        // Classification agrees: Property, not Variable.
        let (kind, _) = classify_at(&a, 4, col).expect("classifies");
        assert_eq!(kind, SemKind::Property);
        // Control: a plain use of the local still resolves to it.
        let lcol = src.lines().nth(2).unwrap().find("age").unwrap() + 1;
        let r = resolve(&a, 3, lcol).expect("local resolves");
        assert_eq!(r.kind, SymbolKind::Local);
    }

    /// `u . age` with spaces is the same member access, so it classifies as the
    /// field too: the test is that the preceding token is a dot.
    #[test]
    fn a_spaced_member_access_is_still_member_position() {
        let src = "type User = { age: Int64 }\n\
                   fn f(u: User) -> Int64 {\n\
                   \x20   let age = 3\n\
                   \x20   return u . age\n\
                   }\n\
                   fn main() -> Int64 { return 0 }";
        let a = analyze(src);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        // Line 4, cursor on the `age` spelled after `u . `.
        let col = src.lines().nth(3).unwrap().find(". age").unwrap() + 3;
        let r = resolve(&a, 4, col).expect("spaced access resolves");
        assert_eq!(r.kind, SymbolKind::Field, "{}", r.hover);
        let (kind, _) = classify_at(&a, 4, col).expect("classifies");
        assert_eq!(kind, SemKind::Property);
    }

    #[test]
    fn match_arm_binders_are_indexed_as_locals() {
        // The arm's `m` is its own binding: hover on its use answers for the
        // payload binder, not the outer `m: String`.
        let src = "fn f(o: Option<Int64>, m: String) -> Int64 {\n\
                   \x20   return match o {\n\
                   \x20       Some(m) => m + 1,\n\
                   \x20       None => 0\n\
                   \x20   }\n\
                   }";
        let a = analyze(src);
        // Line 3, cursor on the arm body's `m`.
        let col = src.lines().nth(2).unwrap().find("=> m").unwrap() + 3 + 1;
        let r = resolve(&a, 3, col).expect("arm use resolves");
        assert!(
            r.hover.starts_with("let m") && !r.hover.contains("String"),
            "arm binder captured by the outer param: {}",
            r.hover
        );
        // The binder anchors at its pattern spelling, so go-to-def lands there.
        assert_eq!(r.target_line, 3);
    }

    /// The `Ok(e)` binder anchors at its own pattern spelling, never at a
    /// same-named use in an earlier arm's body (`0 - e`).
    #[test]
    fn an_arm_binder_anchors_at_its_own_pattern_not_an_earlier_arm_use() {
        let src = "fn main() -> Int64 {\n\
                   \x20   let r: Result<Int64, Int64> = Ok(2)\n\
                   \x20   return match r { Err(e) => 0 - e, Ok(e) => e + 1 }\n\
                   }";
        let a = analyze(src);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let mut cols: Vec<usize> = a
            .locals
            .iter()
            .filter(|b| b.name == "e")
            .map(|b| b.col)
            .collect();
        cols.sort();
        // Line 3 spells `e` four times; only the two pattern payloads bind.
        let l2 = src.lines().nth(2).unwrap();
        assert_eq!(
            cols,
            vec![l2.find("Err(").unwrap() + 5, l2.find("Ok(").unwrap() + 4]
        );
    }

    #[test]
    fn lambda_params_are_indexed_as_locals() {
        let src = "fn twice(f: fn(Int64) -> Int64, v: Int64) -> Int64 {\n\
                   \x20   return f(f(v))\n\
                   }\n\
                   fn main() -> Int64 {\n\
                   \x20   let x = 1\n\
                   \x20   return twice(x -> x + 1, x)\n\
                   }";
        let a = analyze(src);
        // The lambda body's `x` resolves to the lambda's own param on line 6,
        // not the outer `let x` on line 5.
        let l6 = src.lines().nth(5).unwrap();
        let col = l6.find("x -> x").unwrap() + 5 + 1;
        let r = resolve(&a, 6, col).expect("lambda body use resolves");
        assert_eq!(r.target_line, 6, "{}", r.hover);
    }

    #[test]
    fn class_completions_pick_the_declared_theme_not_the_first() {
        // Two sequence types are enumerated; the call declares `TwB`, so the
        // answers come from it, not from the first.
        let src = "type TwA = String where value =~ \"(red|blue)( (red|blue))*\"\n\
                   type TwB = String where value =~ \"(cat|dog)( (cat|dog))*\"\n\
                   fn cls(c: TwB) -> Int64 { return 0 }\n\
                   fn css() -> String { return \".dog{color:brown}\" }\n\
                   fn main() -> Int64 { return cls(\"dog\") }\n";
        let a = analyze(src);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let line = 5;
        let col = src.lines().nth(4).unwrap().find("\"dog\"").unwrap() + 2;
        let items = class_completions(&a, src, line, col).expect("completions");
        assert!(items.iter().any(|c| c.label == "dog"), "{items:?}");
        assert!(!items.iter().any(|c| c.label == "red"), "{items:?}");
        // Hover names the type it answered from.
        let hv = class_token_hover(&a, src, line, col).expect("hover").text;
        assert!(hv.contains("`TwB`"), "{hv}");
    }

    #[test]
    fn a_move_hint_anchors_at_the_taking_occurrence_on_a_double_use_line() {
        // Both uses of `a` are on line 4; the move is the last one, inside
        // `take`, so the hint lands after it.
        let src = "fn take(s: consume String) -> Int64 { return 1 }\n\
                   fn main() -> Int64 {\n\
                   \x20   let a = \"x\" + \"y\"\n\
                   \x20   print(a); take(a)\n\
                   \x20   return 0\n\
                   }";
        let mut a = analyze(src);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        a.memory = vec![MemoryNote {
            name: "a".into(),
            line: 3,
            text: "moved at line 4 into `take(..)`".into(),
            last_use: Some(4),
            moved_into: Some("`take(..)`".into()),
        }];
        let hints = inlay_hints(&a);
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert_eq!(hints[0].line, 4);
        let l4 = src.lines().nth(3).unwrap();
        // end_col of the last `a` on the line.
        let expected_col = l4.rfind('a').unwrap() + 2;
        assert_eq!(hints[0].col, expected_col, "{hints:?}");
    }
}
