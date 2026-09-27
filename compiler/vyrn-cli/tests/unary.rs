//! A unary operator is type-preserving, so the expected type reaches its
//! operand: `let a: Float32 = -0.5` checks as `0.5` does.

use std::process::Command;

fn check(src: &str) -> String {
    let dir = std::env::temp_dir().join("vyrn-unary");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("u.vyrn");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n")
        + &String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// The positive half is the control: if it fails, the test proves nothing.
#[test]
fn a_negated_literal_gets_the_expected_type_its_positive_does() {
    for ty in ["Float32", "Float64", "Int64", "Int32", "Int16", "Int8"] {
        let lit = if ty.starts_with("Float") { "0.5" } else { "7" };
        let pos = check(&format!(
            "fn main() -> Int64 {{ let a: {ty} = {lit}  print(a) return 0 }}\n"
        ));
        assert!(
            pos.trim() == "ok" || pos.trim().is_empty(),
            "{ty}: the POSITIVE literal should check — if this fails the test proves nothing:\n{pos}"
        );
        let neg = check(&format!(
            "fn main() -> Int64 {{ let a: {ty} = -{lit}  print(a) return 0 }}\n"
        ));
        assert!(
            !neg.contains("declared") && !neg.contains("but initializer is"),
            "{ty}: `-{lit}` should get the same expected type as `{lit}`, got:\n{neg}"
        );
    }
}
