//! Exercises exact-goal updates through the public app-server API.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_features::Feature;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

const READ_TIMEOUT: Duration = Duration::from_secs(30);

async fn goal_server() -> Result<(TestAppServer, String, TempDir)> {
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new("http://127.0.0.1:1")
        .enable_feature(Feature::Goals)
        .write(codex_home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    let request = app_server
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = app_server.read_response(request).await?;
    Ok((app_server, thread.id, codex_home))
}

#[tokio::test]
async fn thread_goal_updates_require_a_matching_goal() -> Result<()> {
    let (mut app_server, thread_id, _codex_home) = goal_server().await?;
    let request = app_server
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread_id,
                "objective": "finish exact-goal updates",
            })),
        )
        .await?;
    let response: serde_json::Value = app_server.read_response(request).await?;

    let goal = &response["goal"];
    let goal_id = goal["goalId"].as_str().expect("goalId must be present");
    let unsafe_completion = app_server
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread_id,
                "status": "complete",
            })),
        )
        .await?;
    let completion_error: JSONRPCError = timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(unsafe_completion)),
    )
    .await??;
    assert_eq!(
        completion_error.error.message,
        "completing a goal requires ifMatches for the current goal"
    );

    let safe_stop = app_server
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread_id,
                "status": "blocked",
            })),
        )
        .await?;
    let stopped: serde_json::Value = app_server.read_response(safe_stop).await?;
    assert_eq!(stopped["goal"]["status"], json!("blocked"));

    let conditional_update = app_server
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread_id,
                "ifMatches": {
                    "goalId": goal_id,
                    "objective": goal["objective"],
                    "tokenBudget": goal["tokenBudget"],
                },
                "status": "complete",
            })),
        )
        .await?;
    let updated: serde_json::Value = app_server.read_response(conditional_update).await?;

    assert_eq!(updated["goal"]["status"], json!("complete"));

    let edit = app_server
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread_id,
                "objective": "finish the edited goal",
            })),
        )
        .await?;
    let edited: serde_json::Value = app_server.read_response(edit).await?;
    assert_eq!(edited["goal"]["goalId"], json!(goal_id));

    let stale_update = app_server
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread_id,
                "ifMatches": {
                    "goalId": goal_id,
                    "objective": "finish exact-goal updates",
                    "tokenBudget": null,
                },
                "status": "active",
            })),
        )
        .await?;
    let error: JSONRPCError = timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(stale_update)),
    )
    .await??;

    assert_eq!(
        error.error.message,
        "cannot update goal: the matched goal is no longer current"
    );
    Ok(())
}
