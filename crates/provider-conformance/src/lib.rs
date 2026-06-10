//! Embedded baseline conformance scenarios.
//!
//! The scenarios are compiled into the crate via `include_str!` so downstream
//! plugin repos can depend on this crate as a dev-dependency without needing
//! to vendor the YAML files.

use anyhow::Result;
use testkit_core::ScenarioFile;

const BASELINE_YAMLS: &[(&str, &str)] = &[
    (
        "streaming-short",
        include_str!("../../../scenarios/streaming-short.yaml"),
    ),
    (
        "streaming-medium",
        include_str!("../../../scenarios/streaming-medium.yaml"),
    ),
    (
        "streaming-long",
        include_str!("../../../scenarios/streaming-long.yaml"),
    ),
    (
        "tool-call-single",
        include_str!("../../../scenarios/tool-call-single.yaml"),
    ),
    (
        "tool-call-parallel",
        include_str!("../../../scenarios/tool-call-parallel.yaml"),
    ),
    (
        "tool-call-single-oai",
        include_str!("../../../scenarios/tool-call-single-oai.yaml"),
    ),
    (
        "tool-call-parallel-oai",
        include_str!("../../../scenarios/tool-call-parallel-oai.yaml"),
    ),
    (
        "error-recovery",
        include_str!("../../../scenarios/error-recovery.yaml"),
    ),
    (
        "cancellation",
        include_str!("../../../scenarios/cancellation.yaml"),
    ),
    (
        "resume-session",
        include_str!("../../../scenarios/resume-session.yaml"),
    ),
];

/// Load the baseline provider conformance scenarios in deterministic order.
pub fn baseline_scenarios() -> Result<Vec<ScenarioFile>> {
    let mut out = Vec::with_capacity(BASELINE_YAMLS.len());
    for (name, raw) in BASELINE_YAMLS {
        let scenario: ScenarioFile = serde_yaml::from_str(raw)
            .map_err(|e| anyhow::anyhow!("baseline scenario `{name}` failed to parse: {e}"))?;
        out.push(scenario);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_scenarios_parse_and_have_expected_names() {
        let scenarios = baseline_scenarios().unwrap();
        let names: Vec<&str> = scenarios.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"streaming-short"));
        assert!(names.contains(&"streaming-long"));
        assert!(names.contains(&"tool-call-single"));
        assert!(names.contains(&"resume-session"));
        assert!(names.contains(&"tool-call-single-oai"));
        assert!(names.contains(&"tool-call-parallel-oai"));
        assert_eq!(scenarios.len(), 10);
    }

    #[test]
    fn embedded_names_match_scenario_names_in_order() {
        let scenarios = baseline_scenarios().unwrap();
        let names: Vec<&str> = scenarios.iter().map(|s| s.name.as_str()).collect();
        let embedded_names: Vec<&str> = BASELINE_YAMLS.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, embedded_names);
    }

    #[test]
    fn resume_scenario_requires_resume_capability_and_session_id() {
        let scenarios = baseline_scenarios().unwrap();
        let resume = scenarios
            .iter()
            .find(|s| s.name == "resume-session")
            .expect("resume-session scenario present");
        assert_eq!(resume.method, testkit_core::ScenarioMethod::Resume);
        assert!(resume
            .request
            .session_id
            .as_deref()
            .is_some_and(|id| !id.is_empty()));
        assert!(resume
            .requires_capabilities
            .iter()
            .any(|cap| cap == "agent/resume"));
    }

    #[test]
    fn oai_scenarios_are_gated_by_oai_style_capability() {
        let scenarios = baseline_scenarios().unwrap();
        for scenario in scenarios.iter().filter(|s| s.name.ends_with("-oai")) {
            assert_eq!(scenario.mock.tool.as_deref(), Some("oai"));
            assert!(
                scenario
                    .requires_capabilities
                    .iter()
                    .any(|cap| cap == "$harness/oai-style"),
                "{} must opt into the stateless OAI harness branch",
                scenario.name
            );
        }
    }

    #[test]
    fn mutually_exclusive_tool_scenarios_have_skip_gates() {
        let scenarios = baseline_scenarios().unwrap();
        for scenario in scenarios
            .iter()
            .filter(|s| s.name.starts_with("tool-call-"))
        {
            assert!(
                scenario
                    .skip_if_capabilities
                    .iter()
                    .any(|cap| cap == "$harness/no-tool-events"),
                "{} must skip providers that cannot surface tool notifications",
                scenario.name
            );
        }
        for scenario in scenarios
            .iter()
            .filter(|s| s.name == "tool-call-single" || s.name == "tool-call-parallel")
        {
            assert!(
                scenario
                    .skip_if_capabilities
                    .iter()
                    .any(|cap| cap == "$harness/oai-style"),
                "{} must skip stateless OAI-style providers",
                scenario.name
            );
        }
    }
}
