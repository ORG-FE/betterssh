use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

use crate::storage::config_dir;

const MAGIC: &[u8; 4] = b"BVLT";
const VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KDF_MEM: u32 = 32 * 1024;
const KDF_TIME: u32 = 4;
const KDF_PARALLELISM: u32 = 1;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Vault {
    pub secrets: HashMap<String, Secret>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Secret {
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub key_passphrase: HashMap<String, String>,
}

impl Vault {
    pub fn get(&self, host_id: &str) -> Option<&Secret> {
        self.secrets.get(host_id)
    }

    pub fn set(&mut self, host_id: &str, secret: Secret) {
        self.secrets.insert(host_id.to_string(), secret);
    }

    pub fn remove(&mut self, host_id: &str) {
        self.secrets.remove(host_id);
    }
}

pub fn vault_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("vault.enc"))
}

pub fn vault_exists() -> bool {
    vault_path().map(|p| p.exists()).unwrap_or(false)
}

#[derive(Zeroize)]
struct DerivedKey([u8; 32]);

impl Drop for DerivedKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub fn create(master_password: &str) -> Result<Vault> {
    let v = Vault::default();
    save(&v, master_password)?;
    Ok(v)
}

pub fn load(master_password: &str) -> Result<Vault> {
    let path = vault_path()?;
    let raw = std::fs::read(&path)
        .with_context(|| format!("read vault {}", path.display()))?;
    let v = decrypt(&raw, master_password)?;
    Ok(v)
}

pub fn save(vault: &Vault, master_password: &str) -> Result<()> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let key = derive_key_with_salt(master_password, &salt)?;
    let plaintext = serde_json::to_vec(vault).context("serialize vault")?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key.0));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|e| anyhow!("encrypt: {}", e))?;

    let mut out = Vec::with_capacity(4 + 1 + SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);

    let path = vault_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = path.with_extension("enc.tmp");
    std::fs::write(&tmp, &out).context("write tmp vault")?;
    set_0600(&tmp).ok();
    std::fs::rename(&tmp, &path).context("rename vault")?;
    Ok(())
}

fn derive_key_with_salt(master_password: &str, salt: &[u8]) -> Result<DerivedKey> {
    let params = Params::new(KDF_MEM, KDF_TIME, KDF_PARALLELISM, Some(32))
        .map_err(|e| anyhow!("argon2 params: {}", e))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(master_password.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow!("argon2: {}", e))?;
    Ok(DerivedKey(out))
}

fn decrypt(raw: &[u8], master_password: &str) -> Result<Vault> {
    if raw.len() < 4 + 1 + SALT_LEN + NONCE_LEN {
        return Err(anyhow!("vault too short"));
    }
    if &raw[0..4] != MAGIC {
        return Err(anyhow!("vault: bad magic"));
    }
    if raw[4] != VERSION {
        return Err(anyhow!("vault: unsupported version {}", raw[4]));
    }
    let salt = &raw[5..5 + SALT_LEN];
    let nonce_bytes = &raw[5 + SALT_LEN..5 + SALT_LEN + NONCE_LEN];
    let ciphertext = &raw[5 + SALT_LEN + NONCE_LEN..];

    let key = derive_key_with_salt(master_password, salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key.0));
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow!("vault: wrong password or corrupted file"))?;
    let v: Vault = serde_json::from_slice(&plaintext).context("parse vault json")?;
    Ok(v)
}

#[cfg(unix)]
fn set_0600(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)?.permissions();
    perm.set_mode(0o600);
    std::fs::set_permissions(path, perm)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_0600(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub fn host_id(host_name: &str, user: &str) -> String {
    format!("{}@{}", user, host_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn test_vault_path(dir: &Path) -> PathBuf {
        dir.join("vault.enc")
    }

    #[test]
    fn create_and_load_vault() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());
        let pass = "test_master_password_123";

        let vault = Vault::default();
        save_with_path(&vault, pass, &path).unwrap();

        let loaded = load_with_path(pass, &path).unwrap();
        assert!(loaded.secrets.is_empty());
    }

    #[test]
    fn wrong_password_fails() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());

        save_with_path(&Vault::default(), "correct", &path).unwrap();
        let result = load_with_path("wrong", &path);
        assert!(result.is_err());
    }

    #[test]
    fn save_and_retrieve_secrets() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());
        let pass = "secret123";

        let mut vault = Vault::default();
        vault.set("root@1.2.3.4", Secret {
            password: "mypassword".into(),
            key_passphrase: HashMap::new(),
        });
        vault.set("admin@5.6.7.8", Secret {
            password: "adminpass".into(),
            key_passphrase: {
                let mut m = HashMap::new();
                m.insert("id_rsa".into(), "keyphrase".into());
                m
            },
        });

        save_with_path(&vault, pass, &path).unwrap();
        let loaded = load_with_path(pass, &path).unwrap();

        assert_eq!(loaded.secrets.len(), 2);
        assert_eq!(loaded.get("root@1.2.3.4").unwrap().password, "mypassword");
        assert_eq!(loaded.get("admin@5.6.7.8").unwrap().password, "adminpass");
        assert_eq!(
            loaded.get("admin@5.6.7.8").unwrap().key_passphrase.get("id_rsa").unwrap(),
            "keyphrase"
        );
    }

    #[test]
    fn remove_secret() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());
        let pass = "pass";

        let mut vault = Vault::default();
        vault.set("a@b", Secret { password: "x".into(), key_passphrase: HashMap::new() });
        save_with_path(&vault, pass, &path).unwrap();

        let mut loaded = load_with_path(pass, &path).unwrap();
        assert!(loaded.get("a@b").is_some());
        loaded.remove("a@b");
        save_with_path(&loaded, pass, &path).unwrap();

        let final_vault = load_with_path(pass, &path).unwrap();
        assert!(final_vault.get("a@b").is_none());
    }

    #[test]
    fn host_id_format() {
        assert_eq!(host_id("1.2.3.4", "root"), "root@1.2.3.4");
        assert_eq!(host_id("myserver.com", "admin"), "admin@myserver.com");
    }

    #[test]
    fn corrupted_file_fails() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());
        fs::write(&path, b"not a vault file").unwrap();
        assert!(load_with_path("any", &path).is_err());
    }

    #[test]
    fn truncated_file_fails() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());
        fs::write(&path, b"BVLT").unwrap();
        assert!(load_with_path("any", &path).is_err());
    }

    #[test]
    fn empty_vault_file() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());
        fs::write(&path, b"").unwrap();
        assert!(load_with_path("any", &path).is_err());
    }

    #[test]
    fn save_creates_file() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());
        assert!(!path.exists());
        save_with_path(&Vault::default(), "pass", &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn multiple_saves_with_different_passwords() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());

        save_with_path(&Vault::default(), "pass1", &path).unwrap();
        assert!(load_with_path("pass1", &path).is_ok());
        assert!(load_with_path("pass2", &path).is_err());

        let mut vault = Vault::default();
        vault.set("x@y", Secret { password: "z".into(), key_passphrase: HashMap::new() });
        save_with_path(&vault, "pass2", &path).unwrap();
        assert!(load_with_path("pass1", &path).is_err());
        let loaded = load_with_path("pass2", &path).unwrap();
        assert_eq!(loaded.get("x@y").unwrap().password, "z");
    }

    #[test]
    fn vault_encrypted_not_plaintext() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());

        let mut vault = Vault::default();
        vault.set("a@b", Secret { password: "secret_password".into(), key_passphrase: HashMap::new() });
        save_with_path(&vault, "pass", &path).unwrap();

        let raw = fs::read(&path).unwrap();
        let as_str = String::from_utf8_lossy(&raw);
        assert!(!as_str.contains("secret_password"), "vault contains plaintext password!");
        assert!(!as_str.contains("a@b"), "vault contains plaintext host_id!");
    }

    #[test]
    fn two_secrets_with_same_key_different_value() {
        let tmp = TempDir::new().unwrap();
        let path = test_vault_path(tmp.path());

        let mut v1 = Vault::default();
        v1.set("a@b", Secret { password: "first".into(), key_passphrase: HashMap::new() });
        save_with_path(&v1, "pass", &path).unwrap();

        let mut v2 = load_with_path("pass", &path).unwrap();
        v2.set("a@b", Secret { password: "second".into(), key_passphrase: HashMap::new() });
        save_with_path(&v2, "pass", &path).unwrap();

        let loaded = load_with_path("pass", &path).unwrap();
        assert_eq!(loaded.get("a@b").unwrap().password, "second");
    }

    fn save_with_path(vault: &Vault, pass: &str, path: &Path) -> Result<()> {
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let key = derive_key_with_salt(pass, &salt)?;
        let plaintext = serde_json::to_vec(vault).context("serialize")?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key.0));
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher.encrypt(nonce, plaintext.as_ref()).map_err(|e| anyhow!("enc: {}", e))?;

        let mut out = Vec::with_capacity(4 + 1 + SALT_LEN + NONCE_LEN + ciphertext.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&salt);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(path, &out)?;
        Ok(())
    }

    fn load_with_path(pass: &str, path: &Path) -> Result<Vault> {
        let raw = fs::read(path).context("read vault")?;
        let v = decrypt(&raw, pass)?;
        Ok(v)
    }
}
