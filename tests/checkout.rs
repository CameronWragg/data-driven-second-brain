//! Checkouts, the Repository Entity `serve` makes for one, and the scope of
//! `search` (SPEC §2 step 4, §4.1–§4.3, §6.4, §14.1).

mod common;

use std::fs;
use std::path::Path;

use common::{PRISMA, PRISMA_PATH, TestBrain, call, redacted};
use rmcp::RoleClient;
use rmcp::service::RunningService;
use serde_json::json;

const API_SSH: &str = "git@github.com:Owner/api.git";
const API_HTTPS: &str = "https://github.com/Owner/api";
const API: &str = "repo:github.com/Owner/api";

/// The `context` result, with `checkout`'s path replaced so it can be
/// snapshotted.
async fn context(client: &RunningService<RoleClient, ()>, checkout: &Path) -> String {
    let (is_error, text) = call(client, "context", json!({})).await;
    assert!(!is_error, "{text}");
    text.replace(&checkout.display().to_string(), "[CHECKOUT]")
}

/// The Repository a `context` result names.
fn repository(context: &str) -> String {
    context
        .lines()
        .find_map(|line| line.strip_prefix("Repository: "))
        .unwrap_or_else(|| panic!("no Repository in {context}"))
        .to_owned()
}

#[tokio::test]
async fn the_first_serve_in_a_new_checkout_creates_its_repository_in_one_server_commit() {
    let test = TestBrain::new();
    let brain = test.init();
    let api = test.checkout("api", Some(API_SSH));
    let client = test.serve_in(&api).await;

    let text = context(&client, &api).await;

    insta::assert_snapshot!(text);
    assert_eq!(test.git(&brain, &["rev-list", "--count", "HEAD"]), "2\n");
    insta::assert_snapshot!(test.git(&brain, &["log", "-1", "--format=%B"]));
    let file = brain.join("entities/repositories/github.com/Owner/api.md");
    insta::assert_snapshot!(fs::read_to_string(file).unwrap());
    assert_eq!(test.git(&brain, &["status", "--porcelain"]), "");
    client.cancel().await.unwrap();

    let again = test.serve_in(&api).await;
    assert_eq!(repository(&context(&again, &api).await), API);
    assert_eq!(test.git(&brain, &["rev-list", "--count", "HEAD"]), "2\n");
    again.cancel().await.unwrap();
}

#[tokio::test]
async fn ssh_and_https_checkouts_of_one_repository_share_its_entity() {
    let test = TestBrain::new();
    let brain = test.init();
    let by_ssh = test.checkout("api-ssh", Some(API_SSH));
    let by_https = test.checkout("api-https", Some(API_HTTPS));

    let ssh = test.serve_for_project(&by_ssh).await;
    let from_ssh = repository(&context(&ssh, &by_ssh).await);
    ssh.cancel().await.unwrap();
    let https = test.serve_for_project(&by_https).await;
    let from_https = repository(&context(&https, &by_https).await);
    https.cancel().await.unwrap();

    assert_eq!((from_ssh.as_str(), from_https.as_str()), (API, API));
    assert_eq!(test.git(&brain, &["rev-list", "--count", "HEAD"]), "2\n");
}

#[tokio::test]
async fn a_repository_without_a_remote_keeps_its_local_id_across_restarts() {
    let test = TestBrain::new();
    let brain = test.init();
    let notes = test.checkout("notes", None);

    let first = test.serve_in(&notes).await;
    let id = repository(&context(&first, &notes).await);
    first.cancel().await.unwrap();
    let second = test.serve_in(&notes).await;
    let again = repository(&context(&second, &notes).await);
    second.cancel().await.unwrap();

    assert!(id.starts_with("repo:local/"), "{id}");
    assert_eq!(again, id);
    let ulid = id.strip_prefix("repo:local/").unwrap();
    assert_eq!(
        fs::read_to_string(notes.join(".git/latens-id"))
            .unwrap()
            .trim(),
        ulid
    );
    assert!(
        brain
            .join(format!("entities/repositories/local/{ulid}.md"))
            .is_file()
    );
}

#[tokio::test]
async fn context_without_a_checkout_says_so_and_path_detects_one() {
    let test = TestBrain::new();
    test.init();
    let api = test.checkout("api", Some(API_SSH));
    let client = test.serve().await;

    let (_, without) = call(&client, "context", json!({})).await;
    let (is_error, with) = call(
        &client,
        "context",
        json!({ "path": api.join("README.md").parent().unwrap() }),
    )
    .await;
    let (_, after) = call(&client, "context", json!({})).await;

    let home = test.home().display().to_string();
    insta::assert_snapshot!(without.replace(&home, "[HOME]"));
    assert!(!is_error, "{with}");
    assert_eq!(repository(&with), API);
    assert_eq!(repository(&after), API);
    for text in [without, with] {
        assert!(text.chars().count() < 4_000, "{text}");
    }
    client.cancel().await.unwrap();
}

/// A `create` operation for a caveat about `about` that mentions `word`.
fn caveat(title: &str, about: &str) -> serde_json::Value {
    json!({
        "op": "create",
        "kind": "caveat",
        "title": title,
        "summary": "The shadow database must exist.",
        "body": "## Symptom\n\nNothing applies.\n\n## Cause\n\nNo shadow database.\n\n## Avoidance\n\nMake one.\n",
        "links": [{ "type": "about", "to": about }],
    })
}

/// Writes `operations`, failing the test if the batch is rejected.
async fn write_ok(
    client: &RunningService<RoleClient, ()>,
    operations: serde_json::Value,
) -> String {
    let (is_error, text) = call(client, "write", json!({ "operations": operations })).await;
    assert!(!is_error, "{text}");
    text
}

/// The IDs in a `search` result, in rank order.
async fn search_ids(
    client: &RunningService<RoleClient, ()>,
    arguments: serde_json::Value,
) -> Vec<String> {
    let (is_error, text) = call(client, "search", arguments).await;
    assert!(!is_error, "{text}");
    text.split("id=\"")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_recollection_written_on_a_branch_is_found_only_there_unless_asked_for() {
    let test = TestBrain::new();
    let brain = test.init();
    let api = test.checkout("api", Some(API_SSH));
    test.git(&api, &["checkout", "--quiet", "-b", "feature/x"]);
    let client = test.serve_in(&api).await;

    let written = write_ok(&client, json!([caveat("Shadow on a branch", API)])).await;
    let id = written.split_whitespace().nth(3).unwrap().to_owned();

    let file = fs::read_to_string(brain.join(format!("recollections/{id}.md"))).unwrap();
    assert!(file.contains("branch: feature/x"), "{file}");
    let message = test.git(&brain, &["log", "-1", "--format=%B"]);
    redacted().bind(|| insta::assert_snapshot!(message));
    assert!(
        search_ids(&client, json!({ "query": "shadow" }))
            .await
            .contains(&id)
    );

    test.git(&api, &["checkout", "--quiet", "main"]);
    assert!(
        !search_ids(&client, json!({ "query": "shadow" }))
            .await
            .contains(&id)
    );
    let (_, labelled) = call(
        &client,
        "search",
        json!({ "query": "shadow", "branches": ["feature/x"] }),
    )
    .await;
    assert!(labelled.contains(&format!("id=\"{id}\"")), "{labelled}");
    assert!(labelled.contains("branch=\"feature/x\""), "{labelled}");
    assert!(
        search_ids(&client, json!({ "query": "shadow", "branches": "all" }))
            .await
            .contains(&id)
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_write_on_the_default_branch_records_no_branch() {
    let test = TestBrain::new();
    let brain = test.init();
    let api = test.checkout("api", Some(API_SSH));
    let client = test.serve_in(&api).await;

    let written = write_ok(&client, json!([caveat("Shadow on main", API)])).await;

    let id = written.split_whitespace().nth(3).unwrap();
    let file = fs::read_to_string(brain.join(format!("recollections/{id}.md"))).unwrap();
    assert!(file.contains("branch: null"), "{file}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn search_covers_the_current_repository_by_default_and_others_when_asked() {
    let test = TestBrain::new();
    test.init();
    test.place_entity(PRISMA_PATH, PRISMA);
    let api = test.checkout("api", Some(API_SSH));
    let web = test.checkout("web", Some("git@github.com:Owner/web.git"));
    let web_client = test.serve_in(&web).await;
    let about_web = write_ok(
        &web_client,
        json!([caveat("Shadow in web", "repo:github.com/Owner/web")]),
    )
    .await;
    web_client.cancel().await.unwrap();
    let about_web = about_web.split_whitespace().nth(3).unwrap().to_owned();
    let client = test.serve_in(&api).await;
    let about_api = write_ok(&client, json!([caveat("Shadow in api", API)])).await;
    let about_api = about_api.split_whitespace().nth(3).unwrap().to_owned();

    let current = search_ids(&client, json!({ "query": "shadow" })).await;
    let all = search_ids(&client, json!({ "query": "shadow", "scope": "all" })).await;
    let other = search_ids(
        &client,
        json!({ "query": "shadow", "scope": "repo:github.com/Owner/web" }),
    )
    .await;
    let (is_error, unknown) = call(
        &client,
        "search",
        json!({ "query": "shadow", "scope": "repo:github.com/Owner/nowhere" }),
    )
    .await;

    assert_eq!(current, std::slice::from_ref(&about_api));
    assert!(
        all.contains(&about_api) && all.contains(&about_web),
        "{all:?}"
    );
    assert_eq!(other, [about_web]);
    assert!(is_error && unknown.starts_with("not_found:"), "{unknown}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn search_without_a_checkout_says_current_acts_as_all() {
    let test = TestBrain::new();
    test.init();
    let client = test.serve().await;

    let (is_error, text) = call(&client, "search", json!({ "query": "shadow" })).await;

    assert!(!is_error, "{text}");
    insta::assert_snapshot!(text);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn create_entity_and_update_entity_make_and_change_entities_get_shows() {
    let test = TestBrain::new();
    let brain = test.init();
    let client = test.serve().await;

    let created = write_ok(
        &client,
        json!([
            {
                "op": "create_entity",
                "id": "dep:npm:zod",
                "name": "zod",
                "description": "Schema validation.",
                "docs": [{ "versions": ">=3", "url": "https://zod.dev" }],
                "ref": "z",
            },
            caveat("Zod needs a shadow", "ref:z"),
        ]),
    )
    .await;
    redacted().bind(|| insta::assert_snapshot!(created));
    let message = test.git(&brain, &["log", "-1", "--format=%B"]);
    redacted().bind(|| insta::assert_snapshot!(message));
    let version = created
        .lines()
        .next()
        .unwrap()
        .split("version ")
        .nth(1)
        .unwrap()
        .trim_end_matches(')')
        .to_owned();

    let (is_error, updated) = call(
        &client,
        "write",
        json!({ "operations": [{
            "op": "update_entity",
            "id": "dep:npm:zod",
            "version": version,
            "description": "TypeScript-first schema validation.",
        }] }),
    )
    .await;
    assert!(!is_error, "{updated}");
    let (_, stale) = call(
        &client,
        "write",
        json!({ "operations": [{
            "op": "update_entity",
            "id": "dep:npm:zod",
            "version": version,
            "name": "Zod",
        }] }),
    )
    .await;
    assert!(stale.contains("conflict"), "{stale}");

    let (is_error, got) = call(&client, "get", json!({ "ids": ["dep:npm:zod"] })).await;
    assert!(!is_error, "{got}");
    redacted().bind(|| insta::assert_snapshot!(got));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn entity_operations_report_every_problem() {
    let test = TestBrain::new();
    test.init();
    test.place_entity(PRISMA_PATH, PRISMA);
    let client = test.serve().await;

    let (is_error, text) = call(
        &client,
        "write",
        json!({ "operations": [
            { "op": "create_entity", "id": "dep:npm:prisma", "name": "prisma" },
            { "op": "create_entity", "id": "repo:local/01JB3K8V9Q0000000000000000", "name": "x" },
            {
                "op": "create_entity",
                "id": "repo:github.com/Owner/api",
                "name": " ",
                "docs": [],
                "default_branch": "main",
            },
            { "op": "create_entity", "id": "not an id", "name": "x" },
            { "op": "update_entity", "id": "dep:npm:nowhere", "version": "0" },
        ] }),
    )
    .await;

    assert!(is_error, "{text}");
    insta::assert_snapshot!(text);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn get_on_an_entity_lists_ten_recollections_about_it_then_how_many_more() {
    let test = TestBrain::new();
    test.init();
    test.place_entity(PRISMA_PATH, PRISMA);
    let client = test.serve().await;
    let operations: Vec<_> = (0..12)
        .map(|n| caveat(&format!("Prisma caveat {n}"), "dep:npm:prisma"))
        .collect();
    write_ok(&client, json!(operations)).await;

    let (is_error, got) = call(&client, "get", json!({ "ids": ["dep:npm:prisma"] })).await;

    assert!(!is_error, "{got}");
    assert_eq!(got.matches("<recollection id=").count(), 10, "{got}");
    assert!(got.contains("+2 more"), "{got}");
    assert!(got.contains("`entities: [\"dep:npm:prisma\"]`"), "{got}");
    client.cancel().await.unwrap();
}
