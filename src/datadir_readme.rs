// VERSION BUMP: increment VERSION and update src/datadir_readme.md.tpl whenever
// the template changes. The first line of the template must be
// `<!-- aimx-readme-version: N -->` where N matches VERSION below.

use std::path::Path;

pub const TEMPLATE: &str = include_str!("datadir_readme.md.tpl");
pub const VERSION: u32 = 6;

fn version_line() -> String {
    format!("<!-- aimx-readme-version: {VERSION} -->")
}

pub fn write(data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let dest = data_dir.join("README.md");
    std::fs::write(&dest, TEMPLATE)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

pub fn refresh_if_outdated(data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let dest = data_dir.join("README.md");
    let expected = version_line();

    let needs_write = match std::fs::read_to_string(&dest) {
        Ok(contents) => match contents.lines().next() {
            Some(first_line) => first_line.trim() != expected,
            None => true,
        },
        Err(_) => true,
    };

    if needs_write {
        write(data_dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn template_starts_with_version_comment() {
        let first_line = TEMPLATE.lines().next().unwrap();
        assert_eq!(
            first_line.trim(),
            format!("<!-- aimx-readme-version: {VERSION} -->")
        );
    }

    #[test]
    fn write_creates_file() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path()).unwrap();
        let dest = tmp.path().join("README.md");
        assert!(dest.exists());
        let content = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(content, TEMPLATE);
    }

    #[cfg(unix)]
    #[test]
    fn write_sets_mode_644() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        write(tmp.path()).unwrap();
        let mode = std::fs::metadata(tmp.path().join("README.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[test]
    fn refresh_noop_when_version_matches() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path()).unwrap();
        let dest = tmp.path().join("README.md");
        let before = std::fs::metadata(&dest).unwrap().modified().unwrap();
        // Sleep briefly so mtime would differ if the file were rewritten.
        std::thread::sleep(std::time::Duration::from_millis(50));
        refresh_if_outdated(tmp.path()).unwrap();
        let after = std::fs::metadata(&dest).unwrap().modified().unwrap();
        assert_eq!(
            before, after,
            "file should not be rewritten when version matches"
        );
    }

    #[test]
    fn refresh_overwrites_when_version_differs() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("README.md");
        std::fs::write(&dest, "<!-- aimx-readme-version: 0 -->\nstale content\n").unwrap();
        refresh_if_outdated(tmp.path()).unwrap();
        let content = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(content, TEMPLATE);
    }

    #[test]
    fn refresh_overwrites_when_first_line_missing() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("README.md");
        std::fs::write(&dest, "").unwrap();
        refresh_if_outdated(tmp.path()).unwrap();
        let content = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(content, TEMPLATE);
    }

    #[test]
    fn refresh_overwrites_when_first_line_malformed() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("README.md");
        std::fs::write(&dest, "not a version comment\nsome content\n").unwrap();
        refresh_if_outdated(tmp.path()).unwrap();
        let content = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(content, TEMPLATE);
    }

    #[test]
    fn refresh_creates_file_when_missing() {
        let tmp = TempDir::new().unwrap();
        refresh_if_outdated(tmp.path()).unwrap();
        let content = std::fs::read_to_string(tmp.path().join("README.md")).unwrap();
        assert_eq!(content, TEMPLATE);
    }
}
