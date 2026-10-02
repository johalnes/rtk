//! Interactive streaming contract for `rtk uv run <dbt|rtk>` (what the hook
//! produces from `uv run dbt debug`).
//!
//! A `dbt debug` OAuth prompt carries no newline and blocks on stdin, so it is
//! only visible before child exit when rtk inherits the child's stdio. The uv
//! wrapper used to capture both streams and release them after exit, so the
//! prompt stayed invisible until the process was dead. These tests pin
//! inherited-stdio passthrough for each option layout that finds dbt, while a
//! `dbt` option value leaves an unrelated program on the generic capture filter.
//!
//! Unix-only (child-shim mechanism), matching `dbt_cli_test`. `RTK_TEST_BIN`
//! may point at another rtk build (used to prove the pre-fix red).

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const PROMPT: &[u8] = b"PROMPT>";
const WAIT: Duration = Duration::from_secs(8);

// Skip uv options (and separate values) to reach the inner command. `--active`
// is deliberately absent from the production tables.
const UV_SHIM: &str = r#"#!/bin/sh
shift
while [ $# -gt 0 ]; do
  case "$1" in
    --project|--directory|--package|--with|--with-requirements|--python|--env-file|--group|--extra) shift 2 ;;
    --) shift; break ;;
    -*) shift ;;
    *) break ;;
  esac
done
exec "$@"
"#;

// Prompt without a newline, then block on stdin. Output only follows the
// harness answering, so seeing the prompt proves the child has not finished.
const PROMPT_SHIM: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$SHIM_ARGV_FILE"
printf 'PROMPT>'
printf 'PROMPT>' >&2
IFS= read -r answer
printf 'answer:%s\n' "$answer"
printf 'ERR:%s\n' "$answer" >&2
exit "${SHIM_EXIT:-7}"
"#;

const PROG_SHIM: &str = "#!/bin/sh\nprintf 'UNIQUE-NOISE\\n'\nprintf 'ERROR: boom\\n'\nexit 1\n";

fn setup(dir: &Path) {
    for (name, body) in [
        ("uv", UV_SHIM),
        ("dbt", PROMPT_SHIM),
        ("rtk", PROMPT_SHIM),
        ("prog", PROG_SHIM),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn base(arg: &str) -> Option<&str> {
    Path::new(arg).file_name().and_then(|n| n.to_str())
}

fn reader(
    mut stream: impl Read + Send + 'static,
    tx: mpsc::Sender<Vec<u8>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    })
}

/// Accumulate until `buf` starts with the fixed prompt or the deadline passes.
fn wait_for_prompt(rx: &Receiver<Vec<u8>>, buf: &mut Vec<u8>) -> bool {
    let deadline = Instant::now() + WAIT;
    loop {
        if buf.starts_with(PROMPT) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        match rx.recv_timeout(left) {
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(_) => return false,
        }
    }
}

struct Run {
    code: Option<i32>,
    prompted: bool,
    stdout: String,
    stderr: String,
}

fn run(dir: &Path, args: &[&str]) -> Run {
    let binary = std::env::var_os("RTK_TEST_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_BIN_EXE_rtk")));
    let mut child = Command::new(binary)
        .args(args)
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
        .env("SHIM_ARGV_FILE", dir.join("inner_argv.txt"))
        .env_remove("RTK_RECALL")
        .env_remove("RTK_TEE")
        .env_remove("RTK_NO_TOML")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rtk");

    let mut stdin = child.stdin.take().unwrap();
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    let out_reader = reader(child.stdout.take().unwrap(), out_tx);
    let err_reader = reader(child.stderr.take().unwrap(), err_tx);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let prompted = wait_for_prompt(&out_rx, &mut stdout) & wait_for_prompt(&err_rx, &mut stderr);

    // Answer and reap before asserting: a failed prompt check must never leave
    // the child blocked.
    let _ = stdin.write_all(b"yes\n");
    let _ = stdin.flush();
    drop(stdin);
    let status = child.wait().expect("reap rtk");
    // Reaping the child closes the pipes, so each reader hits EOF and ends;
    // joining guarantees the final bytes are in the channel before draining.
    out_reader.join().unwrap();
    err_reader.join().unwrap();
    for chunk in out_rx.try_iter() {
        stdout.extend_from_slice(&chunk);
    }
    for chunk in err_rx.try_iter() {
        stderr.extend_from_slice(&chunk);
    }

    Run {
        code: status.code(),
        prompted,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

#[test]
fn uv_run_dbt_debug_streams_prompts_before_stdin() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let dbt_abs = dir.path().join("dbt").to_string_lossy().into_owned();
    let cases: [&[&str]; 7] = [
        &["uv", "run", "dbt", "debug"],
        &[
            "uv",
            "run",
            "--project",
            "project with spaces",
            "dbt",
            "debug",
        ],
        &["uv", "run", "--project=myproj", "dbt", "debug"],
        &["uv", "run", "--active", "dbt", "debug"],
        &["uv", "run", "--", "dbt", "debug"],
        &["uv", "run", "rtk", "dbt", "debug"],
        &["uv", "run", &dbt_abs, "debug"],
    ];

    for args in cases {
        let out = run(dir.path(), args);
        assert!(
            out.prompted,
            "{args:?}: stdout and stderr prompts must arrive before stdin is \
             answered; stdout={:?} stderr={:?}",
            out.stdout, out.stderr
        );
        assert_eq!(out.code, Some(7), "{args:?}: exit code must pass through");
        assert_eq!(out.stdout, "PROMPT>answer:yes\n", "{args:?}");
        assert_eq!(out.stderr, "PROMPT>ERR:yes\n", "{args:?}");
        assert!(
            !out.stdout.contains("rtk") && !out.stderr.contains("rtk"),
            "{args:?}: passthrough must add no recovery hint"
        );

        // The inner executable sees its own args: everything after the dbt/rtk
        // token (that token is argv[0], not in `$@`).
        let start = args
            .iter()
            .position(|arg| matches!(base(arg), Some("dbt" | "rtk")))
            .expect("a dbt/rtk token");
        let expected = args[start + 1..].join("\n") + "\n";
        assert_eq!(
            std::fs::read_to_string(dir.path().join("inner_argv.txt")).unwrap(),
            expected,
            "{args:?}: inner argv"
        );
    }
}

#[test]
fn uv_run_non_dbt_program_keeps_generic_filtering() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    for args in [
        &["uv", "run", "--project", "dbt", "prog"][..],
        &["uv", "run", "--project=dbt", "prog"][..],
    ] {
        let out = run(dir.path(), args);
        assert_eq!(out.code, Some(1), "{args:?}");
        assert_eq!(
            out.stdout, "ERROR: boom\n",
            "{args:?}: a `dbt` option value must not widen passthrough"
        );
    }
}
