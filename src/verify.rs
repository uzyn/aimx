use crate::config::Config;
use crate::send;
use std::path::Path;

const VERIFY_ADDRESS: &str = "verify@aimx.email";
const VERIFY_SUBJECT: &str = "aimx verify";
const POLL_INTERVAL_SECS: u64 = 5;
const MAX_WAIT_SECS: u64 = 120;

pub fn run(data_dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let config = match data_dir {
        Some(dir) => Config::load_from_data_dir(dir)?,
        None => Config::load_default()?,
    };

    let from = format!("catchall@{}", config.domain);

    println!("aimx verify - End-to-end email verification\n");

    let catchall_dir = config.mailbox_dir("catchall");
    if !catchall_dir.exists() {
        return Err(format!(
            "Catchall mailbox directory does not exist: {}\nRun `aimx setup` first.",
            catchall_dir.display()
        )
        .into());
    }

    // Take snapshot BEFORE sending to avoid race condition where a fast reply
    // arrives between send and snapshot, causing it to be missed.
    let before: Vec<String> = list_md_files(&catchall_dir);

    println!("Sending test email from {from} to {VERIFY_ADDRESS}...");

    let send_args = crate::cli::SendArgs {
        from: from.clone(),
        to: VERIFY_ADDRESS.to_string(),
        subject: VERIFY_SUBJECT.to_string(),
        body: format!(
            "This is an automated verification email from aimx on {}.\n\
             Please verify DKIM and SPF and reply with results.",
            config.domain
        ),
        reply_to: None,
        references: None,
        attachments: vec![],
    };

    send::run(send_args, data_dir)?;
    println!("Test email sent.\n");

    println!("Waiting for reply from {VERIFY_ADDRESS}...");
    println!("(This may take up to {MAX_WAIT_SECS} seconds)\n");

    let mut elapsed = 0u64;
    while elapsed < MAX_WAIT_SECS {
        std::thread::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS));
        elapsed += POLL_INTERVAL_SECS;

        let after = list_md_files(&catchall_dir);
        let new_files: Vec<&String> = after.iter().filter(|f| !before.contains(f)).collect();

        for file in &new_files {
            let content = std::fs::read_to_string(file).unwrap_or_default();
            if content.contains("aimx verification result") || content.contains(VERIFY_ADDRESS) {
                println!("Reply received!\n");
                print_verification_result(&content);
                return Ok(());
            }
        }

        print!(".");
        std::io::Write::flush(&mut std::io::stdout())?;
    }

    println!("\n");
    Err(format!(
        "Timed out waiting for reply from {VERIFY_ADDRESS}.\n\
         This could mean:\n\
         - DNS records are not yet propagated\n\
         - DKIM signing is not configured correctly\n\
         - The verify service at aimx.email is temporarily unavailable\n\n\
         Run `aimx status` to check your configuration."
    )
    .into())
}

fn list_md_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
                .map(|e| e.path().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn print_verification_result(content: &str) {
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    if parts.len() >= 3 {
        let body = parts[2].trim();
        if !body.is_empty() {
            println!("Verification reply body:");
            println!("{body}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_md_files_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let files = list_md_files(tmp.path());
        assert!(files.is_empty());
    }

    #[test]
    fn list_md_files_finds_md() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("2025-01-01-001.md"), "test").unwrap();
        std::fs::write(tmp.path().join("2025-01-01-002.md"), "test").unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "not md").unwrap();
        let files = list_md_files(tmp.path());
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn list_md_files_nonexistent_dir() {
        let files = list_md_files(Path::new("/nonexistent/dir"));
        assert!(files.is_empty());
    }

    #[test]
    fn print_verification_result_does_not_panic() {
        let content = "---\nid: test\n---\nDKIM: pass\nSPF: pass\n";
        print_verification_result(content);
    }

    #[test]
    fn print_verification_result_empty() {
        print_verification_result("");
    }

    #[test]
    fn verify_address_is_correct() {
        assert_eq!(VERIFY_ADDRESS, "verify@aimx.email");
    }

    #[test]
    fn verify_subject_is_correct() {
        assert_eq!(VERIFY_SUBJECT, "aimx verify");
    }

    #[test]
    fn run_errors_on_missing_catchall_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Create config but no catchall directory
        let config_content = format!(
            "domain: test.com\ndata_dir: {}\nmailboxes:\n  catchall:\n    address: \"*@test.com\"\n",
            tmp.path().display()
        );
        std::fs::write(tmp.path().join("config.yaml"), config_content).unwrap();

        let result = run(Some(tmp.path()));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Catchall mailbox directory does not exist"),
            "Expected missing catchall error, got: {err}"
        );
    }
}
