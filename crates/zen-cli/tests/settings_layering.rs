




mod common;

use common::*;


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_model_flag_overrides_settings() {
    let home = TempDir::new("cli_model_home");
    let cwd = TempDir::new("cli_model_cwd");


    home.write(".zen/settings.json", r#"{"model": "user-model"}"#);

    cwd.write(".zen/settings.json", r#"{"model": "project-model"}"#);

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("cli_model", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare",
                "--setting-sources", "user,project",
            ])
            .mock("cli-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let req_json_0 = request_json(&requests[0]);
    let req_model = req_json_0["model"].as_str().unwrap();
    assert_eq!(req_model, "cli-model", "CLI --model should win");
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_settings_overrides_file_settings() {
    let home = TempDir::new("inline_home");
    let cwd = TempDir::new("inline_cwd");

    cwd.write(".zen/settings.json", r#"{"model": "project-model"}"#);

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("inline", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare",
                "--setting-sources", "project",
                "--settings", r#"{"model":"inline-model"}"#
            ])
            .mock("inline-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let req_json_0 = request_json(&requests[0]);
    let req_model = req_json_0["model"].as_str().unwrap();
    assert_eq!(req_model, "inline-model", "--settings JSON should win over project file");
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_overrides_user() {
    let home = TempDir::new("project_user_home");
    let cwd = TempDir::new("project_user_cwd");

    home.write(".zen/settings.json", r#"{"model": "user-model"}"#);
    cwd.write(".zen/settings.json", r#"{"model": "project-model"}"#);

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("project_user", &home, &cwd)
            .args([
                "-p", "--output-format", "json",
                "--setting-sources", "user,project"
            ])
            .mock("project-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let req_json_0 = request_json(&requests[0]);
    let req_model = req_json_0["model"].as_str().unwrap();
    assert_eq!(req_model, "project-model", "project should override user");
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_overrides_project() {
    let home = TempDir::new("local_home");
    let cwd = TempDir::new("local_cwd");

    cwd.write(".zen/settings.json", r#"{"model": "project-model"}"#);
    cwd.write(".zen/settings.local.json", r#"{"model": "local-model"}"#);

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("local", &home, &cwd)
            .args([
                "-p", "--output-format", "json",
                "--setting-sources", "project,local"
            ])
            .mock("local-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let req_json_0 = request_json(&requests[0]);
    let req_model = req_json_0["model"].as_str().unwrap();
    assert_eq!(req_model, "local-model", "local should override project");
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_setting_sources_loads_no_files() {
    let home = TempDir::new("empty_sources_home");
    let cwd = TempDir::new("empty_sources_cwd");

    home.write(".zen/settings.json", r#"{"model": "user-model"}"#);
    cwd.write(".zen/settings.json", r#"{"model": "project-model"}"#);

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("empty_sources", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare",
                "--setting-sources", ""
            ])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let req_json_0 = request_json(&requests[0]);
    let req_model = req_json_0["model"].as_str().unwrap();

    assert!(req_model.contains("gemini") || req_model == "mock-model",
        "empty setting-sources should use default model, got: {}", req_model);
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn base_url_from_settings() {
    let home = TempDir::new("baseurl_home");
    let cwd = TempDir::new("baseurl_cwd");

    let server = MockServer::single_text("ok").await;

    cwd.write(
        ".zen/settings.json",
        &format!(r#"{{"baseUrl": "{}"}}"#, server.base_url()),
    );

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("baseurl", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare",
                "--setting-sources", "project",
                "--model", "test-model",
            ])
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);

    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(last_result(&stdout)["result"], "ok");
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_allow_merge_across_layers() {
    let home = TempDir::new("perm_merge_home");
    let cwd = TempDir::new("perm_merge_cwd");
    cwd.write("test.txt", "existing");


    cwd.write(".zen/settings.json", r#"{"permissions":{"allow":["Read"]}}"#);

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("write ok via CLI allow"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("perm_merge", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare",
                "--setting-sources", "project",
                "--allowedTools", "Write"
            ])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "write ok via CLI allow");
    let denials = v["permission_denials"].as_array().expect("permission_denials array");
    assert!(denials.is_empty(), "CLI --allowedTools should add to project allow");
}
