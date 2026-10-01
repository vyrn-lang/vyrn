//! Runs a linked program from an integration test on the compiled route.

/// Compiles `program` and runs its `main`, as `vyrn run` does.
///
/// Returns `main`'s return value as the process exit code, a byte. A trap's
/// `error: ..` line on the guest's stderr comes back as `Err`, the split
/// `Resident::call_body` makes for a test body.
pub fn run_compiled(program: &vyrn_frontend::ast::Program) -> Result<i64, String> {
    let bytes = vyrn_codegen::direct::compile(program, vyrn_lower::analyze(program))?;
    let out = vyrn_cli::wasmrun::run(
        &bytes,
        vyrn_cli::wasmrun::Run {
            argv: vec!["main.vyrn".to_string()],
            capture_stderr: true,
            ..Default::default()
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
