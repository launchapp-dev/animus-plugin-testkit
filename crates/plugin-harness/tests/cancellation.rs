//! End-to-end test of the v0.3.0 concurrent-cancel dispatcher.

use std::path::PathBuf;

use testkit_core::{
    ExpectedNotification, ExpectedResponse, MockHint, ScenarioFile, ScenarioMethod,
    ScenarioRequest, TestStatus,
};

fn fake_plugin_binary() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let bin = dir.join("fake-cancellable-plugin");
    assert!(
        bin.is_file(),
        "expected fake-cancellable-plugin at {}",
        bin.display()
    );
    bin
}

fn cancellation_scenario() -> ScenarioFile {
    ScenarioFile {
        name: "cancellation".to_string(),
        description: String::new(),
        timeout_ms: 3000,
        request: ScenarioRequest {
            prompt: "stream until cancelled".to_string(),
            model: Some("test-model".to_string()),
            system_prompt: None,
            cwd: None,
            session_id: None,
            env: Default::default(),
        },
        expected_notifications: Vec::<ExpectedNotification>::new(),
        allow_extra_notifications: true,
        expected_response: ExpectedResponse::default(),
        mock: MockHint::default(),
        requires_capabilities: vec!["$harness/cancellation-loop-v2".to_string()],
        method: ScenarioMethod::Run,
        cancel_after_ms: Some(50),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_dispatcher_drives_cancel_and_passes() {
    let plugin = fake_plugin_binary();
    let report = plugin_harness::protocol::run_all(plugin, vec![cancellation_scenario()], None)
        .await
        .expect("run_all");
    assert_eq!(report.scenarios.len(), 1);
    let r = &report.scenarios[0];
    assert_eq!(
        r.status,
        TestStatus::Pass,
        "expected PASS, got {:?}; diagnostics={:?}",
        r.status,
        r.diagnostics
    );
}
