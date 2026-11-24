use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fluxprod"))
}

fn with_config(text: &str, mode: &str) -> Output {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("cli-config.toml");
    fs::write(&path, text).unwrap();
    command()
        .args([mode, "--config"])
        .arg(path)
        .output()
        .unwrap()
}

#[test]
fn checks_packaged_config_without_hardware() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/fluxprod.toml");
    let output = command()
        .args(["--check-config", "--config"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Valid configuration"));
}

#[test]
fn help_shows_the_binary_and_default_config_path() {
    let output = command().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("Usage: fluxprod"), "{help}");
    assert!(help.contains("[default: /etc/fluxprod.toml]"), "{help}");
}

#[test]
fn invalid_config_fails_in_daemon_mode_before_hardware_access() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bad.toml");
    fs::write(&path, "[gpu]\nsource = 'auto'\nnonsense = 1\n").unwrap();
    let output = command().arg("--config").arg(&path).output().unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(78));
    assert!(error.contains("bad.toml"), "{error}");
    assert!(error.contains("nonsense"), "{error}");
}

#[test]
fn missing_config_has_configuration_exit_code_and_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("missing.toml");
    let output = command()
        .args(["--check-config", "--config"])
        .arg(path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing.toml"));
}

#[test]
fn read_once_with_disabled_slots_needs_no_hardware() {
    let output = with_config(
        "[cpu]\nsource = 'off'\n[gpu]\nsource = 'off'\n",
        "--read-once",
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "CPU: off\nGPU: off\n"
    );
}

#[test]
fn cannot_combine_inspection_modes() {
    let output = command()
        .args(["--check-config", "--read-once"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}
