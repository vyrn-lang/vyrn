//! Finds the tools that turn the emitted wasm into a binary: wasm2c, clang,
//! simde and wasmtime. The native route feeds the module to wasm2c and
//! compiles its C with clang beside [`WASI_HOST_C`]. This lives here, not in
//! the driver, because the wasm generation engine is an excluded
//! crate and this crate is the nearest one both depend on.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Step 1 of every tool's order: a variable naming a path, honoured only when
/// the path exists.
fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
}

/// Step 2 of every tool's order: the unpacked `~/.vyrn/tools/<sha>/` a pin
/// resolves to, or `Ok(None)` when the `vyrn.json` governing `start` pins no
/// such tool. A pin that cannot be resolved is `Err`, never a fall-through.
fn pinned_tool_dir(start: &Path, tool: &str) -> Result<Option<PathBuf>, String> {
    let Some(m) = vyrn_frontend::manifest::find(start)? else {
        return Ok(None);
    };
    let Some((_, version)) = m.toolchain.iter().find(|(n, _)| n == tool) else {
        return Ok(None);
    };
    let lock = vyrn_frontend::manifest::Lock::in_project(&m.dir)?;
    vyrn_frontend::toolpin::pinned_tool(Some(&m.dir), &lock, tool, version).map(Some)
}

/// Returns the wasmtime executable to run a module with, and why that one:
///
///   1. `$VYRN_WASMTIME`.
///   2. The pin: `toolchain.wasmtime` in `vyrn.json`, resolved through
///      `vyrn.lock`. An unresolvable pin is `Err`, never a fall-through: a pin's
///      absence must be loud.
///   3. The `tools/` walk, only when the project declares no pin.
///
/// `Ok(None)` means nothing pinned and nothing found, which callers treat as a
/// skip: wasmtime only checks output, it never builds.
pub fn wasmtime_from(start: &Path) -> Result<Option<(PathBuf, &'static str)>, String> {
    if let Some(p) = env_path("VYRN_WASMTIME") {
        return Ok(Some((p, "override: environment")));
    }
    if let Some(dir) = pinned_tool_dir(start, "wasmtime")? {
        let exe = vyrn_frontend::toolpin::tool_binary(&dir, "wasmtime")
            .ok_or_else(|| unpacked_without("wasmtime", &dir, "a wasmtime binary"))?;
        return Ok(Some((exe, "pinned")));
    }
    Ok(discovered_wasmtime_from(start).map(|p| (p, "discovered: tools/")))
}

/// The refusal for a pinned archive that unpacked without the file the tool
/// needs.
fn unpacked_without(tool: &str, dir: &Path, what: &str) -> String {
    format!(
        "the pinned {tool} archive unpacked to {} with no {what} in it",
        dir.display()
    )
}

/// Returns [`wasmtime_from`]'s path alone.
///
/// # Panics
///
/// If a pin cannot be resolved: a run that skips the checks silently proves
/// nothing, the reason [`require_tools`] panics.
pub fn find_wasmtime_from(start: &Path) -> Option<PathBuf> {
    match wasmtime_from(start) {
        Ok(found) => found.map(|(p, _)| p),
        Err(e) => panic!("{e}"),
    }
}

/// Step 3 of the order: the first `tools/*/wasmtime` found by [`discovered_tool_from`].
fn discovered_wasmtime_from(start: &Path) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "wasmtime.exe"
    } else {
        "wasmtime"
    };
    discovered_tool_from(start, Path::new(exe))
}

/// The first `tools/*/<rel>` that exists walking up from `start`, else walking
/// up from the running compiler. Each `tools/` is read sorted, so the pick is
/// deterministic when several versions sit side by side.
///
/// The source's ancestors come first, so a project's own `tools/` decides. The
/// compiler's ancestors let a program anywhere find the wabt unpacked beside
/// `vyrn`; wabt, unlike clang, is not on `PATH`.
fn discovered_tool_from(start: &Path, rel: &Path) -> Option<PathBuf> {
    tools_walk(start, rel).or_else(|| {
        let exe = std::env::current_exe().ok()?;
        tools_walk(exe.parent()?, rel)
    })
}

fn tools_walk(start: &Path, rel: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let tools = dir.join("tools");
        if !tools.is_dir() {
            continue;
        }
        // An unlistable `tools/` is skipped, not fatal.
        let Ok(entries) = std::fs::read_dir(&tools) else {
            continue;
        };
        let mut hits: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join(rel))
            .filter(|p| p.exists())
            .collect();
        hits.sort();
        if let Some(hit) = hits.into_iter().next() {
            return Some(hit);
        }
    }
    None
}

/// `wasm2c` from a wabt release, with the wasm-rt headers and runtime sources
/// that release lays out around it.
pub struct Wasm2c {
    pub exe: PathBuf,
    /// The first line `wasm2c --version` prints.
    pub version: String,
    /// `include/`, where `wasm-rt.h` is.
    pub include: PathBuf,
    /// `share/wabt/wasm2c/`: `wasm-rt-impl.h`, `wasm-rt-impl.c` and
    /// `wasm-rt-mem-impl.c`, compiled into every binary the route links.
    pub runtime: PathBuf,
    pub why: &'static str,
}

/// Finds wasm2c: `$VYRN_WASM2C`, else the `wabt` pin, else the `tools/` walk.
/// `Ok(None)` means none was found; callers skip, as the native route skips
/// without clang.
///
/// # Errors
///
/// If a pin cannot be resolved, or the executable has no wabt release layout
/// (`include/wasm-rt.h`, `share/wabt/wasm2c/wasm-rt-impl.c`) around it: the
/// route cannot link without the runtime.
pub fn wasm2c_from(start: &Path) -> Result<Option<Wasm2c>, String> {
    let exe = if cfg!(windows) {
        "wasm2c.exe"
    } else {
        "wasm2c"
    };
    let rel = Path::new("bin").join(exe);
    let (exe, why) = match env_path("VYRN_WASM2C") {
        Some(p) => (p, "override: environment"),
        None => match pinned_tool_dir(start, "wabt")? {
            Some(dir) => match vyrn_frontend::toolpin::tool_root(&dir, "bin") {
                Some(root) => (root.join(&rel), "pinned"),
                None => return Err(unpacked_without("wabt", &dir, "`bin` directory")),
            },
            None => match discovered_tool_from(start, &rel) {
                Some(p) => (p, "discovered: tools/"),
                None => return Ok(None),
            },
        },
    };
    let root = exe
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let include = root.join("include");
    let runtime = root.join("share").join("wabt").join("wasm2c");
    if !include.join("wasm-rt.h").exists() || !runtime.join("wasm-rt-impl.c").exists() {
        return Err(format!(
            "the wasm2c at {} has no wabt release layout around it (include/wasm-rt.h and \
              share/wabt/wasm2c/wasm-rt-impl.c beside bin/); the route links that runtime",
            exe.display()
        ));
    }
    let version = Command::new(&exe)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| first_line(&o.stdout))
        .unwrap_or_else(|| UNKNOWN_VERSION.to_string());
    Ok(Some(Wasm2c {
        exe,
        version,
        include,
        runtime,
        why,
    }))
}

/// Finds simde, the header library wasm2c's SIMD output includes: the
/// directory that holds `simde/`, so `-I<dir>` resolves
/// `<simde/wasm/simd128.h>`. Order: `$VYRN_SIMDE`, the pin, the `tools/` walk.
/// A header library reports no version, so none is probed.
///
/// # Panics
///
/// If a pin cannot be resolved, for the reason [`find_wasmtime_from`] panics.
pub fn simde_from(start: &Path) -> Option<(PathBuf, &'static str)> {
    let marker = Path::new("simde").join("wasm").join("simd128.h");
    if let Some(p) = env_path("VYRN_SIMDE").filter(|p| p.join(&marker).exists()) {
        return Some((p, "override: environment"));
    }
    match pinned_tool_dir(start, "simde") {
        Ok(Some(dir)) => {
            if let Some(root) = vyrn_frontend::toolpin::tool_root(&dir, "simde") {
                return Some((root, "pinned"));
            }
        }
        Ok(None) => {}
        Err(e) => panic!("{e}"),
    }
    let hit = discovered_tool_from(start, &marker)?;
    // Back from `simde/wasm/simd128.h` to the directory that holds `simde/`.
    let dir = hit.ancestors().nth(3)?.to_path_buf();
    Some((dir, "discovered: tools/"))
}

/// The WASI host the wasm2c route links: the imports [`crate::WASI_IMPORTS`] lists, each
/// doing what the embedded engine does in `vyrn-genwasm/src/wasi.rs`. The driver defines
/// `VYRN_W2C_HEADER` to the header wasm2c wrote.
pub const WASI_HOST_C: &str = include_str!("wasi_host.c");

/// The marker [`wasi_host_c`] fills with the `vyrn` namespace's stubs.
const EXTERN_STUBS_MARKER: &str = "/*@VYRN_EXTERN_STUBS@*/";

/// Returns [`WASI_HOST_C`] with a stub for each `vyrn` import in
/// `w2c_header`, the header wasm2c wrote.
///
/// Each stub prints `trap::extern_unavailable`'s sentence and exits 1, as every
/// engine does for a reached `extern`. wasm2c writes prototypes, so a stub must
/// match its arity; the signatures come from the header, not from a second
/// copy of `direct::extern_abi_sig`.
///
/// `VYRN_INSTANTIATE` passes one argument per imported namespace, and a program
/// with no reachable `extern` has no `vyrn` import (`Module::sweep`).
pub fn wasi_host_c(w2c_header: &str) -> String {
    let mut stubs = String::new();
    for line in w2c_header.lines() {
        let line = line.trim();
        if !line.ends_with(");") {
            continue;
        }
        let Some(open) = line.find('(') else { continue };
        let Some((ret, sym)) = line[..open].rsplit_once(' ') else {
            continue;
        };
        let Some(mangled) = sym.strip_prefix("w2c_vyrn_") else {
            continue;
        };
        let name = demangle_w2c(mangled);
        let inner = line[open + 1..line.len() - 2].trim();
        let params: Vec<String> = if inner.is_empty() || inner == "void" {
            Vec::new()
        } else {
            inner
                .split(',')
                .enumerate()
                .map(|(i, t)| format!("{} a{i}", t.trim()))
                .collect()
        };
        stubs.push_str(&format!(
            "{ret} {sym}({}) {{\n",
            if params.is_empty() {
                "void".to_string()
            } else {
                params.join(", ")
            }
        ));
        for i in 0..params.len() {
            stubs.push_str(&format!("    (void)a{i};\n"));
        }
        stubs.push_str(&format!(
            "    fputs({:?}, stderr);\n    exit(1);\n",
            format!(
                "error: {}\n",
                vyrn_frontend::trap::extern_unavailable(&name)
            )
        ));
        if ret != "void" {
            stubs.push_str("    return 0;\n");
        }
        stubs.push_str("}\n");
    }
    let block = if stubs.is_empty() {
        "#define VYRN_INSTANTIATE(inst, wasi) wasm2c_prog_instantiate((inst), (wasi))\n".to_string()
    } else {
        format!(
            "struct w2c_vyrn {{\n    int unused;\n}};\nstatic struct w2c_vyrn g_vyrn;\n\
             #define VYRN_INSTANTIATE(inst, wasi) \
             wasm2c_prog_instantiate((inst), &g_vyrn, (wasi))\n{stubs}"
        )
    };
    WASI_HOST_C.replace(EXTERN_STUBS_MARKER, &block)
}

/// Turns a wasm2c C symbol back into the imported name. wasm2c writes any byte
/// outside `[A-Za-z0-9_]` as `0x` and two hex digits (`_start` is `0x5Fstart`),
/// and a Vyrn identifier may hold any `is_alphabetic` char.
fn demangle_w2c(sym: &str) -> String {
    let b = sym.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'0' && i + 3 < b.len() && b[i + 1] == b'x' {
            if let Ok(v) = u8::from_str_radix(&sym[i + 2..i + 4], 16) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Returns `found` unchanged, so a missing tool is a skip, unless
/// `VYRN_REQUIRE_TOOLS` is set. CI sets it, because there a tool is fetched on
/// purpose and its absence is a broken cache or path, not a choice.
///
/// # Panics
///
/// If `found` is `None` and `VYRN_REQUIRE_TOOLS` is set; the message names
/// `what` and the variable `var` that points at it.
pub fn require_tools(what: &str, var: &str, found: Option<PathBuf>) -> Option<PathBuf> {
    if found.is_none() && std::env::var_os("VYRN_REQUIRE_TOOLS").is_some() {
        panic!(
            "VYRN_REQUIRE_TOOLS is set and `{what}` was not found — this run would have \
             silently skipped the checks that need it. Point `{var}` at the binary, or \
             unset VYRN_REQUIRE_TOOLS to allow the skip."
        );
    }
    found
}

/// The Windows last resort, a macro so the path and the reason that names it
/// are one string.
macro_rules! windows_clang {
    () => {
        r"C:\Program Files\LLVM\bin\clang.exe"
    };
}

/// Returns a clang: `$CLANG`, then `PATH`, then the default Windows install.
pub fn find_clang() -> Option<PathBuf> {
    clang_from().map(|(p, _, _)| p)
}

/// Returns the clang a build runs, the version it reports, and why that one.
///
/// clang is recorded, never pinned: it links against the host's
/// libc and linker, so no portable archive builds everywhere. Memoized for the
/// process, because `--version` is a spawn on a hot path.
pub fn clang_from() -> Option<(PathBuf, String, &'static str)> {
    static FOUND: std::sync::OnceLock<Option<(PathBuf, String, &'static str)>> =
        std::sync::OnceLock::new();
    FOUND.get_or_init(discover_clang).clone()
}

/// The first line of `clang --version`, trimmed and not normalized, or
/// [`UNKNOWN_VERSION`].
fn clang_version(exe: &Path) -> String {
    Command::new(exe)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| first_line(&o.stdout))
        .unwrap_or_else(|| UNKNOWN_VERSION.to_string())
}

/// The version of a tool that reports none.
pub const UNKNOWN_VERSION: &str = "unknown";

fn first_line(out: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(out);
    let line = text.lines().next()?.trim().to_string();
    (!line.is_empty()).then_some(line)
}

fn discover_clang() -> Option<(PathBuf, String, &'static str)> {
    if let Ok(c) = std::env::var("CLANG") {
        let p = PathBuf::from(c);
        if p.exists() {
            let v = clang_version(&p);
            return Some((p, v, "override: environment"));
        }
    }
    // If `clang --version` runs from PATH, use the bare name.
    if let Ok(out) = Command::new("clang").arg("--version").output() {
        if out.status.success() {
            let v = first_line(&out.stdout).unwrap_or_else(|| UNKNOWN_VERSION.to_string());
            return Some((PathBuf::from("clang"), v, "discovered: PATH"));
        }
    }
    if cfg!(windows) {
        let default = PathBuf::from(windows_clang!());
        if default.exists() {
            let v = clang_version(&default);
            return Some((default, v, concat!("discovered: ", windows_clang!())));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No `extern` import: the two-argument instantiate and no stub. One: the
    /// three-argument instantiate, the struct, and a stub at the header's arity,
    /// where a `String` argument crosses as two values.
    #[test]
    fn the_host_takes_its_extern_stubs_from_the_header() {
        let none = wasi_host_c("void wasm2c_prog_instantiate(w2c_prog*, struct w2c_x*);\n");
        assert!(none.contains("wasm2c_prog_instantiate((inst), (wasi))"));
        assert!(!none.contains("struct w2c_vyrn {"));
        assert!(!none.contains(EXTERN_STUBS_MARKER));

        let some = wasi_host_c(
            "struct w2c_vyrn;\n\
             void w2c_vyrn_jsLog(struct w2c_vyrn*, u32, u64);\n\
             f64 w2c_vyrn_jsNow(struct w2c_vyrn*);\n",
        );
        assert!(some.contains("wasm2c_prog_instantiate((inst), &g_vyrn, (wasi))"));
        assert!(some.contains("struct w2c_vyrn {"));
        assert!(some.contains("void w2c_vyrn_jsLog(struct w2c_vyrn* a0, u32 a1, u64 a2) {"));
        assert!(some.contains("f64 w2c_vyrn_jsNow(struct w2c_vyrn* a0) {"));
        // Only the non-void stub returns.
        assert!(some.contains("error: extern `jsNow` is not available on this target\\n"));
        assert!(
            some.contains("    exit(1);\n    return 0;\n}"),
            "f64 returns"
        );
        assert!(some.contains("    exit(1);\n}"), "void does not");
        // A forward declaration is not a prototype and must not become a stub.
        assert!(!some.contains("struct w2c_vyrn; {"));
    }

    /// The C host defines each `crate::WASI_IMPORTS` call once, with its signature, and no other.
    /// wasm2c's header declares only the imports a program reaches, so the C compiler alone
    /// catches a missing or mistyped call only in a program that makes it.
    #[test]
    fn the_c_host_defines_exactly_the_declared_wasi_calls() {
        use crate::wasm::ValType;
        use std::collections::BTreeMap;
        let ty = |c: &str| match c {
            "uint32_t" => Some(ValType::I32),
            "uint64_t" => Some(ValType::I64),
            _ => None,
        };
        let prefix = "w2c_wasi__snapshot__preview1_";
        let mut defined = BTreeMap::new();
        for (head, rest) in WASI_HOST_C
            .split(prefix)
            .zip(WASI_HOST_C.split(prefix).skip(1))
        {
            let (name, rest) = rest.split_once('(').expect("a definition");
            let (params, _) = rest.split_once(") {").expect("a definition");
            let ret = head.rsplit('\n').next().unwrap_or_default().trim();
            let params: Vec<ValType> = params
                .split(',')
                .skip(1)
                .map(|p| ty(p.split_whitespace().next().unwrap_or_default()).expect(p))
                .collect();
            let old = defined.insert(name, (params, ty(ret).into_iter().collect()));
            assert!(old.is_none(), "{name} is defined twice");
        }
        let want: BTreeMap<_, (Vec<_>, Vec<_>)> = crate::WASI_IMPORTS
            .iter()
            .map(|(n, p, r)| (*n, (p.to_vec(), r.to_vec())))
            .collect();
        assert_eq!(defined, want);
    }

    /// A non-ASCII `extern fn` name is escaped by wasm2c, and the refusal must
    /// spell it as the other engines do.
    #[test]
    fn a_mangled_import_name_comes_back_as_the_name_the_program_wrote() {
        assert_eq!(demangle_w2c("jsAdd"), "jsAdd");
        assert_eq!(demangle_w2c("0x5Fstart"), "_start");
        assert_eq!(demangle_w2c("0xCE0xB4ata"), "δata");
    }

    /// One test, so the variable is never raced; it restores it on exit.
    #[test]
    fn require_tools_fails_loud_only_when_the_environment_asks() {
        let saved = std::env::var_os("VYRN_REQUIRE_TOOLS");
        std::env::remove_var("VYRN_REQUIRE_TOOLS");
        assert!(require_tools("nothing", "NOTHING", None).is_none());

        std::env::set_var("VYRN_REQUIRE_TOOLS", "1");
        let missing = std::panic::catch_unwind(|| require_tools("nothing", "NOTHING", None));
        assert!(missing.is_err(), "a missing tool must not skip quietly");
        let here = PathBuf::from(".");
        assert_eq!(
            require_tools("here", "HERE", Some(here.clone())),
            Some(here)
        );

        match saved {
            Some(v) => std::env::set_var("VYRN_REQUIRE_TOOLS", v),
            None => std::env::remove_var("VYRN_REQUIRE_TOOLS"),
        }
    }
}
