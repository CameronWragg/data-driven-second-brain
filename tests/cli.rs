//! The `latens` binary's command line, as an owner or Agent sees it.

mod common;

use common::{TestBrain, stderr, stdout};

fn latens(args: &[&str]) -> std::process::Output {
    TestBrain::new().latens(args)
}

#[test]
fn version_prints_the_crate_version() {
    let output = latens(&["--version"]);

    assert!(output.status.success());
    assert_eq!(stdout(&output), "latens 0.1.0\n");
}

#[test]
fn help_lists_every_subcommand() {
    let output = latens(&["--help"]);

    assert!(output.status.success());
    let help = stdout(&output);
    for subcommand in ["init", "serve", "reindex", "eval", "hook"] {
        assert!(
            help.lines()
                .any(|line| line.trim_start().starts_with(subcommand)),
            "`{subcommand}` missing from:\n{help}"
        );
    }
}

#[test]
fn hook_help_lists_stop() {
    let output = latens(&["hook", "--help"]);

    assert!(output.status.success());
    assert!(stdout(&output).contains("stop"));
}

#[test]
fn unbuilt_subcommands_fail_and_say_so() {
    for args in [
        &["init"][..],
        &["init", "--path", "/brains/mine"],
        &["serve"],
        &["reindex"],
        &["eval"],
        &["hook", "stop"],
    ] {
        let output = latens(args);

        assert!(
            !output.status.success(),
            "`latens {}` succeeded",
            args.join(" ")
        );
        let stderr = stderr(&output);
        assert!(
            stderr.contains("not implemented yet"),
            "`latens {}` stderr: {stderr}",
            args.join(" ")
        );
    }
}
