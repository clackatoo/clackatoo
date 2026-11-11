//! Private credentials and reviewed project definitions. Files are atomic,
//! owner-only, and never read or written through a final symlink.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use rmcp::transport::auth::{
    AuthError, CredentialRefreshGuard, CredentialStore, StoredCredentials,
};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Private {
    values: BTreeMap<String, String>,
    approvals: BTreeSet<String>,
}

pub(crate) fn directory(path: &Path) -> Result<(), String> {
    if !path.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|_| "Cannot create the private MCP credential directory")?;
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "Cannot inspect the private MCP credential directory")?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != rustix::process::getuid().as_raw()
    {
        return Err(
            "The private MCP credential directory must be a real directory owned by this user"
                .into(),
        );
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "Cannot protect the private MCP credential directory")?;
    Ok(())
}

fn read(path: &Path) -> Result<Option<String>, String> {
    let file = match crate::regular::open(path, crate::regular::Links::Refuse) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read a regular MCP credential file".into()),
    };
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect MCP credential permissions")?;
    if metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("MCP credential files must be owned by this user with mode 0600".into());
    }
    let mut text = String::new();
    file.take((1 << 20) + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Cannot read MCP credentials")?;
    if text.len() > 1 << 20 {
        return Err("MCP credentials exceed their size limit".into());
    }
    Ok(Some(text))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    use ring::rand::SecureRandom as _;
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if text.len() > 1 << 20 {
        return Err("Private MCP credentials exceed 1 MiB".into());
    }
    let dir = path.parent().ok_or("Credential file has no directory")?;
    directory(dir)?;
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Refusing to write credentials through a symlink".into());
    }
    // OAuth callbacks and refreshes may finish in different threads in this
    // process. A unique staging name avoids their writes sharing a partial file.
    let mut nonce = [0; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| "Cannot stage private MCP credentials")?;
    let partial = dir.join(format!(
        ".credentials-{}.partial",
        super::config::hex(&nonce)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(|_| "Cannot stage private MCP credentials")?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|_| "Cannot write private MCP credentials")?;
        std::fs::rename(&partial, path).map_err(|_| "Cannot replace private MCP credentials")?;
        std::fs::File::open(dir)
            .and_then(|file| file.sync_all())
            .map_err(|_| "Cannot synchronize private MCP credentials")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(partial);
    }
    result
}

fn load(dir: &Path) -> Result<Private, String> {
    read(&dir.join("credentials.json"))?.map_or_else(
        || Ok(Private::default()),
        |text| {
            serde_json::from_str(&text).map_err(|_| "Invalid private MCP credential file".into())
        },
    )
}

pub(crate) fn approved(dir: &Path, grant: &str) -> bool {
    load(dir).is_ok_and(|p| p.approvals.contains(grant))
}

pub(crate) fn approve(dir: &Path, grant: String) -> Result<(), String> {
    directory(dir)?;
    let _lock = crate::settings::file::lock(dir)?;
    let mut private = load(dir)?;
    if private.approvals.len() >= 1024 {
        return Err(
            "Too many reviewed MCP revisions; remove old entries from the private credentials file"
                .into(),
        );
    }
    private.approvals.insert(grant);
    write(
        &dir.join("credentials.json"),
        &serde_json::to_string(&private).expect("credentials serialize"),
    )
}

pub(crate) fn set(dir: &Path, name: &str, value: &str) -> Result<(), String> {
    if !super::config::name_ok(name) || value.len() > 16384 || value.contains('\0') {
        return Err("Invalid secret name or value".into());
    }
    directory(dir)?;
    let _lock = crate::settings::file::lock(dir)?;
    let mut private = load(dir)?;
    if value.is_empty() {
        private.values.remove(name);
    } else {
        if private.values.len() >= 256 && !private.values.contains_key(name) {
            return Err("At most 256 MCP secrets may be stored".into());
        }
        private.values.insert(name.into(), value.into());
    }
    write(
        &dir.join("credentials.json"),
        &serde_json::to_string(&private).expect("credentials serialize"),
    )
}

pub(super) fn set_many(dir: &Path, values: BTreeMap<String, String>) -> Result<(), String> {
    if values
        .iter()
        .any(|(k, v)| !super::config::name_ok(k) || v.len() > 16384 || v.contains('\0'))
    {
        return Err("Imported credentials contain unsupported names or values".into());
    }
    directory(dir)?;
    let _lock = crate::settings::file::lock(dir)?;
    let mut private = load(dir)?;
    if private.values.len() + values.len() > 256 {
        return Err("Imported credentials would exceed the 256-secret limit".into());
    }
    for (key, value) in values {
        if private.values.contains_key(&key) {
            return Err("Generated credential name collided; retry the import".into());
        }
        private.values.insert(key, value);
    }
    write(
        &dir.join("credentials.json"),
        &serde_json::to_string(&private).expect("credentials serialize"),
    )
}

pub(crate) fn resolve(dir: &Path, setting: &super::config::Setting) -> Result<String, String> {
    match setting {
        super::config::Setting::Literal(s) => Ok(s.clone()),
        super::config::Setting::Secret { secret } => load(dir)?
            .values
            .remove(secret)
            .ok_or_else(|| format!("Missing private MCP secret ‘{secret}’")),
        super::config::Setting::Environment { env } => {
            std::env::var(env).map_err(|_| format!("Missing MCP environment reference ‘{env}’"))
        }
    }
}

pub(crate) fn redacted(bound: &super::config::Bound, text: &str) -> String {
    redacted_with(bound, text, &[])
}
pub(super) fn redacted_with(bound: &super::config::Bound, text: &str, used: &[String]) -> String {
    let mut values = used.to_vec();
    if let Some(def) = &bound.definition {
        for value in def.env.values().chain(def.headers.values()) {
            if let Ok(secret) = resolve(&bound.private, value)
                && !secret.is_empty()
            {
                values.push(secret);
            }
        }
    }
    if let Ok(private) = load(&bound.private) {
        for value in private.values.values().filter(|s| !s.is_empty()) {
            values.push(value.clone());
        }
    }
    if let Ok(store) = OAuthStore::of(bound)
        && let Ok(Some(tokens)) = read(&store.path())
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&tokens)
    {
        for key in ["access_token", "refresh_token", "id_token"] {
            if let Some(secret) = value
                .get("token_response")
                .and_then(|v| v.get(key))
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
            {
                values.push(secret.to_owned());
            }
        }
    }
    values.retain(|value| !value.is_empty());
    values.sort();
    values.dedup();
    let mut output = String::with_capacity(4096);
    let mut append = |part: &str| {
        for ch in part.chars().filter(|c| !c.is_control() || *c == '\n') {
            if output.len() + ch.len_utf8() > 4096 {
                return false;
            }
            output.push(ch);
        }
        true
    };
    if values.is_empty() {
        append(text);
        return output;
    }
    // Match the original input once. Replacement markers must never be
    // matched again, even if a credential is itself a substring of the marker.
    let Ok(matcher) = aho_corasick::AhoCorasick::builder()
        .match_kind(aho_corasick::MatchKind::LeftmostLongest)
        .build(&values)
    else {
        return "Diagnostic unavailable: credential redaction could not be prepared".into();
    };
    let mut end = 0;
    for found in matcher.find_iter(text) {
        if !append(&text[end..found.start()]) || !append("[redacted]") {
            return output;
        }
        end = found.end();
    }
    append(&text[end..]);
    output
}

#[derive(Clone, Debug)]
pub(crate) struct OAuthStore {
    pub(crate) dir: PathBuf,
    epoch: String,
}
fn active_epoch(dir: &Path) -> Result<String, String> {
    let epoch = read(&dir.join("generation"))?.unwrap_or_else(|| "initial".into());
    if epoch != "initial" && (epoch.len() != 32 || !epoch.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err("Invalid private OAuth credential generation".into());
    }
    Ok(epoch)
}
impl OAuthStore {
    pub(crate) fn of(bound: &super::config::Bound) -> Result<Self, String> {
        let dir = bound.private.join(format!("oauth-{}", bound.binding()));
        let epoch = active_epoch(&dir)?;
        Ok(Self { dir, epoch })
    }
    fn path(&self) -> PathBuf {
        self.dir.join(format!("tokens-{}.json", self.epoch))
    }
    pub(crate) fn exists(&self) -> bool {
        self.path().is_file()
    }
    pub(crate) fn forget(&self) -> Result<(), String> {
        use ring::rand::SecureRandom as _;
        directory(&self.dir)?;
        // Revocation must not wait behind a remote token refresh. The
        // generation checks in save invalidate even an already-running exchange.
        let _guard = crate::settings::file::lock(
            self.dir
                .parent()
                .ok_or("OAuth store has no private directory")?,
        )?;
        let mut bytes = [0; 16];
        ring::rand::SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| "Cannot revoke OAuth credential generation")?;
        let epoch = super::config::hex(&bytes);
        write(&self.dir.join("generation"), &epoch)?;
        // A late token exchange can only write its old generation; readers
        // never use it, and save removes it if revocation raced the write.
        for entry in std::fs::read_dir(&self.dir)
            .map_err(|_| "Cannot list private OAuth credentials")?
            .take(256)
            .filter_map(Result::ok)
        {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("tokens-") && name.ends_with(".json") {
                std::fs::remove_file(entry.path())
                    .map_err(|_| "Cannot remove the saved OAuth login")?;
            }
        }
        Ok(())
    }
}
fn auth_error(_: impl std::fmt::Display) -> AuthError {
    AuthError::CredentialStoreError(
        "Private MCP credential storage failed or was revoked; check ownership and sign in again"
            .into(),
    )
}

#[async_trait::async_trait]
impl CredentialStore for OAuthStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            if active_epoch(&store.dir).map_err(auth_error)? != store.epoch {
                return Ok(None);
            }
            read(&store.path())
                .map_err(auth_error)?
                .map(|text| serde_json::from_str(&text).map_err(auth_error))
                .transpose()
        })
        .await
        .map_err(auth_error)?
    }
    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            if active_epoch(&store.dir).map_err(auth_error)? != store.epoch {
                return Err(auth_error("revoked"));
            }
            let path = store.path();
            write(
                &path,
                &serde_json::to_string(&credentials).map_err(auth_error)?,
            )
            .map_err(auth_error)?;
            if active_epoch(&store.dir).map_err(auth_error)? != store.epoch {
                let _ = std::fs::remove_file(path);
                return Err(auth_error("revoked"));
            }
            Ok(())
        })
        .await
        .map_err(auth_error)?
    }
    async fn clear(&self) -> Result<(), AuthError> {
        let path = self.path();
        tokio::task::spawn_blocking(move || match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(auth_error(e)),
        })
        .await
        .map_err(auth_error)?
    }
    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || {
            directory(&dir).map_err(auth_error)?;
            crate::settings::file::lock(&dir)
                .map(|lock| Some(CredentialRefreshGuard::new(lock)))
                .map_err(auth_error)
        })
        .await
        .map_err(auth_error)?
    }
}
