//! Where the emitted wasm meets the toolchain that turns it into a binary.
//!
//! One emitter and one native route (RFC-0125 §2.5): the module this crate
//! writes is fed to wasm2c, and the C that comes back is compiled by clang
//! against the wasi sysroot beside [`WASI_HOST_C`], the host that gives it a
//! `main`. The pins that find those tools sit here rather than in the driver
//! because RFC-0076's wasm generation engine is an EXCLUDED crate the driver
//! may only depend on optionally, so it cannot reach back into the driver for
//! them, and the driver cannot reach into it. This crate is the nearest place
//! both already depend on.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The dev-tree wasi sysroot, if one exists: the first `tools/wasi-sysroot-*`
/// directory found walking up from `start` (sorted, so the pick is
/// deterministic when several versions are unpacked side by side).
pub fn tools_wasi_sysroot_from(start: &Path) -> Option<std::path::PathBuf> {
    for dir in start.ancestors() {
        let tools = dir.join("tools");
        if !tools.is_dir() {
            continue;
        }
        // A `tools/` directory that passes `is_dir()` but cannot be listed (an
        // ACL denial, a race with deletion) is skipped, not fatal: ending the
        // walk here would hide a valid tool installed at a higher ancestor.
        let Ok(entries) = std::fs::read_dir(&tools) else {
            continue;
        };
        let mut hits: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("wasi-sysroot"))
            })
            .collect();
        hits.sort();
        if let Some(hit) = hits.into_iter().next() {
            return Some(hit);
        }
    }
    None
}

/// A variable naming a path, honoured only when the path is there — step 1 of
/// the order for every tool.
fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
}

/// Step 2 of the order for any tool: the unpacked `~/.vyrn/tools/<sha>/` a pin
/// resolves to, or `Ok(None)` when the `vyrn.json` governing `start` pins no
/// such tool. A pin that cannot be resolved is `Err`, never a fall-through.
///
/// The pin is the project's, so it is read the way every other manifest rule is
/// read: by walking up from `start` to the `vyrn.json` that governs it.
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

/// A wasmtime executable to run a module with, and WHY that one — the discovery
/// order RFC-0102 M1 defines:
///
///   1. `$VYRN_WASMTIME`, the explicit escape hatch, which reports itself.
///   2. The pin: `toolchain.wasmtime` in `vyrn.json`, resolved through
///      `vyrn.lock` to a hash and through vendor/cache to an unpacked directory.
///      A pinned tool that cannot be resolved is `Err` — never a fall-through to
///      PATH, because the whole value of a pin is that its absence is loud.
///   3. The `tools/` walk, ONLY when the project declares no pin, so a clone of
///      a project that never pinned anything behaves exactly as it did.
///
/// `Ok(None)` is "nothing pinned and nothing found", which stays a SKIP: nothing
/// here needs wasmtime to BUILD, only to check its own output.
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

/// The refusal for a pinned archive that resolved, unpacked, and turned out not
/// to hold what the tool is for. It is an `Err` rather than a fall-through for
/// the same reason every other pin failure is.
fn unpacked_without(tool: &str, dir: &Path, what: &str) -> String {
    format!(
        "the pinned {tool} archive unpacked to {} with no {what} in it",
        dir.display()
    )
}

/// A wasi sysroot directory, and WHY that one — [`wasmtime_from`]'s order, for
/// the tool `--sysroot=` points at (RFC-0102 M2).
///
/// The pinned answer is the directory a consumer actually points clang at, not
/// the `<sha>` above it: `wasi-sysroot-25.0.tar.gz` unpacks to a version-named
/// directory, and `include/` is the marker that finds it either way.
pub fn wasi_sysroot_from(start: &Path) -> Result<Option<(PathBuf, &'static str)>, String> {
    if let Some(p) = env_path("WASI_SYSROOT") {
        return Ok(Some((p, "override: environment")));
    }
    if let Some(dir) = pinned_tool_dir(start, "wasi-sysroot")? {
        let root = vyrn_frontend::toolpin::tool_root(&dir, "include")
            .ok_or_else(|| unpacked_without("wasi-sysroot", &dir, "`include` directory"))?;
        return Ok(Some((root, "pinned")));
    }
    Ok(tools_wasi_sysroot_from(start).map(|p| (p, "discovered: tools/")))
}

/// `libclang_rt.builtins-wasm32.a`, and WHY that one — the same order again.
///
/// Step 3 is the one place this tool differs: with no pin the archive is found
/// *next to* the sysroot, which is the wasi-sdk release layout the `tools/`
/// convention reproduces, so the sysroot already chosen is what the walk starts
/// from.
pub fn wasi_builtins_from(
    start: &Path,
    sysroot: &Path,
) -> Result<Option<(PathBuf, &'static str)>, String> {
    if let Some(p) = env_path("WASI_BUILTINS") {
        // A link line needs the `.a`, but the variable is named for the tool and
        // the tool ships as a directory, so both spellings arrive: CI exports the
        // file, and a developer who exports the unpacked directory used to get
        // `wasm-ld: is a directory` from clang. Same two levels as the pin.
        let lib = p
            .is_dir()
            .then(|| vyrn_frontend::toolpin::tool_file(&p, BUILTINS_A))
            .flatten()
            .unwrap_or(p);
        return Ok(Some((lib, "override: environment")));
    }
    if let Some(dir) = pinned_tool_dir(start, "wasi-builtins")? {
        let lib = vyrn_frontend::toolpin::tool_file(&dir, BUILTINS_A)
            .ok_or_else(|| unpacked_without("wasi-builtins", &dir, BUILTINS_A))?;
        return Ok(Some((lib, "pinned")));
    }
    Ok(builtins_near_sysroot(sysroot).map(|p| (p, "discovered: tools/")))
}

/// [`wasi_sysroot_from`]'s answer for callers that only want the path; panics on
/// an unresolvable pin, for the reason [`find_wasmtime_from`] does.
pub fn find_wasi_sysroot_from(start: &Path) -> Option<PathBuf> {
    match wasi_sysroot_from(start) {
        Ok(found) => found.map(|(p, _)| p),
        Err(e) => panic!("{e}"),
    }
}

/// [`wasi_builtins_from`]'s answer for callers that only want the path.
pub fn find_wasi_builtins_from(start: &Path, sysroot: &Path) -> Option<PathBuf> {
    match wasi_builtins_from(start, sysroot) {
        Ok(found) => found.map(|(p, _)| p),
        Err(e) => panic!("{e}"),
    }
}

/// [`wasmtime_from`]'s answer for the callers that only want the path.
///
/// A pin that cannot be resolved panics rather than reading as "not installed",
/// for the reason [`require_tools`] panics: a run that silently skips the checks
/// a tool exists for is a green run that proves nothing.
pub fn find_wasmtime_from(start: &Path) -> Option<PathBuf> {
    match wasmtime_from(start) {
        Ok(found) => found.map(|(p, _)| p),
        Err(e) => panic!("{e}"),
    }
}

/// The dev-tree wasmtime: the first `tools/wasmtime-*/wasmtime` found walking up
/// from `start` (sorted, so the pick is deterministic when several versions are
/// unpacked side by side). Step 3 of the order — consulted only when nothing is
/// pinned.
fn discovered_wasmtime_from(start: &Path) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "wasmtime.exe"
    } else {
        "wasmtime"
    };
    discovered_tool_from(start, Path::new(exe))
}

/// The `tools/` walk every discovered tool takes: the first `tools/*/<rel>` that
/// exists, walking up from `start`, the entries of each `tools/` sorted so the
/// pick is deterministic when several versions are unpacked side by side.
///
/// Then the same walk from the RUNNING COMPILER, which is what makes a toolchain
/// unpacked beside `vyrn` reach a program compiled anywhere. The source's
/// ancestors come first, so a project that carries its own `tools/` still
/// decides; the compiler's are the fallback, and `shim_wasm` has taken them for
/// the sysroot since RFC-0102. The route needs this and the textual one did not:
/// clang is on `PATH` and wabt is not, so `vyrn build` on a file in a temp
/// directory found the compiler and then said `could not find wasm2c`.
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
        // Same rule as the sysroot walk: an unlistable `tools/` is skipped, not
        // fatal — the walk continues at the ancestors above it.
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

/// The wasm2c route's tools (RFC-0125 §2.5, measured in §3 M1; PLAN-0125-runtime
/// §6 step 3): `wasm2c` from a wabt release, with the wasm-rt headers and
/// runtime sources that release lays out around it.
///
/// Discovered, never pinned, the way clang is: `$VYRN_WASM2C` names the
/// executable, else the `tools/` walk finds `tools/wabt-*/bin/wasm2c`. The
/// version is what `wasm2c --version` prints, recorded by `vyrn deps` beside
/// clang's. CI has no wabt, so a consumer that finds nothing skips, as the
/// native route skips without clang.
pub struct Wasm2c {
    pub exe: PathBuf,
    pub version: String,
    /// `include/`, where `wasm-rt.h` is.
    pub include: PathBuf,
    /// `share/wabt/wasm2c/`: `wasm-rt-impl.h`, `wasm-rt-impl.c` and
    /// `wasm-rt-mem-impl.c`, compiled into every binary the route links.
    pub runtime: PathBuf,
    pub why: &'static str,
}

/// The wabt release layout around a `wasm2c` executable: `bin/wasm2c`,
/// `include/wasm-rt.h`, `share/wabt/wasm2c/wasm-rt-impl.c`. An executable with
/// no runtime beside it is `Err`, not "absent": the route cannot link without
/// the runtime, and a silent fall-through would report the tool missing when
/// it is there.
pub fn wasm2c_from(start: &Path) -> Result<Option<Wasm2c>, String> {
    let exe = if cfg!(windows) {
        "wasm2c.exe"
    } else {
        "wasm2c"
    };
    let rel = Path::new("bin").join(exe);
    let (exe, why) = match env_path("VYRN_WASM2C") {
        Some(p) => (p, "override: environment"),
        // The pin, then the `tools/` walk — `wasmtime_from`'s order (RFC-0102
        // M2), which the route's tools joined when the route became the only
        // one (RFC-0125 §2.5).
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
            "the wasm2c at {} has no wabt release layout around it (include/wasm-rt.h and              share/wabt/wasm2c/wasm-rt-impl.c beside bin/); the route links that runtime",
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

/// simde, the header library wasm2c's output includes for SIMD (RFC-0075's
/// lanes are `v128` in the C): the directory that holds `simde/`, so that
/// `-I<dir>` resolves `<simde/wasm/simd128.h>`. `$VYRN_SIMDE` names it, else
/// the `tools/` walk finds `tools/simde/`. The version is not probed: a header
/// library reports none, and the release is recorded in RFC-0125 §3 M1.
pub fn simde_from(start: &Path) -> Option<(PathBuf, &'static str)> {
    let marker = Path::new("simde").join("wasm").join("simd128.h");
    if let Some(p) = env_path("VYRN_SIMDE").filter(|p| p.join(&marker).exists()) {
        return Some((p, "override: environment"));
    }
    // The pin, then the walk. A pin that will not resolve panics rather than
    // reading as "not installed", for the reason `find_wasmtime_from` does.
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

/// The WASI host the wasm2c route links (RFC-0125 §2.4's two-hundred-line
/// host): the fifteen imports `direct.rs` declares, each doing what the CLI's
/// embedded engine does in `wasmrun.rs`. `VYRN_W2C_HEADER` is defined by the
/// driver to the header wasm2c wrote.
pub const WASI_HOST_C: &str = include_str!("wasi_host.c");

/// The marker [`wasi_host_c`] fills with the `vyrn` namespace's stubs.
const EXTERN_STUBS_MARKER: &str = "/*@VYRN_EXTERN_STUBS@*/";

/// [`WASI_HOST_C`] with RFC-0012's `vyrn` namespace filled in, read off the
/// header wasm2c just wrote.
///
/// A reached `extern` must fail the same way on every engine (RFC-0125 §3 M5,
/// the `extern-unavailable` row): the embedded engine answers each name in the
/// namespace with `trap::extern_unavailable`'s sentence on fd 2 and exit 1, and
/// a native binary has no host to answer with anything else. The text-IR route
/// wrote one nullary C stub per declaration and let the linker reconcile it;
/// wasm2c writes a PROTOTYPE, so a stub with the wrong arity does not link.
///
/// So the signatures come from the header rather than from a second reading of
/// the declarations. The one place that knows how a `String` argument crosses
/// this boundary is `direct::extern_abi_sig`, and a transcription of it here
/// would be the second chance to make the same mistake that this file's
/// neighbours keep refusing. The header is machine-written and its types are
/// wasm-rt's four, so the transform is textual: name each parameter, keep the
/// return type, and give the body the refusal.
///
/// It also decides the arity of `wasm2c_prog_instantiate`, which takes one
/// argument per imported namespace. A program with no reachable `extern` has no
/// `vyrn` import at all — `Module::sweep` drops the ones nothing calls — so the
/// macro is what lets one host source serve both shapes.
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

/// A wasm2c C symbol back to the name the module imported. wasm2c writes any
/// byte outside `[A-Za-z0-9_]` as `0x` and two upper-case hex digits — `_start`
/// is `0x5Fstart` — and a Vyrn identifier may hold one: the lexer accepts every
/// `is_alphabetic` char, so `δata` is a legal name and its UTF-8 bytes arrive
/// here escaped.
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

/// Turn a missing tool from a SKIP into a failure when `VYRN_REQUIRE_TOOLS` is
/// set, and return it unchanged otherwise.
///
/// Every check in this repo that needs an external binary degrades quietly when
/// it is absent — no `wasmtime` and the wasm column disappears with a `NOTE`,
/// and the run still passes with less checked than its name says. That is right
/// on a developer's machine, where the tool is genuinely optional. It is wrong
/// in CI, where the tool is fetched on purpose and a cache that restored an
/// empty directory, a renamed release asset or a typo in an exported path all
/// read as green.
///
/// So the decision is the CALLER's environment, made once: CI exports
/// `VYRN_REQUIRE_TOOLS=1` and a missing tool stops the build, saying which one
/// and which variable points at it. This lives here rather than in a test
/// harness because two harnesses need it — `vyrn-cli/tests/common` and
/// `vyrn-codegen/tests` — and a rule with two copies is a rule with two
/// answers.
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

/// The one file the builtins archive exists to deliver, named once: the pinned
/// resolver looks for it inside an unpacked blob and the `tools/` walk looks for
/// it beside a sysroot, and two spellings would be two answers.
pub const BUILTINS_A: &str = "libclang_rt.builtins-wasm32.a";

/// `libclang_rt.builtins-wasm32.a` from a `libclang_rt.builtins-wasm32-wasi-*`
/// directory next to the sysroot (the wasi-sdk release-artifact layout),
/// version-agnostic and deterministic (sorted).
pub fn builtins_near_sysroot(sysroot: &Path) -> Option<std::path::PathBuf> {
    let parent = sysroot.parent()?;
    let mut hits: Vec<std::path::PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|e| e.path().join(BUILTINS_A))
        .filter(|p| {
            p.exists()
                && p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("libclang_rt.builtins-wasm32"))
        })
        .collect();
    hits.sort();
    hits.into_iter().next()
}

/// The Windows last resort, spelled once: the path and the reason that names it
/// are the same string, because a reason that disagrees with the path it
/// explains is worse than no reason at all.
macro_rules! windows_clang {
    () => {
        r"C:\Program Files\LLVM\bin\clang.exe"
    };
}

/// Locate a clang executable: `$CLANG`, then PATH, then the default Windows
/// install location.
pub fn find_clang() -> Option<PathBuf> {
    clang_from().map(|(p, _, _)| p)
}

/// The clang a build will run, the version it reports, and WHY that one —
/// [`wasmtime_from`]'s shape for the one tool RFC-0102 does not pin.
///
/// clang stays discovered because a native clang links against the host's libc,
/// linker and system libraries: there is no portable tarball that produces a
/// working native binary everywhere, and a pin that failed at LINK time instead
/// of at resolve time would be worse than no pin. So it is recorded rather than
/// pinned — the version is captured, it enters [`shim_wasm`]'s cache key, and
/// `vyrn deps` prints all three columns.
///
/// Memoized for the process: `--version` is a spawn, this is called on the shim
/// cache's hit path, and the answer cannot change under a running compiler. It
/// is also strictly cheaper than what it replaces, which spawned the same probe
/// on every call.
pub fn clang_from() -> Option<(PathBuf, String, &'static str)> {
    static FOUND: std::sync::OnceLock<Option<(PathBuf, String, &'static str)>> =
        std::sync::OnceLock::new();
    FOUND.get_or_init(discover_clang).clone()
}

/// The first line of `clang --version`, trimmed, or `unknown` when the probe
/// says nothing. Whatever the vendor prints is the version: Apple, Ubuntu and
/// upstream all word that line differently, and normalizing it here would be
/// this repository inventing a version number for a compiler it did not build.
fn clang_version(exe: &Path) -> String {
    Command::new(exe)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| first_line(&o.stdout))
        .unwrap_or_else(|| UNKNOWN_VERSION.to_string())
}

/// What a version column says when nothing knows the answer. One spelling, used
/// by the probe here and by `vyrn deps` for every tool that reports no version.
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
    // Trust PATH: if `clang --version` runs, use the bare name. The output was
    // thrown away until RFC-0102 M3; only the pipe is new.
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

    /// The host's `vyrn` namespace, both ways round (RFC-0012). A module with no
    /// `extern` import gets the two-argument instantiate and no stub; a module
    /// with one gets the three-argument instantiate, the struct the host must
    /// define, and a stub at the header's own arity — including the `String`
    /// argument that crosses as two values, which is the fact a transcription
    /// of the ABI here would be free to get wrong.
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
        // The refusal is the one every other engine prints, and only the
        // non-void stub returns.
        assert!(some.contains("error: extern `jsNow` is not available on this target\\n"));
        assert!(
            some.contains("    exit(1);\n    return 0;\n}"),
            "f64 returns"
        );
        assert!(some.contains("    exit(1);\n}"), "void does not");
        // A forward declaration is not a prototype and must not become a stub.
        assert!(!some.contains("struct w2c_vyrn; {"));
    }

    /// A Vyrn identifier may hold a byte wasm2c escapes: the lexer accepts every
    /// `is_alphabetic` char, so `δata` is a legal `extern fn` name and the
    /// refusal must spell it the way the other engines do.
    #[test]
    fn a_mangled_import_name_comes_back_as_the_name_the_program_wrote() {
        assert_eq!(demangle_w2c("jsAdd"), "jsAdd");
        assert_eq!(demangle_w2c("0x5Fstart"), "_start");
        assert_eq!(demangle_w2c("0xCE0xB4ata"), "δata");
    }

    /// The rule two test harnesses depend on, checked rather than assumed: with
    /// `VYRN_REQUIRE_TOOLS` set a missing tool PANICS, and without it the same
    /// call is a quiet `None` the caller may skip on. A `require_tools` that
    /// silently returned `None` under the variable would give every gate that
    /// uses it the failure mode it was written to remove.
    ///
    /// Serial by construction: it is one test, and it puts the variable back.
    #[test]
    fn require_tools_fails_loud_only_when_the_environment_asks() {
        let saved = std::env::var_os("VYRN_REQUIRE_TOOLS");
        std::env::remove_var("VYRN_REQUIRE_TOOLS");
        assert!(require_tools("nothing", "NOTHING", None).is_none());

        std::env::set_var("VYRN_REQUIRE_TOOLS", "1");
        let missing = std::panic::catch_unwind(|| require_tools("nothing", "NOTHING", None));
        assert!(missing.is_err(), "a missing tool must not skip quietly");
        // A tool that IS there is handed back either way.
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

    /// The dev-tree toolchain discovery: `tools/wasi-sysroot-*` found from any
    /// ancestor of the starting dir, builtins found version-agnostically next
    /// to the sysroot, and both absent on a layout without the convention.
    #[test]
    fn wasi_toolchain_discovery_walks_the_tools_convention() {
        let root = std::env::temp_dir().join(format!("vyrn_tools_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sysroot = root.join("tools/wasi-sysroot-25.0");
        let builtins_dir = root.join("tools/libclang_rt.builtins-wasm32-wasi-25.0");
        let deep = root.join("compiler/target/release");
        std::fs::create_dir_all(&sysroot).unwrap();
        std::fs::create_dir_all(&builtins_dir).unwrap();
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(builtins_dir.join("libclang_rt.builtins-wasm32.a"), b"x").unwrap();

        let found = tools_wasi_sysroot_from(&deep).expect("sysroot discovered from exe dir");
        assert_eq!(found, sysroot);
        let b = builtins_near_sysroot(&found).expect("builtins discovered next to sysroot");
        assert!(b.ends_with("libclang_rt.builtins-wasm32.a"));

        // No convention → no discovery (never invent a path).
        let bare = root.join("elsewhere/deeper");
        std::fs::create_dir_all(&bare).unwrap();
        let _ = std::fs::remove_dir_all(root.join("tools"));
        assert!(tools_wasi_sysroot_from(&bare).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
