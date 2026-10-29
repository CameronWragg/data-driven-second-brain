//! Recovery at startup: the write journal rolled forward, and hand edits
//! committed as the owner's (SPEC §2 step 2, §5.3, §10, §16.3).

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{PRISMA, PRISMA_PATH, TestBrain, call, redacted};
use rmcp::RoleClient;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use serde_json::json;

/// A brain holding the Prisma Entity, committed.
fn brain_with_prisma(test: &TestBrain) -> PathBuf {
    let brain = test.init();
    test.place_entity(PRISMA_PATH, PRISMA);
    test.git(&brain, &["add", "."]);
    test.git(&brain, &["commit", "--quiet", "--message", "Add Prisma"]);
    brain
}

/// A valid `create` operation for a caveat about Prisma.
fn prisma_caveat(title: &str) -> serde_json::Value {
    json!({
        "op": "create",
        "kind": "caveat",
        "title": title,
        "summary": "`prisma migrate dev` exits 0 but applies nothing when SHADOW_DATABASE_URL is unset.",
        "body": "## Symptom\n\nNo migration runs.\n\n## Cause\n\nNo shadow database.\n\n## Avoidance\n\nSet SHADOW_DATABASE_URL.\n",
        "links": [{ "type": "about", "to": "dep:npm:prisma" }],
    })
}

/// Creates a Prisma caveat titled `title` in its own `serve`, returning the
/// file it was written to.
async fn written_caveat(test: &TestBrain, title: &str) -> PathBuf {
    let client = test.serve().await;
    let (is_error, text) = call(
        &client,
        "write",
        json!({ "operations": [prisma_caveat(title)] }),
    )
    .await;
    assert!(!is_error, "{text}");
    client.cancel().await.unwrap();
    let brain = test.default_brain();
    let file = test.git(&brain, &["show", "--name-only", "--format=", "HEAD"]);
    brain.join(file.trim())
}

/// The ID of the Recollection whose file is `file`.
fn id_of(file: &Path) -> String {
    file.file_stem().unwrap().to_str().unwrap().to_owned()
}

/// The IDs in a `search` result for `query`, in rank order.
async fn search_ids(client: &RunningService<RoleClient, ()>, query: &str) -> Vec<String> {
    let (is_error, text) = call(client, "search", json!({ "query": query })).await;
    assert!(!is_error, "{text}");
    text.lines()
        .filter_map(|line| line.split("id=\"").nth(1))
        .map(|rest| rest.split('"').next().unwrap().to_owned())
        .collect()
}

/// Waits up to 10 s for `done`, failing the test if it never is.
async fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A `pre-commit` hook that marks that it ran, then holds the commit until
/// its release file appears and fails it. It uses only shell builtins, since
/// the harness's `PATH` holds nothing but `git`.
const HOLDING_HOOK: &str = "#!/bin/sh
: > \"$0.ran\"
while [ ! -e \"$0.release\" ]; do :; done
exit 1
";

#[tokio::test]
async fn a_batch_killed_between_its_journal_and_its_commit_lands_with_its_own_trailers() {
    let test = TestBrain::new();
    let brain = brain_with_prisma(&test);
    let hook = brain.join(".git/hooks/pre-commit");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, HOLDING_HOOK).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let (client, pid) = test.serve_killable().await;
    let batch = json!({ "operations": [prisma_caveat("Prisma migrations fail silently")] });
    let writing = tokio::spawn(async move {
        // The call never answers: `serve` is killed while it waits.
        let request =
            CallToolRequestParams::new("write").with_arguments(batch.as_object().unwrap().clone());
        let _ = client.call_tool(request).await;
    });
    let ran = brain.join(".git/hooks/pre-commit.ran");
    wait_for("the commit to start", || ran.exists()).await;

    let killed = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let _ = writing.await;
    // The orphaned `git` fails its commit and cleans up after itself.
    fs::remove_file(&hook).unwrap();
    fs::write(brain.join(".git/hooks/pre-commit.release"), "").unwrap();
    let git_lock = brain.join(".git/index.lock");
    wait_for("the orphaned git to exit", || !git_lock.exists()).await;
    assert!(brain.join(".index/journal").exists());
    let client = test.serve().await;
    let (is_error, text) = call(&client, "context", json!({})).await;
    assert!(!is_error, "{text}");

    let message = test.git(&brain, &["log", "-1", "--format=%B"]);
    redacted().bind(|| insta::assert_snapshot!(message));
    assert!(message.contains("Author: agent"), "{message}");
    assert_eq!(test.git(&brain, &["rev-list", "--count", "HEAD"]), "3\n");
    assert_eq!(test.git(&brain, &["status", "--porcelain"]), "");
    assert!(!brain.join(".index/journal").exists());
    let file = test.git(&brain, &["show", "--name-only", "--format=", "HEAD"]);
    let id = id_of(Path::new(file.trim()));
    assert_eq!(search_ids(&client, "shadow").await, [id]);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_file_edited_by_hand_between_sessions_is_committed_as_the_owners_and_re_indexed() {
    let test = TestBrain::new();
    let brain = brain_with_prisma(&test);
    let file = written_caveat(&test, "Prisma migrations fail silently").await;
    let edited = fs::read_to_string(&file)
        .unwrap()
        .replace("No migration runs.", "No replica runs.");
    fs::write(&file, edited).unwrap();

    let client = test.serve().await;

    assert_eq!(search_ids(&client, "replica").await, [id_of(&file)]);
    assert_eq!(search_ids(&client, "migration").await, [] as [String; 0]);
    let message = test.git(&brain, &["log", "-1", "--format=%B"]);
    redacted().bind(|| insta::assert_snapshot!(message));
    assert!(message.contains(&format!("Recollection: {}", id_of(&file))));
    assert_eq!(test.git(&brain, &["status", "--porcelain"]), "");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_hand_edit_that_breaks_the_schema_is_searchable_and_listed_for_repair() {
    let test = TestBrain::new();
    let brain = brain_with_prisma(&test);
    let file = written_caveat(&test, "Prisma migrations fail silently").await;
    let broken = fs::read_to_string(&file)
        .unwrap()
        .replace("state: active", "priority: high")
        .replace("No migration runs.", "No replica runs.");
    fs::write(&file, broken).unwrap();

    let client = test.serve().await;

    assert_eq!(search_ids(&client, "replica").await, [id_of(&file)]);
    let (is_error, context) = call(&client, "context", json!({})).await;
    assert!(!is_error, "{context}");
    let context = context.replace(&test.home().display().to_string(), "[HOME]");
    redacted().bind(|| insta::assert_snapshot!(context));
    assert!(
        test.git(&brain, &["log", "-1", "--format=%B"])
            .contains("Author: owner")
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn with_nothing_changed_since_the_last_session_startup_commits_nothing() {
    let test = TestBrain::new();
    let brain = brain_with_prisma(&test);
    written_caveat(&test, "Prisma migrations fail silently").await;
    let head = test.git(&brain, &["rev-parse", "HEAD"]);

    let client = test.serve().await;
    call(&client, "context", json!({})).await;

    assert_eq!(test.git(&brain, &["rev-parse", "HEAD"]), head);
    client.cancel().await.unwrap();
}
