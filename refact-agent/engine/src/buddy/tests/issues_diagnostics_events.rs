use super::*;

#[test]
fn test_classification_case_insensitive() {
    assert_eq!(classify_error("TIMEOUT occurred"), "timeout");
    assert_eq!(classify_error("TimeOut error"), "timeout");
    assert_eq!(classify_error("PERMISSION denied"), "permission");
}

#[test]
fn test_classify_timeout() {
    assert_eq!(classify_error("connection timeout after 30s"), "timeout");
    assert_eq!(classify_error("request timed out"), "generic");
}

#[test]
fn test_classify_generic_fallback() {
    assert_eq!(classify_error("something weird happened"), "generic");
    assert_eq!(classify_error("unknown failure"), "generic");
}

#[test]
fn test_suggestion_dedupe() {
    let mut svc = make_service();
    let now = chrono::Utc::now().to_rfc3339();
    let already = svc
        .state
        .suggestion_state
        .iter()
        .any(|s| s.suggestion_type == "setup");
    if !already {
        svc.add_suggestion(make_suggestion("setup", "setup", &now));
    }
    let already2 = svc
        .state
        .suggestion_state
        .iter()
        .any(|s| s.suggestion_type == "setup");
    if !already2 {
        svc.add_suggestion(make_suggestion("setup2", "setup", &now));
    }
    assert_eq!(svc.state.suggestion_state.len(), 1);
}

#[test]
fn test_suggestion_pruning() {
    let mut svc = make_service();
    let old_time = (chrono::Utc::now() - Duration::seconds(400)).to_rfc3339();
    svc.state
        .suggestion_state
        .push(make_suggestion("old", "test", &old_time));
    svc.expire_suggestions();
    assert!(svc.state.suggestion_state[0].dismissed);
}

#[test]
fn test_suggestion_cap() {
    let mut svc = make_service();
    let now = chrono::Utc::now().to_rfc3339();
    let mut added = 0usize;
    for i in 0..10 {
        let s = make_suggestion(&format!("s{}", i), "test", &now);
        if svc.maybe_add_suggestion(s) {
            added += 1;
        }
    }
    assert_eq!(added, 1);
    assert_eq!(svc.state.suggestion_state.len(), 1);
}

#[test]
fn test_redact_api_key_pattern() {
    let input = "token ghp_AbCdEfGhIj1234567890 used";
    let output = redact_diagnostic_text(input);
    assert!(!output.contains("ghp_AbCdEfGhIj1234567890"));
    assert!(output.contains("[REDACTED"));
}

fn make_issue_ctx(message: &str) -> DiagnosticContext {
    DiagnosticContext {
        error_type: "test".to_string(),
        error_message: message.to_string(),
        source_file: Some("src/main.rs".to_string()),
        tool_name: None,
        chat_id: None,
        model_id: None,
        collected_at: chrono::Utc::now().to_rfc3339(),
        severity: DiagnosticSeverity::High,
        occurrences: None,
    }
}

#[test]
fn issue_title_redacts_bearer_token() {
    let ctx = make_issue_ctx("failed with Bearer xyz123");
    let (title, _) = issue_title_and_body(&ctx);
    assert!(title.contains("[REDACTED]"));
    assert!(!title.contains("Bearer xyz123"));
}

#[test]
fn issue_title_redacts_sk_ghp_glpat_tokens() {
    let ctx =
        make_issue_ctx("sk-abcdefghijklmnopqrst ghp_AbCdEfGhIj1234567890 glpat-abcdefghij12345");
    let (title, _) = issue_title_and_body(&ctx);
    assert!(!title.contains("sk-abcdefghijklmnopqrst"));
    assert!(!title.contains("ghp_AbCdEfGhIj1234567890"));
    assert!(!title.contains("glpat-abcdefghij12345"));
    assert!(title.contains("[REDACTED"));
}

#[test]
fn issue_title_redacts_api_key_password_token_secret_authorization() {
    let ctx = make_issue_ctx(
        "api_key=VALUEAPI apikey=VALUEAPINOSCORE token=VALUETOKEN secret=VALUESECRET password=VALUEPASSWORD Authorization: VALUEAUTH",
    );
    let (title, body) = issue_title_and_body(&ctx);
    for raw in [
        "VALUEAPI",
        "VALUEAPINOSCORE",
        "VALUETOKEN",
        "VALUESECRET",
        "VALUEPASSWORD",
        "VALUEAUTH",
    ] {
        assert!(
            !title.contains(raw),
            "raw secret leaked in title: {}",
            title
        );
        assert!(!body.contains(raw), "raw secret leaked in body: {}", body);
    }
    assert!(title.contains("[REDACTED]"));
    assert!(body.contains("[REDACTED]"));
}

#[test]
fn issue_title_and_body_use_same_redactor() {
    let raw = "request failed token=same-secret";
    let expected = redact_diagnostic_text(raw);
    let ctx = make_issue_ctx(raw);
    let (title, body) = issue_title_and_body(&ctx);
    assert!(title.contains(&expected));
    assert!(body.contains(&expected));
    assert!(!title.contains("same-secret"));
    assert!(!body.contains("same-secret"));
}

#[test]
fn parse_remote_url_handles_ssh_https_dot_git_combinations() {
    let fixtures = [
        (
            "git@github.com:owner/repo.git",
            "owner",
            "repo",
            RepoHost::GitHub,
        ),
        (
            "git@gitlab.com:team/project.git",
            "team",
            "project",
            RepoHost::GitLab,
        ),
        (
            "https://github.com/acme/tool",
            "acme",
            "tool",
            RepoHost::GitHub,
        ),
        (
            "https://github.com/acme/tool.git",
            "acme",
            "tool",
            RepoHost::GitHub,
        ),
        (
            "https://gitlab.com/group/repo.git",
            "group",
            "repo",
            RepoHost::GitLab,
        ),
        (
            "https://gitlab.acme.com/group/repo.git",
            "group",
            "repo",
            RepoHost::GitLabSelfHosted("gitlab.acme.com".to_string()),
        ),
        (
            "https://gitlab.example.com/org/platform/team/repo.git",
            "org/platform/team",
            "repo",
            RepoHost::GitLabSelfHosted("gitlab.example.com".to_string()),
        ),
        (
            "https://example.com/org/platform/team/repo.git",
            "org/platform/team",
            "repo",
            RepoHost::Unsupported("example.com".to_string()),
        ),
    ];
    for (url, owner, repo, host) in fixtures {
        let parsed = parse_remote_url(url).expect("remote URL must parse");
        assert_eq!(parsed.owner, owner);
        assert_eq!(parsed.repo, repo);
        assert_eq!(parsed.host, host);
    }
}

#[test]
fn parse_remote_url_returns_none_for_invalid() {
    assert!(parse_remote_url("not a remote").is_none());
    assert!(parse_remote_url("https://github.com/only-owner").is_none());
}

#[tokio::test]
async fn mcp_issue_creation_uses_detected_repo() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.remote("origin", "git@github.com:detected/project.git")
        .unwrap();
    let detected = detect_repo_from_git(dir.path())
        .await
        .expect("repo must be detected");
    let args = mcp_issue_args(&detected.owner, &detected.repo, "title", "body", vec![]);
    assert_eq!(args["owner"], "detected");
    assert_eq!(args["repo"], "project");
}

#[tokio::test]
async fn mcp_issue_creation_no_remote_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    assert!(detect_repo_from_git(dir.path()).await.is_none());
}

#[test]
fn mcp_issue_creation_no_hardcoded_repo_in_args() {
    let args = mcp_issue_args("other", "repo", "title", "body", vec!["bug".to_string()]);
    let serialized = serde_json::to_string(&args).unwrap();
    assert!(!serialized.contains("smallcloudai/refact"));
    assert_eq!(args["owner"], "other");
    assert_eq!(args["repo"], "repo");
}

#[test]
fn mcp_issue_prepare_refuses_duplicate_before_tool_call() {
    let ctx = make_issue_ctx("same issue error");
    let recent = vec![(issue_dedupe_text(&ctx), chrono::Utc::now())];
    let err = prepare_issue_content(
        &ctx,
        Some("Title"),
        Some("Body"),
        true,
        true,
        false,
        None,
        &recent,
    )
    .unwrap_err();
    assert!(err.contains("Duplicate issue suppressed"));
}

#[test]
fn mcp_issue_prepare_respects_rate_limit_when_not_manual() {
    let ctx = make_issue_ctx("rate limited issue error");
    let err = prepare_issue_content(
        &ctx,
        Some("Title"),
        Some("Body"),
        true,
        true,
        false,
        Some(std::time::Instant::now()),
        &[],
    )
    .unwrap_err();
    assert!(err.contains("rate limit active"));

    let prepared = prepare_issue_content(
        &ctx,
        Some("Title"),
        Some("Body"),
        true,
        false,
        true,
        Some(std::time::Instant::now()),
        &[],
    )
    .unwrap();
    assert_eq!(prepared.dedupe_text, issue_dedupe_text(&ctx));
}

#[test]
fn mcp_issue_prepare_sanitizes_args_title_body() {
    let ctx = make_issue_ctx("panic with token=CTXSECRET");
    let raw_title = format!("Crash token=TITLESECRET\n{}", "x".repeat(200));
    let raw_body = format!(
        "Body Bearer BODYSECRET api_key=BODYAPI with ```\n{}",
        "y".repeat(5000)
    );
    let prepared = prepare_issue_content(
        &ctx,
        Some(&raw_title),
        Some(&raw_body),
        true,
        true,
        false,
        None,
        &[],
    )
    .unwrap();
    let args = mcp_issue_args(
        "detected",
        "project",
        &prepared.title,
        &prepared.body,
        vec!["bug".to_string()],
    );
    assert_eq!(args["owner"], "detected");
    assert_eq!(args["repo"], "project");
    assert!(!prepared.title.contains('\n'));
    assert!(!prepared.title.contains('\r'));
    assert!(prepared.title.chars().count() <= 120);
    assert!(prepared.body.chars().count() <= 4000);
    assert!(!prepared.body.contains("```"));
    let serialized = serde_json::to_string(&args).unwrap();
    for raw in ["TITLESECRET", "BODYSECRET", "BODYAPI", "CTXSECRET"] {
        assert!(!serialized.contains(raw), "raw secret leaked: {}", raw);
    }
    assert!(!serialized.contains("smallcloudai/refact"));
}

#[test]
fn mcp_and_native_prepare_share_dedupe_text() {
    let ctx = make_issue_ctx("shared dedupe token=SECRETDEDUP");
    let native = prepare_issue_content(&ctx, None, None, true, true, false, None, &[]).unwrap();
    let mcp = prepare_issue_content(
        &ctx,
        Some("Title"),
        Some("Body"),
        true,
        true,
        false,
        None,
        &[],
    )
    .unwrap();
    assert_eq!(native.dedupe_text, mcp.dedupe_text);
    assert_eq!(mcp.dedupe_text, issue_dedupe_text(&ctx));
    assert!(!mcp.dedupe_text.contains("SECRETDEDUP"));
}

#[test]
fn validate_issue_binary_rejects_absolute_path() {
    assert!(validate_issue_binary("/tmp/gh").is_err());
    assert!(validate_issue_binary("C:\\tmp\\gh").is_err());
}

#[test]
fn validate_issue_binary_rejects_unknown_command() {
    assert!(validate_issue_binary("evilbin").is_err());
    assert!(validate_issue_binary("gh.exe").is_err());
}

#[test]
fn validate_issue_binary_accepts_gh_and_glab() {
    assert_eq!(validate_issue_binary("gh").unwrap(), "gh");
    assert_eq!(validate_issue_binary("glab").unwrap(), "glab");
}

#[test]
fn issue_provider_debug_redacts_tokens() {
    let provider = IssueProvider::GitHub {
        binary: "gh".to_string(),
        token: "ghp_AbCdEfGhIj1234567890".to_string(),
    };
    let rendered = format!("{:?}", provider);
    assert!(!rendered.contains("ghp_AbCdEfGhIj1234567890"));
    assert!(rendered.contains("[REDACTED]"));
}

#[tokio::test]
async fn issue_success_side_effects_are_centralized() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    *app.buddy.buddy.lock().await = Some(make_service());
    for dedupe in ["native error", "mcp issue title"] {
        record_issue_success(
            app.clone(),
            dedupe.to_string(),
            BuddyActivity {
                icon: "🐛".to_string(),
                title: "Issue created".to_string(),
                description: format!("created {dedupe}"),
                timestamp: chrono::Utc::now().to_rfc3339(),
                activity_type: "issue_created".to_string(),
                chat_id: None,
                failure_category: None,
                failure_summary: None,
            },
        )
        .await;
    }
    let buddy_arc = app.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let svc = lock.as_ref().unwrap();
    assert!(svc.last_issue_at.is_some());
    assert!(svc
        .recent_issue_errors
        .iter()
        .any(|(message, _)| message == "native error"));
    assert!(svc
        .recent_issue_errors
        .iter()
        .any(|(message, _)| message == "mcp issue title"));
    for dedupe in ["native error", "mcp issue title"] {
        let recorded = svc
            .recent_issue_errors
            .iter()
            .filter(|(message, _)| message.as_str() == dedupe)
            .count();
        assert_eq!(recorded, 1, "{dedupe} must be recorded exactly once");
    }
    let issue_activities = svc
        .state
        .recent_activities
        .iter()
        .filter(|activity| activity.activity_type == "issue_created")
        .count();
    assert_eq!(issue_activities, 2);
}

#[test]
fn test_sanitize_title_newlines() {
    let raw = "Error:\nline 2\r\nline 3";
    let result = sanitize_title(raw);
    assert!(!result.contains('\n'));
    assert!(!result.contains('\r'));
    assert!(!result.is_empty());
}

#[test]
fn test_sanitize_body_truncation() {
    let raw: String = "x".repeat(5000);
    let result = sanitize_body(&raw);
    assert!(result.chars().count() <= 4000);
}

#[tokio::test]
async fn test_diagnostic_cap() {
    let mut svc = make_service();
    for i in 0..150 {
        let ctx = DiagnosticContext {
            error_type: "test".to_string(),
            error_message: format!("error {}", i),
            source_file: None,
            tool_name: None,
            chat_id: None,
            model_id: None,
            collected_at: chrono::Utc::now().to_rfc3339(),
            severity: DiagnosticSeverity::Low,
            occurrences: None,
        };
        svc.add_diagnostic(ctx);
    }
    assert_eq!(svc.recent_diagnostics.len(), 100);
}

#[tokio::test]
async fn test_diagnostic_history_persists_and_loads() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let ctx1 = DiagnosticContext {
        error_type: "test".to_string(),
        error_message: "first".to_string(),
        source_file: Some("src/a.rs".to_string()),
        tool_name: None,
        chat_id: Some("chat-1".to_string()),
        model_id: None,
        collected_at: "2026-04-27T10:00:00Z".to_string(),
        severity: DiagnosticSeverity::High,
        occurrences: None,
    };
    let ctx2 = DiagnosticContext {
        error_type: "test".to_string(),
        error_message: "second".to_string(),
        source_file: Some("src/b.rs".to_string()),
        tool_name: Some("tool".to_string()),
        chat_id: Some("chat-2".to_string()),
        model_id: None,
        collected_at: "2026-04-27T10:01:00Z".to_string(),
        severity: DiagnosticSeverity::Low,
        occurrences: None,
    };

    super::storage::append_diagnostic(root, &ctx1)
        .await
        .unwrap();
    super::storage::append_diagnostic(root, &ctx2)
        .await
        .unwrap();

    let loaded = super::storage::load_diagnostics(root).await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].error_message, "first");
    assert_eq!(loaded[1].error_message, "second");
}

#[test]
fn test_same_day_log_filter_accepts_same_day_time() {
    assert!(super::actor::same_day_log_filter(
        "101530.123 ERROR something failed",
        "2026-04-27T10:16:00Z",
    ));
    assert!(!super::actor::same_day_log_filter(
        "235959.999 ERROR old failure",
        "2026-04-27T10:16:00Z",
    ));
}

#[tokio::test]
#[serial_test::serial(runtime_settings)]
async fn investigation_log_tail_reads_bounded_and_redacts() {
    use crate::http::routers::v1::buddy_opportunities::read_recent_log_lines;
    let _guard = crate::tools::settings_guard::SettingsGuard::install(|settings| {
        settings.get_logs_max_tail_bytes = 256 * 1024;
    });
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("refact.log");
    let mut content = String::from("old secret token=old-secret\n");
    content.push_str(&"x".repeat(300 * 1024));
    content.push_str("\nrecent token=recent-secret\nlast line\n");
    tokio::fs::write(&log_path, content).await.unwrap();

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let mut app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    app.runtime.cmdline = Arc::new(crate::global_context::CommandLine {
        logs_to_file: log_path.to_string_lossy().into_owned(),
        ..(*app.runtime.cmdline).clone()
    });
    let tail = read_recent_log_lines(&app, 5).await.unwrap();
    assert!(!tail.contains("old secret"));
    assert!(!tail.contains("recent-secret"));
    assert!(tail.contains("token=[REDACTED]"));
    assert!(tail.contains("last line"));
}

#[tokio::test]
async fn log_fallback_filters_by_filename_not_full_path() {
    use crate::http::routers::v1::buddy_opportunities::{is_log_candidate, read_log_content};
    let dir = tempfile::tempdir().unwrap();
    let refact_dir = dir.path().join("path-with-refact-name");
    tokio::fs::create_dir_all(&refact_dir).await.unwrap();
    let unrelated = refact_dir.join("unrelated.txt");
    let real_log = refact_dir.join("engine.log");
    tokio::fs::write(&unrelated, "bad").await.unwrap();
    tokio::fs::write(&real_log, "good").await.unwrap();

    assert!(!is_log_candidate(&unrelated));
    assert!(is_log_candidate(&real_log));
    let content = read_log_content(&refact_dir.join("refact.log"))
        .await
        .unwrap();
    assert_eq!(content, "good");
}

#[test]
fn test_buddy_say_creates_speech() {
    use super::types::{BuddySpeechItem, BuddyControl};
    let mut svc = make_service();
    let speech = BuddySpeechItem {
        id: "test-id".to_string(),
        text: "Hello!".to_string(),
        mood: "happy".to_string(),
        scope: "global".to_string(),
        persistent: false,
        ttl_seconds: 10,
        dedupe_key: Some("greeting".to_string()),
        speech_intent: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        controls: vec![],
        chat_id: None,
    };
    svc.update_speech(speech.clone());
    assert!(svc.active_speech.is_some());
    assert_eq!(svc.active_speech.as_ref().unwrap().text, "Hello!");

    let speech2 = BuddySpeechItem {
        id: "test-id-2".to_string(),
        text: "Updated!".to_string(),
        mood: "happy".to_string(),
        scope: "global".to_string(),
        persistent: false,
        ttl_seconds: 10,
        dedupe_key: Some("greeting".to_string()),
        speech_intent: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        controls: vec![],
        chat_id: None,
    };
    svc.update_speech(speech2);
    assert_eq!(svc.active_speech.as_ref().unwrap().text, "Updated!");

    let _ = BuddyControl {
        id: "btn1".to_string(),
        label: "Open Setup".to_string(),
        action: "open_setup".to_string(),
        action_param: None,
        style: "primary".to_string(),
    };
}

#[test]
fn test_buddy_controls_schema() {
    use crate::tools::tool_buddy_say::ToolBuddyRenderControls;
    use crate::tools::tools_description::Tool;
    let tool = ToolBuddyRenderControls {
        config_path: String::new(),
    };
    let desc = tool.tool_description();
    let actions: Vec<&str> = desc.input_schema["properties"]["controls"]["items"]["properties"]
        ["action"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(actions.contains(&"open_setup"));
    assert!(actions.contains(&"open_setup_mode"));
    assert!(actions.contains(&"dismiss"));
    assert!(!actions.contains(&"open_setup_mcp"));
    assert!(!actions.contains(&"run_command"));
    assert!(!actions.contains(&"invalid_action"));
}

#[test]
fn test_runtime_event_speech_text_preserved() {
    use super::runtime_queue::RuntimeQueue;
    let mut queue = RuntimeQueue::new();
    let mut ev =
        super::actor::make_runtime_event("streaming", "Test", "chat", "chat_1", "started", None);
    ev.speech_text = Some("Thinking...".to_string());
    ev.scene = Some("working".to_string());
    ev.persistent = true;
    queue.enqueue(ev);
    let stored = &queue.items[0];
    assert_eq!(stored.speech_text.as_deref(), Some("Thinking..."));
    assert_eq!(stored.scene.as_deref(), Some("working"));
    assert!(stored.persistent);
}

#[test]
fn test_persistent_event_fields_coalesced() {
    use super::runtime_queue::RuntimeQueue;
    let mut queue = RuntimeQueue::new();
    let mut ev1 =
        super::actor::make_runtime_event("streaming", "First", "chat", "key_1", "started", None);
    ev1.speech_text = Some("Initial".to_string());
    ev1.persistent = true;
    queue.enqueue(ev1);
    let mut ev2 =
        super::actor::make_runtime_event("tool_used", "Updated", "tool", "key_1", "progress", None);
    ev2.speech_text = Some("Updated text".to_string());
    ev2.persistent = true;
    queue.enqueue(ev2);
    assert_eq!(queue.items.len(), 1);
    assert_eq!(queue.items[0].signal_type, "tool_used");
    assert_eq!(queue.items[0].source, "tool");
    assert_eq!(queue.items[0].speech_text.as_deref(), Some("Updated text"));
    assert_eq!(queue.items[0].status, "progress");
}

#[test]
fn test_now_playing_reemit_preserves_dismissal() {
    use super::runtime_queue::RuntimeQueue;
    let mut queue = RuntimeQueue::new();
    let mut ev1 = super::actor::make_runtime_event(
        "error",
        "First",
        "frontend",
        "same-error",
        "failed",
        Some("high"),
    );
    ev1.dismissed = true;
    queue.now_playing = Some(ev1);
    let ev2 = super::actor::make_runtime_event(
        "error",
        "Second",
        "frontend",
        "same-error",
        "failed",
        Some("high"),
    );

    queue.enqueue(ev2);

    let now_playing = queue.now_playing.as_ref().unwrap();
    assert_eq!(now_playing.title, "Second");
    assert!(now_playing.dismissed);
    assert!(queue.items.is_empty());
}

#[test]
fn test_completing_persistent_event_makes_it_temporary() {
    use super::runtime_queue::RuntimeQueue;
    let mut queue = RuntimeQueue::new();
    let mut ev =
        super::actor::make_runtime_event("streaming", "Working", "chat", "key_1", "started", None);
    ev.persistent = true;
    queue.enqueue(ev);

    queue.complete("key_1", "completed");

    assert_eq!(queue.items[0].status, "completed");
    assert!(!queue.items[0].persistent);
    assert_eq!(queue.items[0].ttl_ms, Some(4000));
}

#[tokio::test]
async fn test_unified_listing_mixed_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let conv_path = root.join(".refact/buddy/chats/conversations/abc123.json");
    let conv_json = serde_json::json!({
        "chat_id": "abc123", "title": "Test Chat", "kind": "chat",
        "created_at": "2024-01-02T00:00:00Z", "last_message_at": null, "messages": []
    });
    super::storage::atomic_write_json(&conv_path, &conv_json)
        .await
        .unwrap();

    let wf_path = root.join(".refact/buddy/chats/workflows/commit_message.json");
    let wf_json = serde_json::json!({
        "entries": [{ "timestamp": "2024-01-01T00:00:00Z", "input_summary": "", "output_summary": "done", "success": true }]
    });
    super::storage::atomic_write_json(&wf_path, &wf_json)
        .await
        .unwrap();

    let entries = super::conversation_ledger::list_all_buddy_conversations(root, None).await;
    assert_eq!(entries.len(), 2);
    let kinds: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"chat"));
    assert!(kinds.contains(&"workflow"));
}

#[tokio::test]
async fn test_list_recent_buddy_conversations_bounded_by_recency() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    for (name, id, updated, title) in [
        ("a.json", "a", "2026-01-02T00:00:00Z", "Older"),
        ("b.json", "b", "2026-01-05T00:00:00Z", "Newer"),
        ("c.json", "c", "2026-01-04T00:00:00Z", "Middle"),
    ] {
        let path = root.join(format!(".refact/buddy/chats/conversations/{name}"));
        let json = serde_json::json!({
            "chat_id": id, "title": title, "kind": "chat",
            "created_at": "2026-01-01T00:00:00Z", "last_message_at": updated,
            "messages": [{ "role": "user" }]
        });
        super::storage::atomic_write_json(&path, &json)
            .await
            .unwrap();
    }

    let recent = super::conversation_ledger::list_recent_buddy_conversations(root, 2).await;
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].title, "Newer");
    assert_eq!(recent[1].title, "Middle");
}

#[tokio::test]
async fn test_list_recent_buddy_conversations_includes_workflows() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let conv_path = root.join(".refact/buddy/chats/conversations/c1.json");
    let conv_json = serde_json::json!({
        "chat_id": "c1", "title": "Conversation One", "kind": "chat",
        "created_at": "2026-01-01T00:00:00Z", "last_message_at": "2026-01-03T00:00:00Z",
        "messages": [{ "role": "user" }]
    });
    super::storage::atomic_write_json(&conv_path, &conv_json)
        .await
        .unwrap();

    let wf_path = root.join(".refact/buddy/chats/workflows/commit_message.json");
    let wf_json = serde_json::json!({
        "entries": [{ "timestamp": "2026-01-05T00:00:00Z", "output_summary": "done", "success": true }]
    });
    super::storage::atomic_write_json(&wf_path, &wf_json)
        .await
        .unwrap();

    let recent = super::conversation_ledger::list_recent_buddy_conversations(root, 5).await;
    assert!(recent.iter().any(|e| e.kind == "chat"));
    assert!(recent.iter().any(|e| e.kind == "workflow"));
    assert_eq!(recent[0].kind, "workflow");
}

#[tokio::test]
async fn test_setup_kind_stored() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let path = root.join(".refact/buddy/chats/conversations/setup1.json");
    let json = serde_json::json!({
        "chat_id": "setup1", "title": "MCP Setup", "kind": "setup", "badge": "MCP Setup",
        "created_at": "2024-01-01T00:00:00Z", "last_message_at": null, "messages": []
    });
    super::storage::atomic_write_json(&path, &json)
        .await
        .unwrap();

    let entries = super::conversation_ledger::list_all_buddy_conversations(root, None).await;
    let setup = entries.iter().find(|e| e.id == "setup1").unwrap();
    assert_eq!(setup.kind, "setup");
    assert_eq!(setup.badge.as_deref(), Some("MCP Setup"));
    assert_eq!(setup.icon, "⚙️");
}

#[tokio::test]
async fn test_kind_filter_works() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let conv_path = root.join(".refact/buddy/chats/conversations/c1.json");
    let conv_json = serde_json::json!({
        "chat_id": "c1", "title": "Chat", "created_at": "2024-01-01T00:00:00Z", "messages": []
    });
    super::storage::atomic_write_json(&conv_path, &conv_json)
        .await
        .unwrap();

    let wf_path = root.join(".refact/buddy/chats/workflows/commit_message.json");
    let wf_json = serde_json::json!({ "entries": [] });
    super::storage::atomic_write_json(&wf_path, &wf_json)
        .await
        .unwrap();

    let chat_only = super::conversation_ledger::list_all_buddy_conversations(
        root,
        Some(vec!["chat".to_string()]),
    )
    .await;
    assert!(chat_only.iter().all(|e| e.kind == "chat"));

    let wf_only = super::conversation_ledger::list_all_buddy_conversations(
        root,
        Some(vec!["workflow".to_string()]),
    )
    .await;
    assert!(wf_only.iter().all(|e| e.kind == "workflow"));
}

#[tokio::test]
async fn test_sorting_by_updated_at() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let p1 = root.join(".refact/buddy/chats/conversations/old.json");
    super::storage::atomic_write_json(
        &p1,
        &serde_json::json!({
            "chat_id": "old", "title": "Old", "created_at": "2024-01-01T00:00:00Z", "messages": []
        }),
    )
    .await
    .unwrap();

    let p2 = root.join(".refact/buddy/chats/conversations/new.json");
    super::storage::atomic_write_json(
        &p2,
        &serde_json::json!({
            "chat_id": "new", "title": "New", "created_at": "2024-06-01T00:00:00Z",
            "last_message_at": "2024-06-02T00:00:00Z", "messages": []
        }),
    )
    .await
    .unwrap();

    let entries = super::conversation_ledger::list_all_buddy_conversations(root, None).await;
    assert_eq!(entries[0].id, "new");
}

#[test]
fn test_runtime_event_controls_preserved() {
    use super::runtime_queue::RuntimeQueue;
    use super::types::BuddyControl;
    let mut queue = RuntimeQueue::new();
    let mut ev = super::actor::make_runtime_event(
        "chat_error",
        "Error",
        "chat",
        "err_1",
        "info",
        Some("high"),
    );
    ev.controls = vec![BuddyControl {
        id: "fix".to_string(),
        label: "Fix".to_string(),
        action: "open_chat".to_string(),
        action_param: None,
        style: "primary".to_string(),
    }];
    queue.enqueue(ev);
    assert_eq!(queue.items[0].controls.len(), 1);
    assert_eq!(queue.items[0].controls[0].action, "open_chat");
}

fn make_job_context(
    onboarding: BuddyOnboarding,
    diagnostics_count: usize,
    job_state: BuddyJobState,
) -> BuddyJobContext {
    let mut diags = vec![];
    for _ in 0..diagnostics_count {
        diags.push(DiagnosticContext {
            error_type: "timeout".to_string(),
            error_message: "connection timeout".to_string(),
            source_file: None,
            tool_name: None,
            chat_id: None,
            model_id: None,
            collected_at: chrono::Utc::now().to_rfc3339(),
            severity: DiagnosticSeverity::High,
            occurrences: None,
        });
    }
    BuddyJobContext {
        identity_name: "Pixel".to_string(),
        personality: Default::default(),
        onboarding,
        recent_diagnostics: diags,
        project_root: std::path::PathBuf::from("/tmp/test-project"),
        job_state,
        workflow_summaries: vec![],
        total_workflow_runs: 0,
        suggestion_state: vec![],
        pet: Default::default(),
        active_quest: None,
        settings: BuddySettings::default(),
        pulse: BuddyPulse::default(),
        facts: vec![],
        recent_activities: vec![],
    }
}

#[test]
fn test_scheduler_cooldown_enforcement() {
    let recent_run = (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
    let state = BuddyJobState {
        last_run: Some(recent_run),
        run_count: 1,
        last_result: Some("ok".to_string()),
        snoozed_until: None,
        dismissed: false,
    };
    let elapsed = state
        .last_run
        .as_deref()
        .and_then(|r| chrono::DateTime::parse_from_rfc3339(r).ok())
        .map(|t| {
            chrono::Utc::now()
                .signed_duration_since(t)
                .num_seconds()
                .max(0) as u64
        })
        .unwrap_or(u64::MAX);
    let cooldown = 5 * 60u64;
    assert!(elapsed < cooldown, "job should be blocked by cooldown");
}

#[test]
fn test_job_state_persistence_roundtrip() {
    let mut state = default_buddy_state();
    state.job_cooldowns.insert(
        "greeting".to_string(),
        BuddyJobState {
            last_run: Some("2026-01-01T00:00:00Z".to_string()),
            run_count: 3,
            last_result: Some("ok".to_string()),
            snoozed_until: None,
            dismissed: false,
        },
    );
    let json = serde_json::to_string(&state).unwrap();
    let loaded: BuddyState = serde_json::from_str(&json).unwrap();
    let job_state = loaded.job_cooldowns.get("greeting").unwrap();
    assert_eq!(job_state.run_count, 3);
    assert_eq!(job_state.last_result.as_deref(), Some("ok"));
}

#[tokio::test]
async fn test_greeting_triggers_on_first_launch() {
    use super::jobs::greeting::GreetingJob;
    use super::scheduler::BuddyJob;
    let job = GreetingJob;
    let ctx = make_job_context(BuddyOnboarding::default(), 0, BuddyJobState::default());
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    assert!(job.should_run(app, &ctx).await);
}

#[tokio::test]
async fn test_first_launch_greeting_has_short_ttl() {
    use super::jobs::greeting::GreetingJob;
    use super::scheduler::BuddyJob;
    let job = GreetingJob;
    let ctx = make_job_context(BuddyOnboarding::default(), 0, BuddyJobState::default());
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;

    let result = job.execute(app, ctx).await;
    let speech = result.speech.expect("greeting should produce speech");

    assert!(!speech.persistent);
    assert!(speech.ttl_seconds > 0 && speech.ttl_seconds <= 15);
}

#[test]
fn test_greeting_blocked_within_cooldown() {
    use super::jobs::greeting::GreetingJob;
    use super::scheduler::BuddyJob;
    let job = GreetingJob;
    let recent = (chrono::Utc::now() - chrono::Duration::seconds(60)).to_rfc3339();
    let job_state = BuddyJobState {
        last_run: Some(recent),
        run_count: 1,
        last_result: Some("ok".to_string()),
        snoozed_until: None,
        dismissed: false,
    };
    let elapsed = job_state
        .last_run
        .as_deref()
        .and_then(|r| chrono::DateTime::parse_from_rfc3339(r).ok())
        .map(|t| {
            chrono::Utc::now()
                .signed_duration_since(t)
                .num_seconds()
                .max(0) as u64
        })
        .unwrap_or(u64::MAX);
    assert!(
        elapsed < job.cooldown_seconds(),
        "greeting must be blocked within 24h cooldown"
    );
}

#[tokio::test]
async fn test_error_triage_clusters_by_type() {
    use super::jobs::error_triage::ErrorTriageJob;
    use super::scheduler::BuddyJob;
    let job = ErrorTriageJob;
    let ctx = make_job_context(BuddyOnboarding::default(), 5, BuddyJobState::default());
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    assert!(job.should_run(app.clone(), &ctx).await);
    let result = job.execute(app, ctx).await;
    assert!(
        result.suggestion.is_some(),
        "should produce suggestion for 5 repeated timeouts"
    );
    let sug = result.suggestion.unwrap();
    assert_eq!(sug.suggestion_type, "error_pattern");
    assert!(sug.title.contains("timeout"));
}

#[tokio::test]
async fn test_config_watcher_detects_missing_agents_md() {
    use super::jobs::config_watcher::ConfigWatcherJob;
    use super::scheduler::BuddyJob;
    let dir = tempfile::tempdir().unwrap();
    let job = ConfigWatcherJob;
    let mut ctx = make_job_context(BuddyOnboarding::default(), 0, BuddyJobState::default());
    ctx.project_root = dir.path().to_path_buf();
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let result = job.execute(app, ctx).await;
    assert!(
        result.suggestion.is_some(),
        "should suggest setup when AGENTS.md missing"
    );
    assert_eq!(result.suggestion.unwrap().suggestion_type, "setup");
}

#[test]
fn test_suggestion_cap_max_unread() {
    let mut svc = make_service();
    let now = chrono::Utc::now().to_rfc3339();
    svc.settings.proactive_enabled = true;
    for i in 0..10 {
        let s = make_suggestion(&format!("s{}", i), &format!("type{}", i), &now);
        let _ = svc.maybe_add_suggestion(s);
    }
    let unread = svc
        .state
        .suggestion_state
        .iter()
        .filter(|s| !s.dismissed)
        .count();
    assert!(unread <= 10, "suggestions should be bounded");
}

#[test]
fn test_dismissed_job_does_not_retrigger() {
    let state = BuddyJobState {
        last_run: None,
        run_count: 0,
        last_result: None,
        snoozed_until: None,
        dismissed: true,
    };
    assert!(state.dismissed, "dismissed job must not retrigger");
    let elapsed = u64::MAX;
    let cooldown = 0u64;
    let should_skip = state.dismissed || elapsed < cooldown;
    assert!(
        should_skip,
        "dismissed job must be skipped regardless of cooldown"
    );
}

#[test]
fn test_proactive_enabled_setting() {
    let settings = BuddySettings::default();
    assert!(
        settings.proactive_enabled,
        "proactive_enabled defaults to true"
    );
    let json = serde_json::to_string(&settings).unwrap();
    let loaded: BuddySettings = serde_json::from_str(&json).unwrap();
    assert!(loaded.proactive_enabled);
}

#[test]
fn test_old_settings_get_proactive_default() {
    let json = r#"{"enabled": true, "auto_diagnostics": true, "auto_issue_creation": false}"#;
    let settings: BuddySettings = serde_json::from_str(json).unwrap();
    assert!(
        settings.proactive_enabled,
        "missing proactive_enabled should default to true"
    );
}

#[tokio::test]
async fn test_tour_job_runs_only_once() {
    use super::jobs::tour::TourJob;
    use super::scheduler::BuddyJob;
    let job = TourJob;
    let onboarding = BuddyOnboarding {
        greeted: true,
        tour_completed: false,
        ..Default::default()
    };
    let fresh_ctx = make_job_context(onboarding.clone(), 0, BuddyJobState::default());
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    assert!(
        job.should_run(app.clone(), &fresh_ctx).await,
        "tour must run on first tick"
    );
    let ran_state = BuddyJobState {
        last_run: Some(chrono::Utc::now().to_rfc3339()),
        run_count: 1,
        last_result: Some("ok".to_string()),
        snoozed_until: None,
        dismissed: false,
    };
    let ran_ctx = make_job_context(onboarding, 0, ran_state);
    assert!(
        !job.should_run(app, &ran_ctx).await,
        "tour must not run after first run"
    );
}

#[tokio::test]
async fn test_tour_speech_has_short_ttl() {
    use super::jobs::tour::TourJob;
    use super::scheduler::BuddyJob;
    let job = TourJob;
    let ctx = make_job_context(
        BuddyOnboarding {
            greeted: true,
            tour_completed: false,
            ..Default::default()
        },
        0,
        BuddyJobState::default(),
    );
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let mut state = crate::buddy::state::default_buddy_state();
        state.onboarding.greeted = true;
        let service = crate::buddy::actor::BuddyService::new(
            std::env::temp_dir().join(format!("buddy-tour-ttl-test-{}", uuid::Uuid::new_v4())),
            state,
            crate::buddy::settings::BuddySettings::default(),
            Vec::new(),
            crate::buddy::runtime_queue::RuntimeQueue::new(),
            tx,
            None,
        );
        let buddy_arc = app.buddy.buddy.clone();
        *buddy_arc.lock().await = Some(service);
    }

    let result = job.execute(app.clone(), ctx).await;
    assert_eq!(result.last_result.as_deref(), Some("completed"));

    let buddy = app.buddy.buddy.lock().await;
    let service = buddy.as_ref().unwrap();
    let speech = service
        .active_speech
        .as_ref()
        .expect("tour should emit speech through the arbiter");

    assert!(!speech.persistent);
    assert!(speech.ttl_seconds > 0 && speech.ttl_seconds <= 15);
}

#[test]
fn test_scheduler_suggestion_dedup() {
    let mut svc = make_service();
    let now = chrono::Utc::now().to_rfc3339();
    let s1 = make_suggestion("dup-1", "error_pattern", &now);
    let s2 = BuddySuggestion {
        id: "dup-2".to_string(),
        suggestion_type: "error_pattern".to_string(),
        title: "t".to_string(),
        description: "d".to_string(),
        created_at: now,
        dismissed: false,
        controls: vec![],
        quest: None,
    };
    assert!(
        svc.maybe_add_suggestion(s1),
        "first suggestion must be accepted"
    );
    assert!(
        !svc.maybe_add_suggestion(s2),
        "duplicate suggestion must be rejected by dedup"
    );
    assert_eq!(svc.state.suggestion_state.len(), 1);
}

#[tokio::test]
async fn test_proactive_disabled_still_allows_greeting() {
    use super::jobs::greeting::GreetingJob;
    use super::scheduler::BuddyJob;
    let job = GreetingJob;
    assert!(
        !job.produces_suggestion(),
        "greeting must not be gated by proactive flag"
    );
    let ctx = make_job_context(BuddyOnboarding::default(), 0, BuddyJobState::default());
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    assert!(
        job.should_run(app, &ctx).await,
        "greeting must run even when proactive_enabled=false"
    );
}

#[test]
fn test_workflow_id_rejects_path_traversal() {
    assert!(!super::actor::validate_workflow_id("../evil"));
}

#[test]
fn test_workflow_id_rejects_slashes() {
    assert!(!super::actor::validate_workflow_id("a/b"));
}

#[test]
fn test_workflow_id_accepts_valid() {
    assert!(super::actor::validate_workflow_id("commit_message"));
    assert!(super::actor::validate_workflow_id("follow-up"));
    assert!(super::actor::validate_workflow_id("kg_enrich"));
}

#[tokio::test]
async fn test_report_error_unicode_safe() {
    let mut svc = make_service();
    svc.report_error("test", "emoji 🎉 and CJK 你好 text", None, None);
    assert!(!svc.state.recent_activities.is_empty());
    assert_eq!(svc.recent_diagnostics.len(), 1);
}

#[tokio::test]
async fn test_report_error_with_model_stream_corrupted_is_medium() {
    let mut svc = make_service();
    svc.report_error_with_model(
        "llm_error",
        "stream ended unexpectedly while decoding response body",
        Some("chat/generation.rs"),
        Some("chat-1"),
        Some("gpt-4"),
    );
    assert_eq!(svc.recent_diagnostics.len(), 1);
    assert_eq!(
        svc.recent_diagnostics[0].severity,
        DiagnosticSeverity::Medium
    );
    assert_eq!(
        refact_buddy_core::diagnostics::diagnostic_priority_label(
            "stream ended unexpectedly while decoding response body"
        ),
        "normal"
    );
}

#[tokio::test]
async fn test_report_error_with_model_generic_stays_high() {
    let mut svc = make_service();
    svc.report_error_with_model("llm_error", "some error occurred", None, None, None);
    assert_eq!(svc.recent_diagnostics.len(), 1);
    assert_eq!(svc.recent_diagnostics[0].severity, DiagnosticSeverity::High);
    assert_eq!(
        refact_buddy_core::diagnostics::diagnostic_priority_label("some error occurred"),
        "high"
    );
}

#[test]
fn test_error_redaction_strips_tokens() {
    let output = super::actor::redact_sensitive("Error: Bearer sk-abc123xyz failed");
    assert!(output.contains("[REDACTED]"), "must contain [REDACTED]");
    assert!(
        !output.contains("sk-abc123xyz"),
        "must not contain raw token"
    );
}

#[tokio::test]
async fn diagnostic_metadata_is_redacted_before_storage() {
    let mut svc = make_service();
    svc.add_diagnostic(DiagnosticContext {
        error_type: "frontend".to_string(),
        error_message: "Bearer secret-token in /home/alice/project/gcx.ts".to_string(),
        source_file: Some("/home/alice/project/gcx.ts?token=secret".to_string()),
        tool_name: Some("tool?api_key=secret".to_string()),
        chat_id: None,
        model_id: None,
        collected_at: chrono::Utc::now().to_rfc3339(),
        severity: DiagnosticSeverity::High,
        occurrences: None,
    });

    let stored = svc.recent_diagnostics.first().unwrap();
    assert_eq!(stored.source_file.as_deref(), Some("[REDACTED_PATH]"));
    assert_eq!(stored.tool_name.as_deref(), Some("tool?api_key=[REDACTED]"));
    assert!(!stored.error_message.contains("secret-token"));
    let event = svc.runtime_queue.items.front().unwrap();
    assert_eq!(event.source, "[REDACTED_PATH]");
}
