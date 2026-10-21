//! The native route's cache under `~/.vyrn/cache/native/<key>/`.
//!
//! Three kinds of file do not depend on the program: wabt's two runtime
//! objects, the WASI host object, and a precompiled header of wasm-rt, libc
//! and simde. Compiling them again costs about 0.8 s of a 1.2 s build, so
//! [`Native::open`] keys a directory on everything that can change one of
//! them, and each entry is built once.
//!
//! The key hashes the full `clang --version`, the wasm2c version, simde's
//! `simde-common.h`, every flag, the header text, and every `wasm-rt*` file of
//! wabt's `include/` and runtime directories. A changed input is a new
//! directory, so a stale entry cannot be found. The host object also depends
//! on the program's header (`w2c_prog`'s layout), so its name hashes the host
//! text and that header.
//!
//! An entry is written to a temporary name and renamed, so a concurrent build
//! reads a whole file or none. A `.sum` file beside each entry records its
//! SHA-256 (the header: its length), and a mismatch or a missing entry
//! rebuilds. The header is not hashed, because that costs more than the build
//! saves; a flipped bit in it is caught only by clang, which rejects a
//! malformed header and also checks the system headers' sizes and times.

use super::{native_compile_flags, show_path, NativeTarget};
use std::path::{Path, PathBuf};
use std::process::Command;
use vyrn_frontend::hash::sha256_hex;

/// What every translation unit of a native build includes, precompiled.
const PCH_H: &str = "#include <assert.h>\n#include <math.h>\n#include <stdarg.h>\n\
#include <stddef.h>\n#include <string.h>\n#include <stdlib.h>\n#include \"wasm-rt.h\"\n\
#include <simde/wasm/simd128.h>\n";

/// One cache directory and the flags its entries were built with.
pub struct Native {
    clang: PathBuf,
    flags: Vec<String>,
    runtime: PathBuf,
    dir: PathBuf,
}

fn read(p: &Path) -> Result<Vec<u8>, ()> {
    std::fs::read(p).map_err(|e| eprintln!("error: cannot read {}: {e}", p.display()))
}

impl Native {
    /// Returns the cache directory for this toolchain, creating it.
    pub fn open(
        clang: &Path,
        w2c: &vyrn_codegen::toolchain::Wasm2c,
        simde: &Path,
        target: NativeTarget,
    ) -> Result<Native, ()> {
        let mut c = Command::new(clang);
        // `show_path` drops the `\\?\` prefix of a canonicalized start, which
        // clang's include search does not take.
        c.arg(format!("-I{}", show_path(&w2c.include)))
            .arg(format!("-I{}", show_path(&w2c.runtime)))
            .arg(format!("-I{}", show_path(simde)));
        // wasm-rt's guard-page handler maps a stack overflow to a trap: a vectored
        // exception handler on Windows, a signal handler on an alternate stack on
        // Linux and macOS. On Windows wasm-rt also counts calls in every prologue
        // unless told not to (2.5x on `benching.vyrn`'s "push 1000"); the handler
        // still catches the overflow without the counter.
        //
        // `WIN32_LEAN_AND_MEAN` halves the `windows.h` that `wasm-rt.h` includes
        // (about 330 ms of clang's header time); the generated code is the same.
        if cfg!(windows) {
            c.arg("-DWASM_RT_NONCONFORMING_UNCHECKED_STACK_EXHAUSTION=1");
            c.arg("-DWIN32_LEAN_AND_MEAN");
        }
        native_compile_flags(&mut c, target);
        let flags: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let version = Command::new(clang)
            .arg("--version")
            .output()
            .map_err(|e| eprintln!("error: failed to run clang ({}): {e}", clang.display()))?;
        let mut key = Vec::new();
        let mut part = |b: &[u8]| {
            key.extend((b.len() as u64).to_le_bytes());
            key.extend(b);
        };
        part(b"vyrn native cache 1");
        part(&version.stdout);
        part(w2c.version.as_bytes());
        part(&read(&simde.join("simde").join("simde-common.h"))?);
        part(flags.join("\n").as_bytes());
        part(PCH_H.as_bytes());
        for dir in [&w2c.include, &w2c.runtime] {
            let mut names: Vec<_> = std::fs::read_dir(dir)
                .map_err(|e| eprintln!("error: cannot read {}: {e}", dir.display()))?
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| n.starts_with("wasm-rt"))
                .collect();
            names.sort();
            for n in names {
                part(n.as_bytes());
                part(&read(&dir.join(&n))?);
            }
        }
        let dir = vyrn_frontend::manifest::native_cache_dir().join(&sha256_hex(&key)[..32]);
        std::fs::create_dir_all(&dir)
            .map_err(|e| eprintln!("error: cannot create {}: {e}", dir.display()))?;
        Ok(Native {
            clang: clang.to_path_buf(),
            flags,
            runtime: w2c.runtime.clone(),
            dir,
        })
    }

    /// A clang command that carries every compile flag of the key.
    pub fn cc(&self) -> Command {
        let mut c = Command::new(&self.clang);
        c.args(&self.flags);
        c
    }

    /// Returns the precompiled header, for `-include-pch`.
    pub fn pch(&self) -> Result<PathBuf, ()> {
        let h = self.dir.join("pre.h");
        let fill = |c: &mut Command| {
            c.arg("-c").arg("-x").arg("c-header").arg(&h);
        };
        if let Some(hit) = self.hit("pre.h.pch", false) {
            return Ok(hit);
        }
        place_header(&h)?;
        self.build("pre.h.pch", false, fill)
    }

    /// Returns the object of wabt's `<stem>.c`.
    pub fn runtime(&self, stem: &str) -> Result<PathBuf, ()> {
        self.object(
            &format!("{stem}.o"),
            &show_path(&self.runtime.join(format!("{stem}.c"))),
            &[],
        )
    }

    /// Returns the object of the WASI host, `host_c` with the text `text`.
    ///
    /// The host reads `w2c_prog` from the program's `header`, so the name
    /// hashes the header too, less the include guard that names the program.
    pub fn host(
        &self,
        host_c: &Path,
        text: &str,
        header: &str,
        define: &str,
    ) -> Result<PathBuf, ()> {
        let mut k = text.as_bytes().to_vec();
        for line in header.lines().filter(|l| !l.contains("_W2C_H_GENERATED_")) {
            k.push(b'\n');
            k.extend(line.as_bytes());
        }
        let name = format!("host-{}.o", &sha256_hex(&k)[..32]);
        self.object(&name, &show_path(host_c), &[define])
    }

    fn object(&self, name: &str, src: &str, extra: &[&str]) -> Result<PathBuf, ()> {
        if let Some(hit) = self.hit(name, true) {
            return Ok(hit);
        }
        self.build(name, true, |c| {
            c.arg("-c").arg(src).args(extra);
        })
    }

    /// Returns the entry `name` if it matches its `.sum`.
    fn hit(&self, name: &str, hashed: bool) -> Option<PathBuf> {
        let path = self.dir.join(name);
        let sum = std::fs::read_to_string(self.dir.join(format!("{name}.sum"))).ok()?;
        (digest(&path, hashed)? == sum).then_some(path)
    }

    /// Compiles into a temporary beside the entry, then renames it into place.
    /// If the rename fails (a peer holds the file open on Windows), the
    /// temporary is the result: valid, and unshared.
    fn build(
        &self,
        name: &str,
        hashed: bool,
        fill: impl FnOnce(&mut Command),
    ) -> Result<PathBuf, ()> {
        let (dest, tmp) = (
            self.dir.join(name),
            self.dir.join(format!("{name}.{}", tmp_suffix())),
        );
        let mut c = self.cc();
        fill(&mut c);
        c.arg("-o").arg(&tmp);
        match c.status() {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("error: clang exited with {s}");
                return Err(());
            }
            Err(e) => {
                eprintln!("error: failed to run clang ({}): {e}", self.clang.display());
                return Err(());
            }
        }
        let sum = digest(&tmp, hashed).ok_or_else(|| {
            eprintln!("error: clang wrote no {}", tmp.display());
        })?;
        if std::fs::rename(&tmp, &dest).is_err() {
            return Ok(tmp);
        }
        // A `.sum` that does not land leaves the entry unverified: the next build
        // rebuilds it.
        let sum_tmp = self.dir.join(format!("{name}.sum.{}", tmp_suffix()));
        if std::fs::write(&sum_tmp, sum).is_ok()
            && std::fs::rename(&sum_tmp, self.dir.join(format!("{name}.sum"))).is_err()
        {
            let _ = std::fs::remove_file(&sum_tmp);
        }
        Ok(dest)
    }
}

/// A temporary's suffix that no other writer, in this process or another,
/// holds at the same time: the process id and a per-process count.
fn tmp_suffix() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("tmp{}.{n}", std::process::id())
}

/// Writes [`PCH_H`] to `h` unless `h` already holds it.
///
/// Clang records `h`'s mtime in the precompiled header and refuses it once
/// `h` is newer, so a concurrent build must never rewrite a correct `h`: the
/// peer that built from it would fail. A missing `h` is linked into place from
/// a whole temporary, and the link fails rather than replaces where a peer
/// placed it first.
fn place_header(h: &Path) -> Result<(), ()> {
    let stale = match std::fs::read(h) {
        Ok(b) if b == PCH_H.as_bytes() => return Ok(()),
        Ok(_) => true,
        Err(_) => false,
    };
    let tmp = h.with_file_name(format!("pre.h.{}", tmp_suffix()));
    std::fs::write(&tmp, PCH_H)
        .map_err(|e| eprintln!("error: cannot write {}: {e}", tmp.display()))?;
    let placed = if stale {
        std::fs::rename(&tmp, h)
    } else {
        match std::fs::hard_link(&tmp, h) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => std::fs::rename(&tmp, h),
            _ => Ok(()),
        }
    };
    let _ = std::fs::remove_file(&tmp);
    placed.map_err(|e| eprintln!("error: cannot write {}: {e}", h.display()))
}

/// The SHA-256 of the file, or its length where hashing costs more than it saves.
fn digest(p: &Path, hashed: bool) -> Option<String> {
    if hashed {
        Some(sha256_hex(&std::fs::read(p).ok()?))
    } else {
        Some(std::fs::metadata(p).ok()?.len().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vyrn-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("pre.h")
    }

    #[test]
    fn a_correct_header_keeps_its_mtime() {
        let h = scratch("pch-keep");
        place_header(&h).unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let f = std::fs::File::options().write(true).open(&h).unwrap();
        f.set_modified(old).unwrap();
        drop(f);
        place_header(&h).unwrap();
        assert_eq!(std::fs::metadata(&h).unwrap().modified().unwrap(), old);
    }

    #[test]
    fn a_stale_header_is_replaced() {
        let h = scratch("pch-stale");
        std::fs::write(&h, "stale").unwrap();
        place_header(&h).unwrap();
        assert_eq!(std::fs::read(&h).unwrap(), PCH_H.as_bytes());
    }
}
