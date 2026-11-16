use boundrail::hotspots;
use serde_json::json;

#[test]
fn validation_reads_older_cached_reports_without_random_baselines() {
    let validation: hotspots::Validation = serde_json::from_value(json!({
        "files":10,"top_files":1,"bugfix_commits":2,"top_bugfix_commits":1,
        "share":0.5,"lift":5.0
    }))
    .unwrap();
    assert_eq!(validation.expected_random_share, None);
    assert_eq!(validation.random_lift, None);
}
