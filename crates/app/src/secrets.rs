//! API keys, kept in the OS keyring rather than on disk.
//!
//! A key is stored under the re-code service and the provider's name. If the keyring can't be
//! reached (locked, headless, or absent), storing fails and the key simply isn't remembered; the
//! program keeps working and says so, rather than writing a secret to a file.

use async_trait::async_trait;
use std::collections::HashMap;

const SERVICE: &str = "re-code";

/// Stores and retrieves API keys. A trait so the driver can be tested without a real keyring.
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn set(&self, account: &str, secret: &str) -> Result<(), String>;
    async fn get(&self, account: &str) -> Result<Option<String>, String>;
    async fn delete(&self, account: &str) -> Result<(), String>;
    /// Which of the given accounts have a stored key. Missing or unreadable means "no".
    async fn present(&self, accounts: &[String]) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for account in accounts {
            if matches!(self.get(account).await, Ok(Some(_))) {
                out.insert(account.clone());
            }
        }
        out
    }
}

fn attributes(account: &str) -> HashMap<&str, &str> {
    HashMap::from([("service", SERVICE), ("account", account)])
}

/// The real OS keyring.
pub struct Keyring {
    keyring: oo7::Keyring,
}

impl Keyring {
    /// Opens the keyring. This can prompt the user to unlock it the first time.
    pub async fn open() -> Result<Self, String> {
        let keyring = oo7::Keyring::new().await.map_err(|error| error.to_string())?;
        keyring.unlock().await.map_err(|error| error.to_string())?;
        Ok(Self { keyring })
    }
}

#[async_trait]
impl SecretStore for Keyring {
    async fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        let label = format!("re-code API key for {account}");
        self.keyring
            .create_item(&label, &attributes(account), secret.as_bytes(), true)
            .await
            .map_err(|error| error.to_string())
    }

    async fn get(&self, account: &str) -> Result<Option<String>, String> {
        let items = self.keyring.search_items(&attributes(account)).await.map_err(|error| error.to_string())?;
        let Some(item) = items.first() else { return Ok(None) };
        let secret = item.secret().await.map_err(|error| error.to_string())?;
        String::from_utf8(secret.to_vec()).map(Some).map_err(|_| "the stored key isn't valid text".to_owned())
    }

    async fn delete(&self, account: &str) -> Result<(), String> {
        self.keyring.delete(&attributes(account)).await.map_err(|error| error.to_string())
    }
}

/// Used when the keyring can't be opened: keys aren't remembered, and the program says so.
pub struct Unavailable {
    pub reason: String,
}

#[async_trait]
impl SecretStore for Unavailable {
    async fn set(&self, _account: &str, _secret: &str) -> Result<(), String> {
        Err(format!("the system keyring isn't available ({}), so the key can't be saved", self.reason))
    }
    async fn get(&self, _account: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    async fn delete(&self, _account: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// An in-memory store for tests.
    #[derive(Default)]
    pub(crate) struct MemoryStore {
        items: Mutex<BTreeMap<String, String>>,
    }

    #[async_trait]
    impl SecretStore for MemoryStore {
        async fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            self.items.lock().unwrap().insert(account.to_owned(), secret.to_owned());
            Ok(())
        }
        async fn get(&self, account: &str) -> Result<Option<String>, String> {
            Ok(self.items.lock().unwrap().get(account).cloned())
        }
        async fn delete(&self, account: &str) -> Result<(), String> {
            self.items.lock().unwrap().remove(account);
            Ok(())
        }
    }

    #[tokio::test]
    async fn stores_and_removes_keys() {
        let store = MemoryStore::default();
        store.set("anthropic", "sk-1").await.unwrap();
        assert_eq!(store.get("anthropic").await.unwrap().as_deref(), Some("sk-1"));
        assert_eq!(store.present(&["anthropic".into(), "openai".into()]).await.len(), 1);
        store.delete("anthropic").await.unwrap();
        assert_eq!(store.get("anthropic").await.unwrap(), None);
    }

    // Needs a running, unlocked Secret Service.
    #[tokio::test]
    #[ignore = "writes to the real OS keyring"]
    async fn the_real_keyring_round_trips() {
        let keyring = Keyring::open().await.unwrap();
        keyring.set("re-code-test", "value-123").await.unwrap();
        assert_eq!(keyring.get("re-code-test").await.unwrap().as_deref(), Some("value-123"));
        keyring.delete("re-code-test").await.unwrap();
        assert_eq!(keyring.get("re-code-test").await.unwrap(), None);
    }
}
