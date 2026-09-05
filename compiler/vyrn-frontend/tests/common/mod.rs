//! Running a linked program from an integration test, on the compiled route
//! (RFC-0125 §3 M5, the `library-run` row).

/// Compile `program` and run its `main`, as `vyrn run --engine wasm` does.
///
/// The answer is `main`'s return value, which the guest hands to `proc_exit` and
/// the host reports as the exit code — a byte, like any process's. A trap writes
/// an `error: ..` line on the guest's standard error and comes back as `Err`:
/// the split `vyrn run` makes for a program and `Resident::call_body` makes for
/// a test body.
pub fn run_compiled(program: &vyrn_frontend::ast::Program) -> Result<i64, String> {
    let bytes = vyrn_codegen::direct::compile(program)?;
    let out = vyrn_cli::wasmrun::run(
        &bytes,
        vyrn_cli::wasmrun::Run {
            argv: vec!["main.vyrn".to_string()],
            stdin_prefix: Vec::new(),
            capture_stdout: false,
            capture_stderr: true,
            meter: false,
        },
    )?;
    let said = String::from_utf8_lossy(&out.stderr);
    match said.rfind("error: ") {
        Some(at) if at == 0 || said.as_bytes()[at - 1] == b'\n' => {
            Err(said[at + 7..].trim_end().to_string())
        }
        _ => {
            eprint!("{said}");
            Ok(i64::from(out.code))
        }
    }
}
