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
fn json_env_on_bare_absolute_path_fallback_streams_passthrough() {
    // DBT_LOG_FORMAT=json changes console shape without any argv flag: the
    // bare absolute-path fallback (`rtk ./dbt run`) must stream raw like the
    // native path instead of capturing/merging stderr into stdout.
    const JSON: &str = "{\"info\":\"run\"}\n{\"info\":\"done\"}\n";
    const ERR: &str = "some stderr text\n";
    let dir = setup_case(JSON, ERR);
    let tool = dir.path().join("dbt");
    let out = command(dir.path(), &[tool.to_str().unwrap(), "run"])
        .env("DBT_LOG_FORMAT", "json")
        .output()
        .unwrap();
    assert_eq!(stdout(&out), JSON);
    assert_eq!(String::from_utf8(out.stderr).unwrap(), ERR);
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
fn ineligible_flags_bypass_filtering_and_forward_arguments() {
    // `--log-format json` is output-changing: must pass through raw so the
    // text noise rules never touch structured output. Exact argv + status
    // forwarding still holds.
    let dir = setup(RAW);
    let out = command(dir.path(), &["dbt", "run", "--log-format", "json"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out), RAW);
    assert_eq!(String::from_utf8(out.stderr).unwrap(), ERROR);
    assert_eq!(
        fs::read_to_string(dir.path().join("argv.txt")).unwrap(),
        "run\n--log-format\njson\n"
    );
}

#[test]
fn selected_commands_filter_like_bare_and_forward_exact_argv() {
    // Ticket 2: eligible `--select` receives the same guarded compression as
    // bare commands (banner stripped, successes/skips elided on consistent
    // footer, diagnostics kept, recall hint present), with exact argv.
    let dir = setup(RAW);
    let out = command(dir.path(), &["dbt", "run", "--select", "good"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(
        !text.contains("Succeeded") && !text.contains("Skipped"),
        "{text}"
    );
    assert!(
        text.contains("Failed") && text.contains("Summary: 3 total"),
        "{text}"
    );
    assert!(text.contains(ERROR.trim()), "{text}");
    assert!(text.contains("rtk recall"), "{text}");
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

// Edited real captures: dbt Core 1.12.5 + duckdb 1.11.0 on the saved DuckDB
// playground project. All timestamps are scrubbed to a single fixed value;
// versions, counts, line shapes and byte structure are preserved, including
// the leading ESC[0m and the color codes that `strip_ansi` must remove before
// line matching. v1 logs everything to stdout, so there is no .stderr
// fixture (mirroring the dbt_cloud_* captures above, which also carry no
// stderr). Binary assertions here pin behavior, not an expected-output file.
const V1_RUN_RAW: &str = include_str!("fixtures/dbt_v1_run_raw.txt");
const V1_TEST_RAW: &str = include_str!("fixtures/dbt_v1_test_raw.txt");
const V1_BUILD_RAW: &str = include_str!("fixtures/dbt_v1_build_raw.txt");
const V1_SELECT_RAW: &str = include_str!("fixtures/dbt_v1_select_raw.txt");
const V1_NOSELECT_RAW: &str = include_str!("fixtures/dbt_v1_noselect_raw.txt");

fn recall_hash(text: &str) -> &str {
    text.split("[full output: rtk recall ")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap()
}

fn assert_v1_recall(dir: &Path, raw: &str, text: &str) {
    let recalled = command(dir, &["recall", recall_hash(text)])
        .output()
        .unwrap();
    assert!(recalled.status.success());
    assert_eq!(stdout(&recalled), raw);
}

#[test]
fn v1_run_capture_elides_successes_and_recovers_raw() {
    let dir = setup_case(V1_RUN_RAW, "");
    let out = command(dir.path(), &["dbt", "run"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(!text.contains("[OK in"), "{text}");
    assert!(!text.contains("Running with dbt="), "{text}");
    assert!(!text.contains("START sql table model"), "{text}");
    assert!(
        text.contains("Done. PASS=2 WARN=0 ERROR=0 SKIP=0 NO-OP=0 REUSED=0 TOTAL=2"),
        "{text}"
    );
    assert_v1_recall(dir.path(), V1_RUN_RAW, &text);
}

#[test]
fn v1_select_capture_elides_successes_and_recovers_raw() {
    let dir = setup_case(V1_SELECT_RAW, "");
    let out = command(dir.path(), &["dbt", "run", "--select", "resource_summary"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(!text.contains("[OK in"), "{text}");
    assert!(
        text.contains("Done. PASS=1 WARN=0 ERROR=0 SKIP=0 NO-OP=0 REUSED=0 TOTAL=1"),
        "{text}"
    );
    assert_v1_recall(dir.path(), V1_SELECT_RAW, &text);
}

#[test]
fn v1_test_capture_keeps_fail_line_and_stdout_diagnostic() {
    let dir = setup_case(V1_TEST_RAW, "");
    let out = command(dir.path(), &["dbt", "test"])
        .env("DBT_EXIT", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(!text.contains("[PASS in"), "{text}");
    assert!(
        text.contains("FAIL 150 unique_resource_catalog_url"),
        "{text}"
    );
    assert!(
        text.contains("[ERROR]: in test unique_resource_catalog_url"),
        "{text}"
    );
    assert!(
        text.contains("Got 150 results, configured to fail if != 0"),
        "{text}"
    );
    assert!(
        text.contains("Done. PASS=19 WARN=0 ERROR=1 SKIP=0 NO-OP=0 REUSED=0 TOTAL=20"),
        "{text}"
    );
    assert_v1_recall(dir.path(), V1_TEST_RAW, &text);
}

#[test]
fn v1_build_capture_elides_ok_pass_and_skip_keeps_fail() {
    let dir = setup_case(V1_BUILD_RAW, "");
    let out = command(dir.path(), &["dbt", "build"])
        .env("DBT_EXIT", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(
        !text.contains("[OK in") && !text.contains("[PASS in") && !text.contains("[SKIP]"),
        "{text}"
    );
    assert!(
        text.contains("FAIL 150 unique_resource_catalog_url"),
        "{text}"
    );
    assert!(
        text.contains("[ERROR]: in test unique_resource_catalog_url"),
        "{text}"
    );
    assert!(
        text.contains("Done. PASS=14 WARN=0 ERROR=1 SKIP=7 NO-OP=0 REUSED=0 TOTAL=22"),
        "{text}"
    );
    assert_v1_recall(dir.path(), V1_BUILD_RAW, &text);
}

#[test]
fn v1_no_selection_capture_keeps_warnings_and_infers_no_footer() {
    let dir = setup_case(V1_NOSELECT_RAW, "");
    let out = command(
        dir.path(),
        &["dbt", "run", "--select", "nonexistent_model_xyz"],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(
        text.contains(
            "The selection criterion 'nonexistent_model_xyz' does not match any enabled nodes"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "Nothing to do. Try checking your model configs and model specification args"
        ),
        "{text}"
    );
    // No `Done.` footer, so nothing is elided and no recall hint is emitted.
    assert!(!text.contains("Done."), "{text}");
    assert!(!text.contains("rtk recall"), "{text}");
}

// Byte-exact v1 cases mirroring assert_real_case above: the complete filtered
// output (including the deterministic recall hint) is pinned, then the raw log
// is recovered exactly. Regenerate the expected files if recall id derivation
// ever changes. Fixture-level savings (ceil(bytes/4)): run 89.4%, test 91.1%,
// build 90.5%, select 84.7% — v1 timestamps every line, so noise dominates.
const V1_RUN_EXPECTED: &str = include_str!("fixtures/dbt_v1_run_expected.txt");
const V1_TEST_EXPECTED: &str = include_str!("fixtures/dbt_v1_test_expected.txt");
const V1_BUILD_EXPECTED: &str = include_str!("fixtures/dbt_v1_build_expected.txt");
const V1_SELECT_EXPECTED: &str = include_str!("fixtures/dbt_v1_select_expected.txt");

fn assert_v1_exact(raw: &str, expected: &str, args: &[&str], exit: i32) {
    let dir = setup_case(raw, "");
    let out = command(dir.path(), args).output().unwrap();
    assert_eq!(out.status.code(), Some(exit));
    let text = stdout(&out);
    assert_eq!(text, expected);
    assert_v1_recall(dir.path(), raw, &text);
}

#[test]
fn v1_run_capture_byte_exact_filtered_output() {
    assert_v1_exact(V1_RUN_RAW, V1_RUN_EXPECTED, &["dbt", "run"], 0);
}

#[test]
fn v1_test_capture_byte_exact_filtered_output() {
    let dir = setup_case(V1_TEST_RAW, "");
    let out = command(dir.path(), &["dbt", "test"])
        .env("DBT_EXIT", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert_eq!(text, V1_TEST_EXPECTED);
    assert_v1_recall(dir.path(), V1_TEST_RAW, &text);
}

#[test]
fn v1_build_capture_byte_exact_filtered_output() {
    let dir = setup_case(V1_BUILD_RAW, "");
    let out = command(dir.path(), &["dbt", "build"])
        .env("DBT_EXIT", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert_eq!(text, V1_BUILD_EXPECTED);
    assert_v1_recall(dir.path(), V1_BUILD_RAW, &text);
}

#[test]
fn v1_select_capture_byte_exact_filtered_output() {
    assert_v1_exact(
        V1_SELECT_RAW,
        V1_SELECT_EXPECTED,
        &["dbt", "run", "--select", "resource_summary"],
        0,
    );
}

// Parity proof for the eight v1 Core strip rules (ticket 1 acceptance). Each
// rule must remove at least one real v1 line; the additions must leave the
// already-filtered Fusion/Cloud output byte-identical. Rules are matched
// against ANSI-free lines because `strip_ansi` runs before line matching.

const DBT_TOML: &str = include_str!("../src/filters/dbt.toml");

const V1_RULES: [&str; 8] = [
    r"^\d\d:\d\d:\d\d\s+Running with dbt=",
    r"^\d\d:\d\d:\d\d\s+Registered adapter:",
    r"^\d\d:\d\d:\d\d\s+Unable to do partial parsing",
    r"^\d\d:\d\d:\d\d\s+Found \d+ .*(models|tests|sources)",
    r"^\d\d:\d\d:\d\d\s+Concurrency: \d+ threads",
    r"^\d\d:\d\d:\d\d\s+\d+ of \d+ START .* \[RUN\]$",
    r"^\d\d:\d\d:\d\d\s+Finished running .* in \d+ hours",
    r"^\d\d:\d\d:\d\d\s+Completed (successfully|with )",
];

const V1_FIXTURES: [&str; 5] = [
    V1_RUN_RAW,
    V1_TEST_RAW,
    V1_BUILD_RAW,
    V1_SELECT_RAW,
    V1_NOSELECT_RAW,
];

const CLOUD_FIXTURES: [&str; 2] = [RUN_RAW, TEST_RAW];

fn ansi_free(text: &str) -> String {
    regex::Regex::new(r"\x1b\[[0-9;]*m")
        .unwrap()
        .replace_all(text, "")
        .into_owned()
}

fn dbt_strip_rules() -> Vec<String> {
    let parsed: toml::Value = toml::from_str(DBT_TOML).expect("dbt.toml must parse");
    parsed["filters"]["dbt"]["strip_lines_matching"]
        .as_array()
        .expect("strip_lines_matching must be an array")
        .iter()
        .map(|rule| rule.as_str().expect("rule must be a string").to_string())
        .collect()
}

fn joined(fixtures: &[&str]) -> String {
    fixtures
        .iter()
        .map(|fixture| ansi_free(fixture))
        .collect::<Vec<_>>()
        .join("\n")
}

fn count_matches(rule: &str, corpus: &str) -> usize {
    let re = regex::Regex::new(rule).unwrap();
    corpus.lines().filter(|line| re.is_match(line)).count()
}

fn apply_rules(rules: &[String], corpus: &str) -> Vec<String> {
    let compiled: Vec<regex::Regex> = rules
        .iter()
        .map(|rule| regex::Regex::new(rule).unwrap())
        .collect();
    corpus
        .lines()
        .filter(|line| !compiled.iter().any(|re| re.is_match(line)))
        .map(str::to_string)
        .collect()
}

#[test]
fn v1_rules_each_strip_a_real_v1_line() {
    let rules = dbt_strip_rules();
    let v1 = joined(&V1_FIXTURES);
    for rule in V1_RULES {
        assert!(
            rules.iter().any(|present| present == rule),
            "rule missing from dbt.toml: {rule}"
        );
        assert!(
            count_matches(rule, &v1) > 0,
            "rule matches no v1 fixture line: {rule}"
        );
    }
}

#[test]
fn v1_rules_do_not_change_fusion_cloud_output() {
    let rules = dbt_strip_rules();
    let cloud = joined(&CLOUD_FIXTURES);

    // Exactly one addition (`\s+Running with dbt=`) collides with 2 cloud
    // lines; the other seven match none.
    let collisions: Vec<(&str, usize)> = V1_RULES
        .iter()
        .map(|rule| (*rule, count_matches(rule, &cloud)))
        .filter(|(_, count)| *count > 0)
        .collect();
    assert_eq!(
        collisions,
        [(r"^\d\d:\d\d:\d\d\s+Running with dbt=", 2)],
        "unexpected Fusion/Cloud collisions"
    );

    // The colliding lines are already stripped by the pre-existing
    // single-space rule, so old and new rule sets filter identically.
    let old_rules: Vec<String> = rules
        .iter()
        .filter(|rule| !V1_RULES.contains(&rule.as_str()))
        .cloned()
        .collect();
    assert_ne!(rules, old_rules);
    assert_eq!(apply_rules(&rules, &cloud), apply_rules(&old_rules, &cloud));
}
