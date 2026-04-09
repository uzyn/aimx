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

fn read_frontmatter(md_path: &Path) -> serde_yaml::Value {
    let content = std::fs::read_to_string(md_path).unwrap();
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    assert!(
        parts.len() >= 3,
        "Markdown file missing frontmatter delimiters"
    );
    serde_yaml::from_str(parts[1].trim()).unwrap()
}

fn get_yaml_str<'a>(map: &'a serde_yaml::Mapping, key: &str) -> &'a str {
    map.get(&serde_yaml::Value::String(key.to_string()))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

fn find_md_files(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect()
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
fn help_shows_data_dir_option() {
    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--data-dir"))
        .stdout(predicate::str::contains("AIMX_DATA_DIR"));
}

#[test]
fn ingest_plain_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1);

    let parsed = read_frontmatter(&md_files[0]);
    let map = parsed.as_mapping().unwrap();
    assert_eq!(get_yaml_str(map, "from"), "Alice <alice@example.com>");
    assert_eq!(get_yaml_str(map, "subject"), "Plain text test");
    assert_eq!(get_yaml_str(map, "message_id"), "plain-001@example.com");
    assert_eq!(get_yaml_str(map, "mailbox"), "catchall");
    assert_eq!(
        map.get(&serde_yaml::Value::String("read".to_string()))
            .unwrap(),
        &serde_yaml::Value::Bool(false)
    );

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("This is a plain text email for testing."));
}

#[test]
fn ingest_html_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/html_only.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1);

    let parsed = read_frontmatter(&md_files[0]);
    let map = parsed.as_mapping().unwrap();
    assert_eq!(get_yaml_str(map, "subject"), "HTML only test");

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("Hello from HTML"));
    assert!(!content.contains("<html>"));
}

#[test]
fn ingest_multipart_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/multipart.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1);

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("This is the plain text version."));
    assert!(!content.contains("<html>"));
}

#[test]
fn ingest_attachment_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/with_attachment.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1);

    let att_path = tmp.path().join("catchall/attachments/readme.txt");
    assert!(att_path.exists());
    let att_content = std::fs::read_to_string(&att_path).unwrap();
    assert!(att_content.contains("This is the content of the attached file."));

    let parsed = read_frontmatter(&md_files[0]);
    let map = parsed.as_mapping().unwrap();
    let attachments = map
        .get(&serde_yaml::Value::String("attachments".to_string()))
        .unwrap()
        .as_sequence()
        .unwrap();
    assert_eq!(attachments.len(), 1);
    let att = attachments[0].as_mapping().unwrap();
    assert_eq!(
        att.get(&serde_yaml::Value::String("filename".to_string()))
            .unwrap()
            .as_str()
            .unwrap(),
        "readme.txt"
    );
}

#[test]
fn ingest_routes_to_named_mailbox() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("alice@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let alice_files = find_md_files(&tmp.path().join("alice"));
    assert_eq!(alice_files.len(), 1);

    let catchall_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(catchall_files.len(), 0);
}

#[test]
fn ingest_unknown_routes_to_catchall() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("unknown@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let catchall_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(catchall_files.len(), 1);
}

#[test]
fn ingest_via_env_var() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_DATA_DIR", tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1);
}

#[test]
fn dkim_keygen_end_to_end() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .success()
        .stdout(predicate::str::contains("DKIM keypair generated"))
        .stdout(predicate::str::contains("_domainkey"));

    assert!(tmp.path().join("dkim/private.key").exists());
    assert!(tmp.path().join("dkim/public.key").exists());

    let private_pem = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();
    assert!(private_pem.contains("BEGIN RSA PRIVATE KEY"));

    let public_pem = std::fs::read_to_string(tmp.path().join("dkim/public.key")).unwrap();
    assert!(public_pem.contains("BEGIN RSA PUBLIC KEY"));
}

#[test]
fn dkim_keygen_no_overwrite() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .success();

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exist"));
}

#[test]
fn dkim_keygen_force_overwrite() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .success();

    let original = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .arg("--force")
        .assert()
        .success();

    let new = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();
    assert_ne!(original, new);
}
