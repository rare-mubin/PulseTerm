use std::sync::{Arc, Mutex as StdMutex};

use russh::client;
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKey};
use russh::Disconnect;
use russh_sftp::client::SftpSession;

use super::known_hosts::{self, HostKeyCheck};
use super::sftp::Sftp;
use crate::runtime;

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyStatus {
    Pending,
    Ok,
    Mismatch,
}

/// Implements russh's client callbacks. The only one we care about is host key
/// verification, which we run through the TOFU store.
pub struct SshHandler {
    host_id: String,
    key_status: Arc<StdMutex<KeyStatus>>,
}

impl client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_public_key: &PublicKey) -> Result<bool, Self::Error> {
        let result = known_hosts::check_and_store(&self.host_id, server_public_key);
        let mut status = self.key_status.lock().unwrap();
        match result {
            HostKeyCheck::New | HostKeyCheck::Known => {
                *status = KeyStatus::Ok;
                Ok(true)
            }
            HostKeyCheck::Mismatch => {
                *status = KeyStatus::Mismatch;
                Ok(false)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum AuthMethod {
    Password(String),
    PrivateKey { path: String, passphrase: Option<String> },
}

#[derive(Clone, Debug)]
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
}

impl ConnectParams {
    pub fn host_id(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// One live SSH connection. Cloning is cheap and shares the same underlying
/// session, so every terminal opened from it multiplexes channels over a
/// single TCP connection rather than dialling again per tab.
#[derive(Clone)]
pub struct Connection {
    pub label: String,
    /// The file-browsing half of the same connection. Opened at connect time so
    /// the explorer never has to negotiate anything of its own.
    pub sftp: Sftp,
    pub(super) handle: Arc<client::Handle<SshHandler>>,
}

/// Dials, verifies the host key, and authenticates. Safe to await from the
/// gpui executor -- the work itself happens on the SSH runtime.
pub async fn connect(params: ConnectParams) -> Result<Connection, String> {
    runtime::spawn(async move { connect_inner(params).await }).await
}

async fn connect_inner(params: ConnectParams) -> Result<Connection, String> {
    let host_id = params.host_id();
    let key_status = Arc::new(StdMutex::new(KeyStatus::Pending));
    let handler = SshHandler {
        host_id: host_id.clone(),
        key_status: key_status.clone(),
    };

    let config = Arc::new(client::Config::default());

    let mut handle = client::connect(config, (params.host.as_str(), params.port), handler)
        .await
        .map_err(|e| {
            if *key_status.lock().unwrap() == KeyStatus::Mismatch {
                format!(
                    "REMOTE HOST IDENTIFICATION HAS CHANGED for {host_id}. This could mean someone \
                     is intercepting the connection. Refusing to connect. If you trust this change, \
                     remove the old entry from PulseTerm's known_hosts.json."
                )
            } else {
                format!("Could not connect to {host_id}: {e}")
            }
        })?;

    let authenticated = match params.auth.clone() {
        AuthMethod::Password(password) => handle
            .authenticate_password(&params.username, &password)
            .await
            .map_err(|e| e.to_string())?
            .success(),
        AuthMethod::PrivateKey { path, passphrase } => {
            let key = load_secret_key(&path, passphrase.as_deref())
                .map_err(|e| format!("Failed to load private key: {e}"))?;
            let key = Arc::new(key);
            let hash_alg = handle
                .best_supported_rsa_hash()
                .await
                .map_err(|e| e.to_string())?
                .flatten();
            handle
                .authenticate_publickey(&params.username, PrivateKeyWithHashAlg::new(key, hash_alg))
                .await
                .map_err(|e| e.to_string())?
                .success()
        }
    };

    if !authenticated {
        return Err("Authentication failed".to_string());
    }

    // SFTP is a subsystem channel on the connection we already have -- no
    // second dial, no second authentication.
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("Failed to open SFTP channel: {e}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("Failed to start the SFTP subsystem: {e}"))?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("SFTP handshake failed: {e}"))?;

    Ok(Connection {
        label: format!("{}@{}", params.username, host_id),
        sftp: Sftp::new(sftp),
        handle: Arc::new(handle),
    })
}

impl Connection {
    pub fn disconnect(&self) {
        let handle = self.handle.clone();
        runtime::detach(async move {
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        });
    }

    /// Disconnects and waits for the goodbye to actually go out.
    ///
    /// `disconnect` above is fire-and-forget, which is right while the app
    /// keeps running -- the task finishes on the SSH runtime in its own time.
    /// A caller that is about to *exit* would race process teardown against it,
    /// and the server would see a dropped socket instead of a clean close.
    pub async fn disconnect_and_wait(&self) {
        let handle = self.handle.clone();
        runtime::spawn(async move {
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        })
        .await;
    }
}
