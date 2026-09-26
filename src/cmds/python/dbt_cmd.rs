//! Guarded dbt result elision, after the existing TOML noise filter.

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
        assert_eq!(
            summarize(&format!("{RESULTS}{FOOTER}\n{FOOTER}"), "build"),
            None
        );
        assert_eq!(summarize(&format!("{RESULTS}{FOOTER}"), "test"), None);
    }
}
