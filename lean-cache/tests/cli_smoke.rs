use std::process::Command;

#[test]
fn help_runs() {
    let bin = env!("CARGO_BIN_EXE_lean-cache");
    let out = Command::new(bin).output().expect("exec");
    // --help não é suportado; comando desconhecido retorna uso.
    assert!(String::from_utf8_lossy(&out.stdout).contains("Uso: lean-cache"));
}

#[test]
fn hash_is_deterministic() {
    use std::fs;
    let dir = std::env::temp_dir().join("lean-cache-smoke");
    let _ = fs::create_dir_all(&dir);
    let file = dir.join("a.lean");
    fs::write(&file, "def x := 1\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_lean-cache");
    let out1 = Command::new(bin).args(["hash", file.to_str().unwrap()]).output().unwrap();
    let out2 = Command::new(bin).args(["hash", file.to_str().unwrap()]).output().unwrap();
    assert_eq!(out1.stdout, out2.stdout);
}
