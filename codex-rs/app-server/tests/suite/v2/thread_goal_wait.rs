//! Exercises native goal waiting through the public app-server API.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_sequence;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadGoalSetResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn declared_wait_suppresses_automatic_goal_continuation() -> Result<()> {
    let server = create_mock_responses_server_sequence(vec![
        responses::sse(vec![
            responses::ev_response_created("wait-call"),
            responses::ev_function_call(
                "wait-goal",
                "wait_goal",
                r#"{"state_key":"authority-v1","reason":"Need a verified authorization"}"#,
            ),
            responses::ev_completed("wait-call"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("wait-final"),
            responses::ev_assistant_message("wait-message", "Waiting for authorization"),
            responses::ev_completed("wait-final"),
        ]),
    ])
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::Goals)
        .write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized_with_timeout(Duration::from_secs(30))
        .await?;
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    let request = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(json!({"threadId": thread.id, "objective": "Finish after authorization"})),
        )
        .await?;
    let _: ThreadGoalSetResponse = mcp.read_response(request).await?;
    let started: serde_json::Value = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let completed: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(TurnStatus::Completed, completed.turn.status);
    assert_eq!(None, completed.turn.error);
    assert_eq!(
        completed.turn.id,
        started["turn"]["id"].as_str().unwrap_or_default()
    );
    let request = mcp
        .send_raw_request("thread/goal/get", Some(json!({"threadId": thread.id})))
        .await?;
    let goal_get: serde_json::Value = mcp.read_response(request).await?;
    assert_eq!(json!("authority-v1"), goal_get["wait"]["stateKey"]);
    assert!(
        goal_get["wait"]["goalId"]
            .as_str()
            .is_some_and(|goal_id| !goal_id.is_empty())
    );
    assert_eq!(json!(1), goal_get["wait"]["generation"]);

    let requests = server.received_requests().await.unwrap_or_default();
    let model_requests = requests
        .iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .count();
    assert_eq!(2, model_requests);
    match timeout(
        Duration::from_millis(500),
        mcp.read_notification::<serde_json::Value>("turn/started"),
    )
    .await
    {
        Err(_) => {}
        Ok(Ok(started)) => panic!("waiting goal started another turn: {started}"),
        Ok(Err(error)) => panic!("failed to read next turn notification: {error}"),
    }
    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn queued_turn_does_not_resume_an_unrelated_waiting_goal() -> Result<()> {
    let server = create_mock_responses_server_sequence(vec![
        responses::sse(vec![
            responses::ev_response_created("wait-call"),
            responses::ev_function_call(
                "wait-goal",
                "wait_goal",
                r#"{"state_key":"dependency-v1","reason":"Need a changed dependency"}"#,
            ),
            responses::ev_completed("wait-call"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("wait-final"),
            responses::ev_assistant_message("wait-message", "Waiting"),
            responses::ev_completed("wait-final"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("queued-turn"),
            responses::ev_assistant_message("queued-message", "Queued input handled"),
            responses::ev_completed_with_tokens("queued-turn", 50),
        ]),
    ])
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::Goals)
        .write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized_with_timeout(Duration::from_secs(30))
        .await?;
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    let request = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(json!({"threadId": thread.id, "objective": "Wait for dependency"})),
        )
        .await?;
    let _: ThreadGoalSetResponse = mcp.read_response(request).await?;
    let _: serde_json::Value = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let _: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;

    let request = mcp
        .send_raw_request(
            "thread/queue/add",
            Some(json!({
                "threadId": thread.id,
                "input": [{"type": "text", "text": "Unrelated queued work", "textElements": []}],
                "clientUserMessageId": "unrelated-queued-work",
            })),
        )
        .await?;
    let _: serde_json::Value = mcp.read_response(request).await?;
    let _: serde_json::Value = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let completed: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(TurnStatus::Completed, completed.turn.status);

    let request = mcp
        .send_raw_request("thread/goal/get", Some(json!({"threadId": thread.id})))
        .await?;
    let goal_get: serde_json::Value = mcp.read_response(request).await?;
    assert_eq!(json!("dependency-v1"), goal_get["wait"]["stateKey"]);
    assert_eq!(json!("active"), goal_get["goal"]["status"]);
    assert_eq!(json!(0), goal_get["goal"]["tokensUsed"]);
    assert!(
        timeout(
            Duration::from_millis(500),
            mcp.read_notification::<serde_json::Value>("turn/started"),
        )
        .await
        .is_err(),
        "queued work must not start the waiting goal"
    );
    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn objective_change_resumes_waiting_goal_but_unchanged_set_does_not() -> Result<()> {
    let server = create_mock_responses_server_sequence(vec![
        responses::sse(vec![
            responses::ev_response_created("initial-wait-call"),
            responses::ev_function_call(
                "initial-wait-goal",
                "wait_goal",
                r#"{"state_key":"objective-v1","reason":"Need a changed objective"}"#,
            ),
            responses::ev_completed("initial-wait-call"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("initial-wait-final"),
            responses::ev_assistant_message("initial-wait-message", "Waiting"),
            responses::ev_completed("initial-wait-final"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("resumed-complete-call"),
            responses::ev_function_call(
                "resumed-complete-goal",
                "update_goal",
                r#"{"status":"complete"}"#,
            ),
            responses::ev_completed("resumed-complete-call"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("resumed-final"),
            responses::ev_assistant_message("resumed-message", "Finished changed objective"),
            responses::ev_completed("resumed-final"),
        ]),
    ])
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::Goals)
        .write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized_with_timeout(Duration::from_secs(30))
        .await?;
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    let set_goal = |objective: &str| json!({"threadId": thread.id, "objective": objective});
    let request = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(set_goal("Finish after objective changes")),
        )
        .await?;
    let _: ThreadGoalSetResponse = mcp.read_response(request).await?;
    let _: serde_json::Value = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let _: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;

    for _ in 0..3 {
        let request = mcp
            .send_raw_request(
                "thread/goal/set",
                Some(set_goal("Finish after objective changes")),
            )
            .await?;
        let _: ThreadGoalSetResponse = mcp.read_response(request).await?;
    }
    assert!(
        timeout(
            Duration::from_millis(500),
            mcp.read_notification::<serde_json::Value>("turn/started"),
        )
        .await
        .is_err(),
        "unchanged goal set must preserve the wait"
    );

    let request = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(set_goal("Finish the revised objective")),
        )
        .await?;
    let _: ThreadGoalSetResponse = mcp.read_response(request).await?;
    let _: serde_json::Value = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let completed: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(TurnStatus::Completed, completed.turn.status);
    assert_eq!(None, completed.turn.error);
    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn direct_user_turn_clears_wait_and_restores_goal_continuation() -> Result<()> {
    let server = create_mock_responses_server_sequence(vec![
        responses::sse(vec![
            responses::ev_response_created("initial-wait-call"),
            responses::ev_function_call(
                "initial-wait-goal",
                "wait_goal",
                r#"{"state_key":"user-v1","reason":"Need user input"}"#,
            ),
            responses::ev_completed("initial-wait-call"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("initial-wait-final"),
            responses::ev_assistant_message("initial-wait-message", "Waiting"),
            responses::ev_completed("initial-wait-final"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("user-turn"),
            responses::ev_assistant_message("user-turn-message", "I can continue"),
            responses::ev_completed("user-turn"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("goal-complete-call"),
            responses::ev_function_call("goal-complete", "update_goal", r#"{"status":"complete"}"#),
            responses::ev_completed("goal-complete-call"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("goal-final"),
            responses::ev_assistant_message("goal-final-message", "Goal complete"),
            responses::ev_completed("goal-final"),
        ]),
    ])
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::Goals)
        .write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized_with_timeout(Duration::from_secs(30))
        .await?;
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    let request = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(json!({"threadId": thread.id, "objective": "Wait for user"})),
        )
        .await?;
    let _: ThreadGoalSetResponse = mcp.read_response(request).await?;
    let _: serde_json::Value = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let _: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                input: vec![UserInput::Text {
                    text: "I have new information".to_string(),
                    text_elements: Vec::new(),
                }],
                model: Some("gpt-5.4".to_string()),
                ..Default::default()
            },
        })
        .await?;
    let _: serde_json::Value = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let _: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    let _: serde_json::Value = timeout(
        Duration::from_secs(5),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let completed: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(TurnStatus::Completed, completed.turn.status);
    assert_eq!(None, completed.turn.error);
    server.verify().await;
    Ok(())
}

#[test_case::test_case(false; "live_thread")]
#[test_case::test_case(true; "after_restart")]
#[tokio::test]
async fn changed_wake_starts_one_turn_and_duplicate_is_ignored(
    restart_after_wait: bool,
) -> Result<()> {
    let server = create_mock_responses_server_sequence(vec![
        responses::sse(vec![
            responses::ev_response_created("initial-wait-call"),
            responses::ev_function_call(
                "initial-wait-goal",
                "wait_goal",
                r#"{"state_key":"dependency-v1","reason":"Need a verified dependency result"}"#,
            ),
            responses::ev_completed("initial-wait-call"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("initial-wait-final"),
            responses::ev_assistant_message("initial-wait-message", "Waiting"),
            responses::ev_completed("initial-wait-final"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("woken-complete-call"),
            responses::ev_function_call(
                "woken-complete-goal",
                "update_goal",
                r#"{"status":"complete"}"#,
            ),
            responses::ev_completed("woken-complete-call"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("woken-final"),
            responses::ev_assistant_message("woken-message", "Dependency handled"),
            responses::ev_completed("woken-final"),
        ]),
    ])
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::Goals)
        .write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized_with_timeout(Duration::from_secs(30))
        .await?;
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    let request = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(json!({"threadId": thread.id, "objective": "Finish when dependency changes"})),
        )
        .await?;
    let _: ThreadGoalSetResponse = mcp.read_response(request).await?;
    let _: serde_json::Value = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let _: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    if restart_after_wait {
        drop(mcp);
        mcp = TestAppServer::builder()
            .with_codex_home(codex_home.path())
            .without_managed_config()
            .build_initialized_with_timeout(Duration::from_secs(30))
            .await?;
        let request = mcp
            .send_raw_request("thread/goal/get", Some(json!({"threadId": thread.id})))
            .await?;
        let persisted: serde_json::Value = mcp.read_response(request).await?;
        let request = mcp
            .send_raw_request(
                "thread/goal/wake",
                Some(json!({
                    "threadId": thread.id,
                    "goalId": persisted["wait"]["goalId"],
                    "observedGeneration": persisted["wait"]["generation"],
                    "stateKey": "dependency-v2"
                })),
            )
            .await?;
        let cold_wake = mcp
            .read_stream_until_error_message(RequestId::Integer(request))
            .await?;
        assert_eq!(
            "resume the thread before waking its goal",
            cold_wake.error.message
        );
        let request = mcp
            .send_thread_resume_request(ThreadResumeParams {
                thread_id: thread.id.clone(),
                ..Default::default()
            })
            .await?;
        let _: ThreadResumeResponse = mcp.read_response(request).await?;
        assert!(
            timeout(
                Duration::from_millis(500),
                mcp.read_notification::<serde_json::Value>("turn/started"),
            )
            .await
            .is_err(),
            "restored wait must suppress automatic continuation"
        );
    }
    let request = mcp
        .send_raw_request("thread/goal/get", Some(json!({"threadId": thread.id})))
        .await?;
    let goal_get: serde_json::Value = mcp.read_response(request).await?;
    let wake = json!({
        "threadId": thread.id,
        "goalId": goal_get["wait"]["goalId"],
        "observedGeneration": goal_get["wait"]["generation"],
        "stateKey": "dependency-v2"
    });
    let mut unchanged = wake.clone();
    unchanged["stateKey"] = json!("dependency-v1");
    let request = mcp
        .send_raw_request("thread/goal/wake", Some(unchanged))
        .await?;
    let response: serde_json::Value = mcp.read_response(request).await?;
    assert_eq!(json!(false), response["accepted"]);
    let mut stale_goal = wake.clone();
    stale_goal["goalId"] = json!("stale-goal");
    let request = mcp
        .send_raw_request("thread/goal/wake", Some(stale_goal))
        .await?;
    let response: serde_json::Value = mcp.read_response(request).await?;
    assert_eq!(json!(false), response["accepted"]);
    let mut stale_generation = wake.clone();
    stale_generation["observedGeneration"] = json!(999);
    let request = mcp
        .send_raw_request("thread/goal/wake", Some(stale_generation))
        .await?;
    let response: serde_json::Value = mcp.read_response(request).await?;
    assert_eq!(json!(false), response["accepted"]);
    let first = mcp
        .send_raw_request("thread/goal/wake", Some(wake.clone()))
        .await?;
    let duplicate = mcp.send_raw_request("thread/goal/wake", Some(wake)).await?;
    let accepted: serde_json::Value = mcp.read_response(first).await?;
    let ignored: serde_json::Value = mcp.read_response(duplicate).await?;
    assert_eq!(json!(true), accepted["accepted"]);
    assert_eq!(json!(false), ignored["accepted"]);
    let _: serde_json::Value = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/started"),
    )
    .await??;
    let completed: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(TurnStatus::Completed, completed.turn.status);
    assert_eq!(None, completed.turn.error);
    server.verify().await;
    Ok(())
}
