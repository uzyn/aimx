use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

fn setup_test_env(tmp: &Path) -> String {
    let config_content = format!(
        "domain: agent.example.com\ndata_dir: {}\nmailboxes:\n  catchall:\n    address: \"*@agent.example.com\"\n  alice:\n    address: alice@agent.example.com\n",
        tmp.display()
    );
    std::fs::create_dir_all(tmp.join("catchall")).unwrap();
    std::fs::create_dir_all(tmp.join("alice")).unwrap();
    let config_path = tmp.join("config.yaml");
    std::fs::write(&config_path, &config_content).unwrap();
    config_path.to_string_lossy().to_string()
}

#[test]
fn help_shows_subcommands() {
    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("ingest"))
        .stdout(predicate::str::contains("send"))
        .stdout(predicate::str::contains("mailbox"))
        .stdout(predicate::str::contains("mcp"))
        .stdout(predicate::str::contains("setup"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("preflight"))
        .stdout(predicate::str::contains("verify"))
        .stdout(predicate::str::contains("dkim-keygen"));
}

#[test]
fn ingest_plain_fixture() {
    let tmp = TempDir::new().unwrap();
    let _config_path = setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    assert!(!eml.is_empty());

    let message = mail_parser::MessageParser::default().parse(&eml).unwrap();
    assert_eq!(message.subject().unwrap(), "Plain text test");
    assert!(message.body_text(0).is_some());
}

#[test]
fn ingest_html_fixture() {
    let eml = std::fs::read("tests/fixtures/html_only.eml").unwrap();
    let message = mail_parser::MessageParser::default().parse(&eml).unwrap();
    assert_eq!(message.subject().unwrap(), "HTML only test");
    assert!(message.body_html(0).is_some());
}

#[test]
fn ingest_multipart_fixture() {
    let eml = std::fs::read("tests/fixtures/multipart.eml").unwrap();
    let message = mail_parser::MessageParser::default().parse(&eml).unwrap();
    assert_eq!(message.subject().unwrap(), "Multipart test");
    assert!(message.body_text(0).is_some());
}

#[test]
fn ingest_attachment_fixture() {
    let eml = std::fs::read("tests/fixtures/with_attachment.eml").unwrap();
    let message = mail_parser::MessageParser::default().parse(&eml).unwrap();
    assert_eq!(message.subject().unwrap(), "Email with attachment");
    assert!(message.attachment_count() > 0);
}
