//! Native `rtk dbt` CLI contract (ticket 1): discoverability, native help,
//! and exact child argv/status passthrough.
//!
//! Help discovery is asserted cross-platform with no `dbt` on PATH. Process /
//! argv-forwarding tests stub `dbt` on PATH and are Unix-only because the
//! repository has no portable child-shim mechanism (matching `dbt_summary_test`
//! and `dotnet_double_dash_test`).

use std::path::Path;
use std::process::{Command, Output};

fn rtk(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rtk"));
    cmd.current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("RTK_DB_PATH", dir.join("tracking.db"))
        .env("RTK_RECALL_DB", dir.join("recall.db"))
        .env_remove("RTK_RECALL")
        .env_remove("RTK_TEE")
        .env_remove("RTK_NO_TOML");
    cmd
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn empty_path_dir(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("empty-path");
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn root_help_lists_dbt_as_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let out = rtk(dir.path())
        .args(["--help"])
        .env("PATH", empty_path_dir(dir.path()))
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = stdout(&out);
    assert!(
        help.lines().any(|line| {
            let token = line.trim_start();
            token == "dbt" || token.starts_with("dbt ")
        }),
        "root --help must list `dbt`:\n{help}"
    );
}

#[test]
fn dbt_help_is_native_and_needs_no_dbt_binary() {
    let dir = tempfile::tempdir().unwrap();
    let empty = empty_path_dir(dir.path());
    let out = rtk(dir.path())
        .args(["dbt", "--help"])
        .env("PATH", &empty)
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = stdout(&out);
    for needle in ["run", "test", "build", "--select", "--exclude", "-s"] {
        assert!(
            help.contains(needle),
            "missing {needle:?} in dbt help:\n{help}"
        );
    }

    let bare = rtk(dir.path())
        .args(["dbt"])
        .env("PATH", &empty)
        .output()
        .unwrap();
    assert!(bare.status.success());
    assert_eq!(
        stdout(&bare),
        help,
        "bare `rtk dbt` must show the same native help"
    );

    // Only bare `rtk dbt` and the exact `-h`/`--help` forms show wrapper help.
    let short = rtk(dir.path())
        .args(["dbt", "-h"])
        .env("PATH", &empty)
        .output()
        .unwrap();
    assert!(short.status.success());
    assert_eq!(
        stdout(&short),
        help,
        "`rtk dbt -h` must show the same wrapper help as `--help`"
    );
}

#[test]
fn root_help_and_version_before_dbt_win_without_child() {
    // Established Clap precedence: a global `--help`/`--version` before a
    // subcommand shows RTK output and never runs the child. Verified against a
    // non-dbt subcommand (`rtk --help git` == `rtk --help`).
    let dir = tempfile::tempdir().unwrap();
    let empty = empty_path_dir(dir.path());
    for flag in ["--help", "--version"] {
        let prefixed = rtk(dir.path())
            .args([flag, "dbt"])
            .env("PATH", &empty)
            .output()
            .unwrap();
        let plain = rtk(dir.path())
            .args([flag])
            .env("PATH", &empty)
            .output()
            .unwrap();
        assert!(prefixed.status.success(), "rtk {flag} dbt");
        assert_eq!(
            stdout(&prefixed),
            stdout(&plain),
            "rtk {flag} dbt must match plain rtk {flag}"
        );
        assert!(
            !stdout(&prefixed).contains("rtk dbt <COMMAND>"),
            "rtk {flag} dbt must show RTK info, not dbt wrapper help"
        );
    }
}

#[cfg(unix)]
mod child_argv {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const SHIM: &str = "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$DBT_ARGV_FILE\"\necho \"NATIVE-DBT-HELP: ${1:-}\"\nexit \"${DBT_EXIT:-0}\"\n";

    fn stub_dbt(dir: &Path) {
        let tool = dir.join("dbt");
        fs::write(&tool, SHIM).unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn argv_file(dir: &Path) -> std::path::PathBuf {
        dir.join("argv.txt")
    }

    fn command(dir: &Path, args: &[&str]) -> Command {
        let path = format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = rtk(dir);
        cmd.args(args)
            .env("PATH", path)
            .env("DBT_ARGV_FILE", argv_file(dir));
        cmd
    }

    fn recorded_argv(dir: &Path) -> Vec<String> {
        fs::read_to_string(argv_file(dir))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn subcommand_help_forwards_to_child() {
        let dir = tempfile::tempdir().unwrap();
        stub_dbt(dir.path());
        let out = command(dir.path(), &["dbt", "run", "--help"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            stdout(&out).contains("NATIVE-DBT-HELP"),
            "`rtk dbt run --help` must be native dbt help, not RTK help:\n{}",
            stdout(&out)
        );
        assert_eq!(recorded_argv(dir.path()), ["run", "--help"]);
    }

    #[test]
    fn unsupported_subcommands_forward_exact_argv_and_status() {
        for (args, code) in [
            (vec!["dbt", "seed"], "3"),
            (vec!["dbt", "docs", "generate"], "7"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            stub_dbt(dir.path());
            let out = command(dir.path(), &args)
                .env("DBT_EXIT", code)
                .output()
                .unwrap();
            assert_eq!(out.status.code(), code.parse().ok(), "{args:?}");
            assert_eq!(
                recorded_argv(dir.path()),
                &args[1..],
                "argv must reach dbt unchanged: {args:?}"
            );
        }
    }

    #[test]
    fn unknown_flags_forward_exact_argv_and_status() {
        let dir = tempfile::tempdir().unwrap();
        stub_dbt(dir.path());
        let out = command(dir.path(), &["dbt", "run", "--bogus-flag", "value"])
            .env("DBT_EXIT", "4")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4));
        assert_eq!(recorded_argv(dir.path()), ["run", "--bogus-flag", "value"]);
    }

    #[test]
    fn literal_double_dash_is_not_swallowed_by_rtk() {
        let dir = tempfile::tempdir().unwrap();
        stub_dbt(dir.path());
        let out = command(dir.path(), &["dbt", "test", "--", "--select", "model_x"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            recorded_argv(dir.path()),
            ["test", "--", "--select", "model_x"]
        );
    }

    #[test]
    fn argv_boundaries_preserve_quoting_and_punctuation() {
        let dir = tempfile::tempdir().unwrap();
        stub_dbt(dir.path());
        let vars = "{\"key\":\"a b\"}";
        let out = command(
            dir.path(),
            &[
                "dbt",
                "run",
                "--vars",
                vars,
                "--select",
                "tag:daily",
                "--exclude",
                "package:legacy",
            ],
        )
        .output()
        .unwrap();
        assert!(out.status.success());
        assert_eq!(
            recorded_argv(dir.path()),
            [
                "run",
                "--vars",
                vars,
                "--select",
                "tag:daily",
                "--exclude",
                "package:legacy"
            ]
        );
    }

    #[test]
    fn pre_subcommand_globals_preserve_child_argv_and_status() {
        // Valid RTK verbosity globals before `dbt` are consumed by RTK and must
        // not be rejected or forwarded: the child still sees only `seed` and
        // its exit status passes through.
        let cases: [&[&str]; 4] = [
            &["--verbose", "dbt", "seed"],
            &["-v", "dbt", "seed"],
            &["-vv", "dbt", "seed"],
            &["-vvv", "dbt", "seed"],
        ];
        for args in cases {
            let dir = tempfile::tempdir().unwrap();
            stub_dbt(dir.path());
            let out = command(dir.path(), args)
                .env("DBT_EXIT", "5")
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(5), "{args:?}");
            assert!(
                stdout(&out).contains("NATIVE-DBT-HELP"),
                "{args:?} must reach the child:\n{}",
                stdout(&out)
            );
            assert_eq!(recorded_argv(dir.path()), ["seed"], "{args:?}");
        }
    }

    #[test]
    fn leading_and_global_options_forward_argv_and_status() {
        // Leading flags and pre-subcommand options must reach dbt verbatim and
        // return its status, never be intercepted as RTK wrapper help. Only
        // bare `rtk dbt` and exact `-h`/`--help` are wrapper help. The
        // `--ultra-compact` child option (before and after the subcommand)
        // probes for RTK global-option interception by design.
        let cases: [&[&str]; 5] = [
            &["dbt", "--version"],
            &["dbt", "--unknown-flag"],
            &["dbt", "--target", "dev", "run"],
            &["dbt", "--ultra-compact", "run"],
            &["dbt", "run", "--ultra-compact"],
        ];
        for args in cases {
            let dir = tempfile::tempdir().unwrap();
            stub_dbt(dir.path());
            let out = command(dir.path(), args)
                .env("DBT_EXIT", "5")
                .output()
                .unwrap();
            assert_eq!(
                out.status.code(),
                Some(5),
                "{args:?} must return the child status, not wrapper help"
            );
            assert!(
                stdout(&out).contains("NATIVE-DBT-HELP"),
                "{args:?} must reach the child, not wrapper help:\n{}",
                stdout(&out)
            );
            assert_eq!(recorded_argv(dir.path()), &args[1..], "{args:?}");
        }
    }
}
