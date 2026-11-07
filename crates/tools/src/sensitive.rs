//! What counts as a secret. One list, used both by the sandbox (to hide these from commands) and by
//! the engine (reading or writing one always asks, and can't be allowed "always").

use crate::workspace::Location;
use std::path::Path;

/// Credentials and private data under the home folder.
pub const HOME_SECRETS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".config/gcloud",
    ".config/gh",
    ".local/share/keyrings",
    ".password-store",
    ".mozilla",
    ".config/google-chrome",
    ".config/chromium",
    ".config/BraveSoftware",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".pgpass",
    ".cargo/credentials",
    ".cargo/credentials.toml",
];

/// File names that usually hold secrets, wherever they are.
fn secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let exact = [
        ".env",
        ".envrc",
        ".netrc",
        ".pgpass",
        ".git-credentials",
        ".npmrc",
        ".pypirc",
        "credentials",
        "credentials.json",
        "credentials.toml",
        "secrets.yaml",
        "secrets.yml",
        "secrets.json",
        "id_rsa",
        "id_ecdsa",
        "id_ed25519",
        "id_dsa",
    ];
    let suffixes = [".pem", ".key", ".p12", ".pfx", ".kdbx", ".keystore", ".jks", ".gpg", ".asc"];
    exact.contains(&lower.as_str())
        || (lower.starts_with(".env.") && !lower.ends_with(".example") && !lower.ends_with(".sample"))
        || suffixes.iter().any(|suffix| lower.ends_with(suffix))
}

/// Whether a location holds, or is inside, something secret.
#[must_use]
pub fn is_sensitive(location: &Location, root: &Path) -> bool {
    let path = match location {
        Location::Inside(relative) => root.join(relative),
        Location::Outside(path) => path.clone(),
    };
    if path.file_name().and_then(|name| name.to_str()).is_some_and(secret_name) {
        return true;
    }
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else { return false };
    HOME_SECRETS.iter().any(|secret| path.starts_with(home.join(secret)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn outside(path: &str) -> Location {
        Location::Outside(PathBuf::from(path))
    }

    #[test]
    fn secret_files_and_folders_are_recognised_anywhere() {
        let root = Path::new("/project");
        let home = std::env::var("HOME").unwrap();
        for path in [
            format!("{home}/.ssh/id_ed25519"),
            format!("{home}/.ssh"),
            format!("{home}/.config/gh/hosts.yml"),
            format!("{home}/.mozilla/firefox/x.default/logins.json"),
            "/srv/app/.env".to_owned(),
            "/srv/app/.env.production".to_owned(),
            "/etc/ssl/private/server.key".to_owned(),
            "/home/other/vault.kdbx".to_owned(),
        ] {
            assert!(is_sensitive(&outside(&path), root), "{path} should be sensitive");
        }
        assert!(is_sensitive(&Location::Inside(".env".into()), root), "inside the project too");
        assert!(is_sensitive(&Location::Inside("certs/dev.pem".into()), root));
    }

    #[test]
    fn ordinary_files_are_not() {
        let root = Path::new("/project");
        let home = std::env::var("HOME").unwrap();
        for path in
            [format!("{home}/notes/todo.md"), format!("{home}/.config/re-code/config.toml"), "/etc/hosts".to_owned()]
        {
            assert!(!is_sensitive(&outside(&path), root), "{path} shouldn't be sensitive");
        }
        assert!(!is_sensitive(&Location::Inside(".env.example".into()), root));
        assert!(!is_sensitive(&Location::Inside("src/keyboard.rs".into()), root));
    }
}
