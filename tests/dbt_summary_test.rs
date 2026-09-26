//! Offline command-level contract: result removal always has recoverable raw output.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const RAW: &str = "dbt-fusion 2.0.0-preview.218\n  Succeeded [  0.04s] model analytics.good (table)\n    Skipped [  0.01s] model analytics.downstream (view)\n     Failed [  0.01s] model analytics.broken (table)\nFinished 'run' with 1 error for target 'dev' [1s]\nProcessed: 3 models\nSummary: 3 total | 1 success | 1 error | 1 skipped\n";
const ERROR: &str = "[error] Database Error in model broken\n  select missing_column\n         ^\n";

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rtk"));
    cmd.args(args)
        .current_dir(dir)
        .env(
            "PATH",
            format!(
                "{}:{}",
                dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("RTK_DB_PATH", dir.join("tracking.db"))
        .env("RTK_RECALL_DB", dir.join("recall.db"))
        .env_remove("RTK_RECALL")
        .env_remove("RTK_TEE")
        .env_remove("RTK_NO_TOML");
    cmd
}

fn setup_case(raw: &str, error: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("stdout.txt"), raw).unwrap();
    fs::write(dir.path().join("stderr.txt"), error).unwrap();
    let tool = dir.path().join("dbt");
    fs::write(&tool, "#!/bin/sh\nprintf '%s\\n' \"$@\" > argv.txt\ncat stdout.txt\ncat stderr.txt >&2\nexit \"${DBT_EXIT:-0}\"\n").unwrap();
    fs::set_permissions(tool, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn setup(raw: &str) -> tempfile::TempDir {
    setup_case(raw, ERROR)
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn summaries_preserve_diagnostics_exit_codes_and_recover_exact_raw() {
    for exit in [0, 7] {
        let dir = setup(RAW);
        let out = command(dir.path(), &["dbt", "run"])
            .env("DBT_EXIT", exit.to_string())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(exit));
        let text = stdout(&out);
        assert!(
            !text.contains("Succeeded") && !text.contains("Skipped"),
            "{text}"
        );
        assert!(text.contains("Failed") && text.contains("Summary: 3 total"));
        assert!(text.contains(ERROR), "{text}");
        let hash = text
            .split("[full output: rtk recall ")
            .nth(1)
            .unwrap()
            .split(']')
            .next()
            .unwrap();
        let recalled = command(dir.path(), &["recall", hash]).output().unwrap();
        assert!(recalled.status.success());
        assert_eq!(stdout(&recalled), format!("{RAW}{ERROR}"));
    }
}

#[test]
fn no_recovery_keeps_raw_instead_of_hiding_results() {
    let dir = setup(RAW);
    let out = command(dir.path(), &["dbt", "run"])
        .env("RTK_RECALL", "0")
        .output()
        .unwrap();
    assert_eq!(stdout(&out).trim_end(), format!("{RAW}{ERROR}").trim_end());
}

#[test]
fn inconsistent_summary_keeps_individual_results() {
    let dir = setup(&RAW.replace("1 success", "2 success"));
    let text = stdout(&command(dir.path(), &["dbt", "run"]).output().unwrap());
    assert!(text.contains("Succeeded") && text.contains("Skipped"));
    assert!(text.contains(ERROR));
}

#[test]
fn unavailable_recovery_store_keeps_raw_instead_of_hiding_results() {
    let dir = setup(RAW);
    let out = command(dir.path(), &["dbt", "run"])
        .env(
            "RTK_RECALL_DB",
            dir.path().join("stdout.txt").join("recall.db"),
        )
        .output()
        .unwrap();
    let text = stdout(&out);
    assert!(!text.contains("rtk recall"), "{text}");
    assert_eq!(text.trim_end(), format!("{RAW}{ERROR}").trim_end());
}

#[test]
fn absolute_executable_path_still_matches_the_dbt_filter() {
    let dir = setup(RAW);
    let tool = dir.path().join("dbt");
    let text = stdout(
        &command(dir.path(), &[tool.to_str().unwrap(), "run"])
            .output()
            .unwrap(),
    );
    assert!(!text.contains("Succeeded"), "{text}");
    assert!(text.contains("Summary: 3 total"), "{text}");
}

#[test]
fn no_toml_bypass_leaves_dbt_output_untouched() {
    let dir = setup(RAW);
    let out = command(dir.path(), &["dbt", "run"])
        .env("RTK_NO_TOML", "1")
        .output()
        .unwrap();
    assert_eq!(stdout(&out), RAW);
    assert_eq!(String::from_utf8(out.stderr).unwrap(), ERROR);
}

#[test]
fn flagged_commands_bypass_filtering_and_forward_arguments() {
    let dir = setup(RAW);
    let out = command(dir.path(), &["dbt", "run", "--select", "good"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out), RAW);
    assert_eq!(String::from_utf8(out.stderr).unwrap(), ERROR);
    assert_eq!(
        fs::read_to_string(dir.path().join("argv.txt")).unwrap(),
        "run\n--select\ngood\n"
    );
}

// Edited real captures: dbt Cloud CLI driving Fusion 2.0.5 on Databricks at
// full volume (93-model run, 446-test test). Production identifiers are
// replaced with generic nameNNN tokens; timestamps, counts, line shapes, and
// byte structure are preserved, so every strip rule and the summary guard see
// exactly what the real output looked like. The expected file includes the
// content-derived `[full output: rtk recall <hash>]` hint — regenerate it if
// recall id derivation ever changes.
const RUN_RAW: &str = include_str!("fixtures/dbt_cloud_run_raw.txt");
const RUN_ERR: &str = include_str!("fixtures/dbt_cloud_run_raw.stderr");
const RUN_EXPECTED: &str = include_str!("fixtures/dbt_cloud_run_expected.txt");
const TEST_RAW: &str = include_str!("fixtures/dbt_cloud_test_raw.txt");
const TEST_ERR: &str = include_str!("fixtures/dbt_cloud_test_raw.stderr");
const TEST_EXPECTED: &str = include_str!("fixtures/dbt_cloud_test_expected.txt");

fn assert_real_case(raw: (&str, &str, &str), subcommand: &str) {
    let dir = setup_case(raw.0, raw.1);
    let out = command(dir.path(), &["dbt", subcommand]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert_eq!(text, raw.2);
    let hash = text
        .split("[full output: rtk recall ")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    let recalled = command(dir.path(), &["recall", hash]).output().unwrap();
    assert!(recalled.status.success());
    assert_eq!(stdout(&recalled), format!("{}{}", raw.0, raw.1));
}

#[test]
fn real_edited_run_capture_filters_started_noise_and_elides_results() {
    assert_real_case((RUN_RAW, RUN_ERR, RUN_EXPECTED), "run");
}

#[test]
fn real_edited_test_capture_filters_started_noise_and_elides_results() {
    assert_real_case((TEST_RAW, TEST_ERR, TEST_EXPECTED), "test");
}
