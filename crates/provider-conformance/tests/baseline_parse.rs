use provider_conformance::baseline_scenarios;

#[test]
fn all_baseline_scenarios_have_a_mock_hint_or_explicit_skip() {
    let scenarios = baseline_scenarios().expect("scenarios load");
    for s in &scenarios {
        let has_mock = s.mock.tool.is_some();
        let is_skip_scenario = !s.requires_capabilities.is_empty();
        assert!(
            has_mock || is_skip_scenario,
            "scenario `{}` has neither a mock hint nor requires_capabilities",
            s.name
        );
    }
}

#[test]
fn cancellation_is_gated_by_harness_capability() {
    let scenarios = baseline_scenarios().expect("scenarios load");
    let cancel = scenarios
        .iter()
        .find(|s| s.name == "cancellation")
        .expect("cancellation scenario present");
    assert!(
        cancel
            .requires_capabilities
            .iter()
            .any(|c| c.starts_with("$harness/")),
        "cancellation scenario should require a harness-internal capability so plugins \
         that haven't opted in SKIP cleanly"
    );
    assert!(
        cancel.cancel_after_ms.is_some(),
        "cancellation scenario must set cancel_after_ms so the v0.3.0 concurrent \
         dispatcher actually issues agent/cancel"
    );
}
