mod common;

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zen_md_project_memory_in_system_prompt() {
    let home = TempDir::new("zen_md_home");
    let cwd = TempDir::new("zen_md_cwd");

    cwd.write("ZEN.md", "PROJECT MEMORY: Always use tabs for indentation.\n- Rule 1\n- Rule 2");

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("zen_md", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("what are the rules?")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let sys_msg = system_message(&requests[0]);
    assert!(sys_msg.contains("PROJECT MEMORY"), "ZEN.md content missing from system prompt: {}", sys_msg);
    assert!(sys_msg.contains("Always use tabs"), "ZEN.md detail missing: {}", sys_msg);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zen_md_user_and_project_both_loaded() {
    let home = TempDir::new("zen_user_home");
    let cwd = TempDir::new("zen_user_cwd");

    home.write(".zen/ZEN.md", "USER MEMORY: Use spaces.");
    cwd.write("ZEN.md", "PROJECT MEMORY: Use tabs.");

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("zen_user_project", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "user,project"])
            .mock("mock-model", &server)
            .arg("what style?")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let sys_msg = system_message(&requests[0]);

    assert!(sys_msg.contains("USER MEMORY"), "user ZEN.md missing: {}", sys_msg);
    assert!(sys_msg.contains("PROJECT MEMORY"), "project ZEN.md missing: {}", sys_msg);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_excludes_memory_and_skills() {
    let home = TempDir::new("bare_home");
    let cwd = TempDir::new("bare_cwd");

    cwd.write("ZEN.md", "BARE SHOULD NOT SEE THIS");

    cwd.write(".zen/skills/testskill/SKILL.md", "---\nname: testskill\ndescription: Test skill\n---\nSkill body content");

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("bare", &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare", "--setting-sources", ""])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let sys_msg = system_message(&requests[0]);
    assert!(!sys_msg.contains("BARE SHOULD NOT SEE THIS"), "bare should exclude ZEN.md: {}", sys_msg);
    assert!(!sys_msg.contains("testskill"), "bare should exclude skills: {}", sys_msg);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_slash_command_expands_arguments() {
    let home = TempDir::new("slash_home");
    let cwd = TempDir::new("slash_cwd");

    cwd.write(".zen/commands/hello.md", "You said: $ARGUMENTS");

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("slash", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("/hello world from test")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let msgs_text = request_messages_text(&requests[0]);
    assert!(msgs_text.contains("You said: world from test"), "slash command not expanded: {}", msgs_text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_slash_command_positional_args() {
    let home = TempDir::new("slash_pos_home");
    let cwd = TempDir::new("slash_pos_cwd");

    cwd.write(".zen/commands/greet.md", "Hello $0 and $1!");

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("slash_pos", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("/greet Alice Bob")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let msgs_text = request_messages_text(&requests[0]);
    assert!(msgs_text.contains("Hello Alice and Bob!"), "positional args not expanded: {}", msgs_text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_custom_command_overrides_user() {
    let home = TempDir::new("slash_override_home");
    let cwd = TempDir::new("slash_override_cwd");

    home.write(".zen/commands/hello.md", "USER: $ARGUMENTS");
    cwd.write(".zen/commands/hello.md", "PROJECT: $ARGUMENTS");

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("slash_override", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "user,project"])
            .mock("mock-model", &server)
            .arg("/hello test")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let msgs_text = request_messages_text(&requests[0]);
    assert!(msgs_text.contains("PROJECT: test"), "project command should override: {}", msgs_text);
    assert!(!msgs_text.contains("USER: test"), "user command should not appear: {}", msgs_text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skills_listed_and_tool_works() {
    let home = TempDir::new("skills_home");
    let cwd = TempDir::new("skills_cwd");

    cwd.write(".zen/skills/myskill/SKILL.md", "---\nname: myskill\ndescription: My custom skill\n---\n# Skill Instructions\nAlways say 'SKILL ACTIVE' when invoked.");

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Skill", "arguments": "{\"name\":\"myskill\"}"}
            }])),
        ),
        Reply::text("skill loaded"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("skills", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("use the skill")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "skill loaded");

    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let second_req = request_messages_text(&requests[1]);
    assert!(second_req.contains("SKILL ACTIVE"), "skill body not returned: {}", second_req);
    assert!(second_req.contains("Skill directory:"), "skill dir path missing: {}", second_req);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_tool_unknown_returns_error() {
    let home = TempDir::new("skill_unknown_home");
    let cwd = TempDir::new("skill_unknown_cwd");

    cwd.write(".zen/skills/known/SKILL.md", "---\nname: known\ndescription: Known skill\n---\nKnown skill body");

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Skill", "arguments": "{\"name\":\"unknown\"}"}
            }])),
        ),
        Reply::text("handled error"),
    ]).await;

    let (_stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("skill_unknown", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("use unknown skill")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let second_req = request_messages_text(&requests[1]);
    assert!(second_req.contains("Unknown skill"), "should error for unknown skill: {}", second_req);
    assert!(second_req.contains("Available: known"), "should list available skills: {}", second_req);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_skills_user_and_project() {
    let home = TempDir::new("multi_skill_home");
    let cwd = TempDir::new("multi_skill_cwd");

    home.write(".zen/skills/shared/SKILL.md", "---\nname: shared\ndescription: User shared\n---\nUser version");
    cwd.write(".zen/skills/shared/SKILL.md", "---\nname: shared\ndescription: Project shared\n---\nProject version");
    cwd.write(".zen/skills/project_only/SKILL.md", "---\nname: project_only\ndescription: Project only\n---\nProject only body");

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("multi_skill", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "user,project"])
            .mock("mock-model", &server)
            .arg("list skills")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let sys_msg = system_message(&requests[0]);

    assert!(sys_msg.contains("Project version") || sys_msg.contains("Project shared"), "project skill should override: {}", sys_msg);
    assert!(sys_msg.contains("project_only"), "project-only skill should appear: {}", sys_msg);
    assert!(!sys_msg.contains("User version"), "user version should be overridden: {}", sys_msg);
}
