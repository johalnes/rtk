//! Guarded dbt result elision, after the existing TOML noise filter.
//!
//! `run` also owns the `rtk dbt` native entry point: bare and selected
//! run/test/build reuse the fallback's TOML capture orchestration byte for
//! byte, while everything else (other subcommands, unknown/malformed or
//! output-changing flags, `--help`) passes straight to dbt.

use crate::core::utils::{self, ChildArgExt};
use anyhow::Result;
use regex::Regex;
use std::sync::LazyLock;

static RESULT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:\d{2}:\d{2}:\d{2}\s+)?\s*(Succeeded|Passed|Warned|Failed|Skipped)\s+(?:\[[^\]\r\n]+\]\s+)?(?:model|test|seed|snapshot|unit test|unit_test)\s+\S.*$",
    )
    .unwrap()
});

/// Return a compact view only when one complete native footer accounts for every
/// observed result. None keeps the existing output. Diagnostics after the footer
/// are never interpreted as result lines, even when they contain matching text.
pub fn summarize(output: &str, command: &str) -> Option<String> {
    if !matches!(command, "run" | "test" | "build") {
        return None;
    }
    let lines: Vec<_> = output.split_inclusive('\n').collect();
    let finished = format!("Finished '{command}' ");
    let footer = lines.iter().position(|line| line.starts_with(&finished))?;
    if lines
        .iter()
        .filter(|line| line.starts_with("Finished '"))
        .count()
        != 1
        || lines
            .iter()
            .filter(|line| line.starts_with("Summary:"))
            .count()
            != 1
        || !lines.get(footer + 1)?.starts_with("Processed:")
    {
        return None;
    }
    let summary = lines
        .get(footer + 2)?
        .trim_end()
        .strip_prefix("Summary: ")?;
    let mut fields = [None; 6];
    for part in summary.split(" | ") {
        let (count, category) = part.split_once(' ')?;
        let index = match category {
            "total" => 0,
            "success" => 1,
            "warn" => 2,
            "error" => 3,
            "skipped" => 4,
            "no-op" => 5,
            _ => return None,
        };
        if fields[index]
            .replace(count.parse::<usize>().ok()?)
            .is_some()
        {
            return None;
        }
    }
    let total = fields[0]?;
    let expected = [
        fields[1].unwrap_or(0),
        fields[2].unwrap_or(0),
        fields[3].unwrap_or(0),
        fields[4].unwrap_or(0).checked_add(fields[5].unwrap_or(0))?,
    ];
    if expected
        .iter()
        .try_fold(0usize, |sum, count| sum.checked_add(*count))?
        != total
    {
        return None;
    }
    let mut observed = [0usize; 4];
    let mut removed = vec![false; lines.len()];
    for (index, line) in lines[..footer].iter().enumerate() {
        // Do not classify text embedded in a diagnostic section as node outcomes.
        if line.contains("Errors and Warnings") {
            return None;
        }
        if let Some(result) = RESULT.captures(line.trim_end_matches(['\r', '\n'])) {
            let category = match &result[1] {
                "Succeeded" | "Passed" => 0,
                "Warned" => 1,
                "Failed" => 2,
                "Skipped" => 3,
                _ => unreachable!(),
            };
            observed[category] += 1;
            removed[index] = category == 0 || category == 3;
        }
    }
    if observed != expected || !removed.iter().any(|remove| *remove) {
        return None;
    }
    Some(
        lines
            .into_iter()
            .zip(removed)
            .filter_map(|(line, remove)| (!remove).then_some(line))
            .collect(),
    )
}

/// Native `rtk dbt` entry point (tickets 1+2: bare and selected commands).
///
/// `rtk dbt --help` stays native RTK help. Bare and selected run/test/build
/// invocations that match the conservative eligibility grammar are filtered
/// with exact forwarded argv; everything else (other subcommands, unknown or
/// malformed flags, output-changing modes, `rtk dbt run --help`) delegates to
/// dbt itself so dbt owns syntax errors and selection semantics.
pub fn run(cli_args: &[String], verbose: u8) -> Result<i32> {
    if cli_args.is_empty() {
        print_native_help();
        return Ok(0);
    }
    // Only bare `-h`/`--help` shows wrapper help. Every other leading flag
    // (`--version`, `--unknown-flag`, …) passes through to dbt with exact argv.
    if cli_args.len() == 1 && matches!(cli_args[0].as_str(), "-h" | "--help") {
        print_native_help();
        return Ok(0);
    }
    match selection_eligible(cli_args) {
        Some(subcommand) => run_filtered(subcommand, cli_args, verbose),
        None => run_passthrough(cli_args, verbose),
    }
}

/// Option value arity. Values stay opaque: never split, rewritten or
/// validated — dbt owns selector syntax.
#[derive(Clone, Copy)]
enum Arity {
    Flag,
    One,
    Multi,
}

/// Options accepted after the subcommand. Arities and the run/build-only
/// `--full-refresh` were verified against the pinned Fusion
/// 2.0.0-preview.218 CLI (`dbt run|test|build --help` probes, 2026-09-28).
fn subcommand_option(token: &str, subcommand: &str) -> Option<Arity> {
    match token {
        "-s" | "--select" | "--exclude" => Some(Arity::Multi),
        "--selector" | "--target" | "-t" | "--project-dir" | "--profile" | "--profiles-dir"
        | "--vars" | "--threads" | "--state" => Some(Arity::One),
        "--defer" => Some(Arity::Flag),
        "--full-refresh" | "-f" if subcommand != "test" => Some(Arity::Flag),
        _ => None,
    }
}

/// Options accepted before the subcommand. The pinned CLI rejects long-form
/// globals there (`dbt --target dev run` exits 1) but accepts `-t`/`-s`.
fn global_option(token: &str) -> Option<Arity> {
    match token {
        "-t" => Some(Arity::One),
        "-s" => Some(Arity::Multi),
        _ => None,
    }
}

/// Split `--option=value` into name and inline value. Short `-s=…` forms are
/// unverified on the pinned CLI and bypass. Every other unknown token —
/// including output-changing modes (`-q`, `-d`, `--log-format`, …), help and
/// version — falls out of the allowlists and bypasses.
fn split_option(token: &str) -> Option<(&str, Option<&str>)> {
    match token.split_once('=') {
        Some((name, value)) if token.starts_with("--") => Some((name, Some(value))),
        Some(_) => None,
        None => Some((token, None)),
    }
}

/// Advance past one option with its value(s). The equals form carries exactly
/// one value: the pinned CLI does not consume a following bare token
/// (`dbt run --select=a b --help` exits 1). Before the subcommand, multi
/// values also stop at a bare subcommand token (`dbt -s x y run` selects
/// `x` and `y`, then runs).
fn skip_option(
    cli_args: &[String],
    index: usize,
    arity: Arity,
    inline: Option<&str>,
    stop_at_subcommand: bool,
) -> Option<usize> {
    if inline.is_some() {
        return if matches!(arity, Arity::Flag) {
            None
        } else {
            Some(index + 1)
        };
    }
    match arity {
        Arity::Flag => Some(index + 1),
        Arity::One => {
            let value = cli_args.get(index + 1)?;
            (!value.starts_with('-')).then_some(index + 2)
        }
        Arity::Multi => {
            let mut end = index;
            while cli_args.get(end + 1).is_some_and(|value| {
                !value.starts_with('-')
                    && (!stop_at_subcommand || !matches!(value.as_str(), "run" | "test" | "build"))
            }) {
                end += 1;
            }
            (end > index).then_some(end + 1)
        }
    }
}

/// Return the subcommand when argv matches the conservative eligibility
/// grammar for selected run/test/build: only verified context/selection
/// options with correct arities, no stray positionals, no help/version or
/// output-changing flags. Unknown or malformed input yields None so the exact
/// argv passes through unchanged and dbt owns the response.
///
/// `pub(crate)` so hook rewriting (`discover::registry`) and the absolute-path
/// fallback (`main::run_fallback`) share the exact eligibility policy with the
/// native entry point — one definition, no second matcher to drift.
pub(crate) fn selection_eligible(cli_args: &[String]) -> Option<&str> {
    let mut index = 0;
    let subcommand = loop {
        let token = cli_args.get(index)?;
        if !token.starts_with('-') {
            if !matches!(token.as_str(), "run" | "test" | "build") {
                return None;
            }
            break token.as_str();
        }
        let (name, inline) = split_option(token)?;
        let arity = global_option(name)?;
        index = skip_option(cli_args, index, arity, inline, true)?;
    };
    index += 1;
    while let Some(token) = cli_args.get(index) {
        if !token.starts_with('-') {
            // run/test/build take no positionals; dbt itself rejects the bare
            // token after an equals form.
            return None;
        }
        let (name, inline) = split_option(token)?;
        let arity = subcommand_option(name, subcommand)?;
        index = skip_option(cli_args, index, arity, inline, false)?;
    }
    Some(subcommand)
}

/// Rewrite eligibility for a normalized `dbt ...` command string (basename
/// already stripped, e.g. `dbt run --select x`). Uses minimal shell-like
/// splitting (single/double quotes group spaces; unbalanced quotes fail
/// closed to bypass). Bare `dbt run`/`test`/`build` are trivially eligible.
pub(crate) fn rewrite_eligible(normalized: &str) -> bool {
    let Some(tokens) = split_shell_words(normalized) else {
        return false;
    };
    if tokens.first().is_some_and(|base| base == "dbt") {
        selection_eligible(&tokens[1..]).is_some()
    } else {
        false
    }
}

/// Minimal shell-word split for rewrite eligibility only: spaces outside
/// `'...'`/`"..."` separate tokens; quotes group but are stripped (matching
/// what the shell passes as argv). Backslash escapes the next char outside
/// quotes. Unbalanced quotes return `None` → caller bypasses rewriting.
fn split_shell_words(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_token = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                        in_token = true;
                    } else {
                        return None;
                    }
                }
                c if c.is_whitespace() => {
                    if in_token {
                        tokens.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                }
                _ => {
                    current.push(c);
                    in_token = true;
                }
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_token {
        tokens.push(current);
    }
    Some(tokens)
}

/// Env overrides can change the console output without any argv flag; bypass
/// so the text noise rules never touch JSON or altered logging output.
/// `pub(crate)` for the absolute-path fallback, which mirrors the native check.
pub(crate) fn output_env_safe() -> bool {
    let json_log = ["DBT_LOG_FORMAT", "DBT_LOG_FORMAT_FILE"]
        .iter()
        .any(|key| matches!(std::env::var(key).as_deref(), Ok("json" | "otel")));
    let suppressed = ["DBT_QUIET", "DBT_DEBUG"]
        .iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.is_empty()));
    !json_log && !suppressed
}

/// Guarded capture for one eligible run/test/build invocation (bare or
/// selected). Delegates to the shared `run_matched_capture` orchestration
/// (same function the generic fallback uses): same merged-stream join, same
/// TOML filter + footer guard, same tee/recall recovery, same tracking
/// labels. The TOML `dbt` filter is matched on the bare `dbt <subcommand>`
/// prefix; the child argv stays the exact original token sequence.
/// `RTK_NO_TOML=1` degrades to the fallback's streaming passthrough.
fn run_filtered(subcommand: &str, cli_args: &[String], verbose: u8) -> Result<i32> {
    if verbose > 0 {
        eprintln!("Running: dbt {}", cli_args.join(" "));
    }
    if output_env_safe() && !crate::core::toml_filter::toml_disabled() {
        let raw_command = format!("dbt {subcommand}");
        if let Some(filter) = crate::core::toml_filter::find_matching_filter(&raw_command)
            && filter.name == "dbt"
        {
            let child_args: Vec<String> = std::iter::once("dbt".to_string())
                .chain(cli_args.iter().cloned())
                .collect();
            return crate::core::toml_filter::run_matched_capture(
                "dbt",
                &raw_command,
                &child_args,
                filter,
                None,
                Some(subcommand),
            );
        }
    }
    run_passthrough(cli_args, verbose)
}

/// Streaming passthrough with exact argv, explicit stdin, and exit status.
fn run_passthrough(cli_args: &[String], verbose: u8) -> Result<i32> {
    if verbose > 0 {
        eprintln!("Running: dbt {}", cli_args.join(" "));
    }
    let full_argv: Vec<String> = std::iter::once("dbt".to_string())
        .chain(cli_args.iter().cloned())
        .collect();
    run_native_passthrough(&full_argv, Some(verbose))
}

/// Streaming passthrough with explicit stdin and exit status.
///
/// `full_argv[0]` is the display/executable name, the rest are child args.
/// String-based here (argv comes from Clap); byte edge cases stay on the
/// fallback path which reads `std::env::args()` directly.
fn run_native_passthrough(full_argv: &[String], verbose: Option<u8>) -> Result<i32> {
    let timer = crate::core::tracking::TimedExecution::start();
    let raw_command = full_argv.join(" ");
    let mut cmd = utils::resolved_command("dbt");
    cmd.child_args(full_argv.iter().skip(1));
    cmd.stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    match cmd.status() {
        Ok(status) => {
            timer.track_passthrough(&raw_command, &format!("rtk fallback: {raw_command}"));
            if verbose.unwrap_or(0) > 0 {
                eprintln!("dbt passthrough: {:?}", full_argv);
            }
            Ok(utils::exit_code_from_status(&status, &raw_command))
        }
        Err(e) => {
            eprintln!("[rtk: {}]", e);
            Ok(127)
        }
    }
}

fn print_native_help() {
    println!(
        "Compact dbt Fusion run/test/build output — strip startup banner and dividers, keep results and full diagnostics\n\
         \n\
         Usage: rtk dbt <COMMAND> [ARGS]...\n\
         \n\
         Commands:\n  \
         run    Filtered `dbt run` output (known selection/context flags; anything else passes through)\n  \
         test   Filtered `dbt test` output (known selection/context flags; anything else passes through)\n  \
         build  Filtered `dbt build` output (known selection/context flags; anything else passes through)\n\
         \n\
         Selection examples (passed through unchanged; dbt owns the syntax):\n  \
         rtk dbt run --select my_model\n  \
         rtk dbt build --select tag:daily --exclude package:legacy\n  \
         rtk dbt test -s path:models/marts\n\
         \n\
         `rtk dbt <subcommand> --help` delegates to dbt for native flag documentation."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // Sanitized Cloud/Fusion shapes from the saved Databricks and DuckDB captures.
    const RESULTS: &str = "Created invocation id example\n12:00:00 Succeeded model analytics.good (table) [1 of 5 in 0.1s]\n12:00:01 Passed test good_test [2 of 5 in 0.1s]\n12:00:02 Skipped model analytics.downstream (view)\n12:00:03 Warned test warning_test\n12:00:04 Failed model analytics.broken (table)\n";
    const FOOTER: &str = "Finished 'build' with 1 warning and 1 error for target 'dev' [1s]\nProcessed: 3 models | 2 tests\nSummary: 5 total | 2 success | 1 warn | 1 error | 1 skipped";
    const DIAGNOSTIC: &str = "\n=================== Errors and Warnings ====================\n[error] Database Error in model broken (target/run/project/models/broken.sql)\n  select no_such_column\n         ^\n12:00:00 Passed test literal_in_diagnostic\n";

    #[test]
    fn removes_success_and_skip_but_preserves_footer_and_diagnostics() {
        let input = format!("{RESULTS}{FOOTER}{DIAGNOSTIC}");
        let expected = format!(
            "Created invocation id example\n12:00:03 Warned test warning_test\n12:00:04 Failed model analytics.broken (table)\n{FOOTER}{DIAGNOSTIC}"
        );
        assert_eq!(summarize(&input, "build"), Some(expected));
    }

    #[test]
    fn recognizes_local_results_and_combined_skipped_noop_counts() {
        let input = "  Succeeded [  0.04s] model analytics.good (table)\n    Skipped [  0.01s] model analytics.a (view)\n    Skipped [  0.01s] model analytics.b (ephemeral)\nFinished 'run' successfully for target 'dev' [1s]\nProcessed: 3 models\nSummary: 3 total | 1 success | 1 skipped | 1 no-op";
        assert_eq!(
            summarize(input, "run").as_deref(),
            input.find("Finished").map(|i| &input[i..])
        );
    }

    #[test]
    fn incomplete_or_inconsistent_summary_does_not_remove_results() {
        for footer in [
            "",
            "Summary: 5 total | 2 success | 1 warn | 1 error | 1 skipped",
            "Finished 'build' with errors\nProcessed: 5 nodes",
            "Finished 'build' with errors\nProcessed: 5 nodes\nSummary: 5 total | 3 success | 1 error | 1 skipped",
            "Finished 'build' with errors\nProcessed: 5 nodes\nSummary: 6 total | 2 success | 1 warn | 1 error | 1 skipped",
            "Finished 'build' with errors\nProcessed: 5 nodes\nSummary: 5 total | 2 success | 1 warn | 1 error | 1 cached",
            "Finished 'build' with errors\nProcessed: 5 nodes\nSummary: 5 total | 2 success | 2 success | 1 warn | 1 error | 1 skipped",
            "Finished 'build' with errors\nProcessed: 5 nodes\nSummary: 18446744073709551616 total | 2 success",
        ] {
            assert_eq!(
                summarize(&format!("{RESULTS}{footer}"), "build"),
                None,
                "{footer}"
            );
        }
        assert_eq!(summarize(&format!("{RESULTS}{FOOTER}"), "test"), None);
    }

    fn eligible(args: &[&str]) -> Option<String> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        selection_eligible(&args).map(str::to_string)
    }

    #[test]
    fn bare_and_separate_multi_value_selections_are_eligible() {
        assert_eq!(eligible(&["run"]), Some("run".into()));
        assert_eq!(
            eligible(&["run", "--select", "table_a", "table_b"]),
            Some("run".into())
        );
        assert_eq!(
            eligible(&["test", "-s", "tag:daily", "path:models/x"]),
            Some("test".into())
        );
        assert_eq!(
            eligible(&["build", "--select", "a", "--exclude", "b"]),
            Some("build".into())
        );
        assert_eq!(eligible(&["run", "-s", "a", "-s", "b"]), Some("run".into()));
        // Selectors stay opaque: commas, graph operators and quoted multi
        // values are forwarded as-is, never split or rewritten.
        assert_eq!(
            eligible(&["run", "--select", "tag:daily,+model+,~x"]),
            Some("run".into())
        );
        assert_eq!(eligible(&["run", "--select", "a b"]), Some("run".into()));
    }

    #[test]
    fn equals_form_carries_exactly_one_value() {
        assert_eq!(eligible(&["run", "--select=a"]), Some("run".into()));
        assert_eq!(
            eligible(&["run", "--select=a", "--exclude", "b"]),
            Some("run".into())
        );
        // The pinned CLI rejects the bare token after an equals form
        // (`dbt run --select=a b --help` exits 1): not eligible.
        assert_eq!(eligible(&["run", "--select=a", "b"]), None);
    }

    #[test]
    fn context_options_and_globals_are_eligible() {
        assert_eq!(
            eligible(&[
                "run",
                "-t",
                "dev",
                "--profile",
                "p",
                "--project-dir",
                ".",
                "--profiles-dir",
                "d",
                "--vars",
                "{a: 1}",
                "--threads",
                "4",
                "--state",
                "s",
                "--defer",
            ]),
            Some("run".into())
        );
        assert_eq!(
            eligible(&["run", "--selector", "nightly"]),
            Some("run".into())
        );
        assert_eq!(eligible(&["build", "-f"]), Some("build".into()));
        assert_eq!(eligible(&["run", "-f"]), Some("run".into()));
        // test does not accept --full-refresh on the pinned CLI.
        assert_eq!(eligible(&["test", "-f"]), None);
        // Short-only global placement before the subcommand.
        assert_eq!(eligible(&["-t", "dev", "run"]), Some("run".into()));
        assert_eq!(eligible(&["-s", "x", "run"]), Some("run".into()));
        assert_eq!(
            eligible(&["-s", "x", "y", "run", "-s", "z"]),
            Some("run".into())
        );
        // Long-form globals are rejected by the pinned CLI before the
        // subcommand (`dbt --target dev run` exits 1).
        assert_eq!(eligible(&["--target", "dev", "run"]), None);
    }

    #[test]
    fn unknown_malformed_or_output_changing_flags_bypass() {
        // Unknown flags and unsupported commands.
        assert_eq!(eligible(&["run", "--unknown"]), None);
        assert_eq!(eligible(&["run", "--output", "json"]), None);
        assert_eq!(eligible(&["seed"]), None);
        assert_eq!(eligible(&["--foo", "run"]), None);
        // Help, version and output-changing modes stay raw.
        assert_eq!(eligible(&["run", "--help"]), None);
        assert_eq!(eligible(&["run", "-h"]), None);
        assert_eq!(eligible(&["run", "-q"]), None);
        assert_eq!(eligible(&["run", "-d"]), None);
        assert_eq!(eligible(&["run", "--log-format", "json"]), None);
        assert_eq!(eligible(&["run", "--log-level", "debug"]), None);
        assert_eq!(eligible(&["--version"]), None);
        // Malformed: missing value, option-looking value, stray positional,
        // `--`, short equals, flag with value.
        assert_eq!(eligible(&["run", "--target"]), None);
        assert_eq!(eligible(&["run", "--select"]), None);
        assert_eq!(eligible(&["run", "--target", "-t"]), None);
        assert_eq!(eligible(&["run", "positional"]), None);
        assert_eq!(eligible(&["run", "--", "x"]), None);
        assert_eq!(eligible(&["run", "-s=x"]), None);
        assert_eq!(eligible(&["run", "--defer=x"]), None);
    }

    #[test]
    fn rewrite_eligibility_matches_selection_policy_on_strings() {
        // Eligible: bare + selections, incl. quoted multiword values.
        assert!(rewrite_eligible("dbt run"));
        assert!(rewrite_eligible("dbt run --select my_model"));
        assert!(rewrite_eligible("dbt test --vars '{a: 1}'"));
        assert!(rewrite_eligible("dbt build --select a --exclude b"));
        // Ineligible: output-changing, help/version, unknown, stray positionals.
        assert!(!rewrite_eligible("dbt run --log-format json"));
        assert!(!rewrite_eligible("dbt run --help"));
        assert!(!rewrite_eligible("dbt --version"));
        assert!(!rewrite_eligible("dbt run --unknown"));
        assert!(!rewrite_eligible("dbt seed"));
        assert!(!rewrite_eligible("dbt"));
        // Unbalanced quotes fail closed to bypass.
        assert!(!rewrite_eligible("dbt test --vars '{a: 1"));
    }
}
