mod common;

use common::archgraph;

#[test]
fn executable_identity_is_boundrail() {
    let version = archgraph(&["--version"]);
    assert!(version.status.success());
    assert!(
        String::from_utf8(version.stdout)
            .unwrap()
            .starts_with("boundrail ")
    );
    let help = archgraph(&["--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("Usage: boundrail")
    );
}
