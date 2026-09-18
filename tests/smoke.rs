#[test]
fn version_prints() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_relais"))
        .arg("--version")
        .output()
        .expect("launch relais");
    assert!(out.status.success(), "relais --version failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().starts_with("relais "),
        "unexpected --version output: {stdout}"
    );
}

#[test]
fn stubs_exit_with_a_clear_message() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_relais"))
        .arg("run")
        .arg("--task")
        .arg("task.json")
        .output()
        .expect("launch relais");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not implemented yet"),
        "unexpected stub message: {stderr}"
    );
}
