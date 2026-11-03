//! Moonlight pairing, client side (REMOTE_DESKTOP.md, 2.4).
//!
//! Pairing is not in moonlight-common-c -- every Moonlight client implements
//! it -- so this is that handshake, against Sunshine's `/pair` endpoint:
//!
//! 1. `getservercert` with our certificate and a salt. Sunshine holds this
//!    request open until it has the PIN; ours arrives on Sunshine's stdin
//!    through the SSH channel (its `-0` flag), written *after* the request is
//!    sent -- one written before it is ignored.
//! 2. `clientchallenge`: a random challenge, AES-128-ECB under
//!    `SHA-256(salt ‖ PIN)[..16]`.
//! 3. `serverchallengeresp`: our answer to the server's challenge, proving we
//!    hold the PIN; Sunshine answers with its secret and an RSA signature.
//!    Checking that signature against the certificate from step 1, and the
//!    server's hash against its secret, is what catches a wrong PIN or
//!    someone in the middle.
//! 4. `clientpairingsecret`: our secret, signed with our key.
//! 5. `pairchallenge` over HTTPS with our certificate as the client
//!    certificate, the server's pinned to the one from step 1.
//!
//! The identity -- certificate, key, unique id -- is made fresh for every
//! launch and lives only in memory: PulseTerm's Sunshine runs with `-1`, so it
//! forgets the pairing on exit anyway, and nothing secret touches the disk.
//! Both ports are reached through SSH forwards, so pairing works even where a
//! firewall would block Sunshine's own ports.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use openssl::asn1::Asn1Time;
use openssl::bn::{BigNum, MsbOption};
use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Private};
use openssl::rand::rand_bytes;
use openssl::rsa::Rsa;
use openssl::sha::sha256;
use openssl::sign::{Signer, Verifier};
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};
use openssl::symm::{Cipher, Crypter, Mode};
use openssl::x509::{X509NameBuilder, X509};

/// Who we are to Sunshine for this launch. Kept for the stream window, which
/// must present the same certificate.
#[derive(Clone)]
pub struct Identity {
    pub unique_id: String,
    pub cert: X509,
    pub key: PKey<Private>,
}

impl Identity {
    pub fn new() -> Result<Self, String> {
        let e = |e: openssl::error::ErrorStack| e.to_string();
        let key = PKey::from_rsa(Rsa::generate(2048).map_err(e)?).map_err(e)?;
        let mut name = X509NameBuilder::new().map_err(e)?;
        // The name every GameStream client uses; Sunshine does not read it.
        name.append_entry_by_text("CN", "NVIDIA GameStream Client").map_err(e)?;
        let name = name.build();
        let mut serial = BigNum::new().map_err(e)?;
        serial.rand(64, MsbOption::MAYBE_ZERO, false).map_err(e)?;
        let mut cert = X509::builder().map_err(e)?;
        cert.set_version(2).map_err(e)?;
        let serial = serial.to_asn1_integer().map_err(e)?;
        cert.set_serial_number(&serial).map_err(e)?;
        cert.set_subject_name(&name).map_err(e)?;
        cert.set_issuer_name(&name).map_err(e)?;
        cert.set_pubkey(&key).map_err(e)?;
        let (from, until) = (Asn1Time::days_from_now(0).map_err(e)?, Asn1Time::days_from_now(3650).map_err(e)?);
        cert.set_not_before(&from).map_err(e)?;
        cert.set_not_after(&until).map_err(e)?;
        cert.sign(&key, MessageDigest::sha256()).map_err(e)?;
        Ok(Self { unique_id: hex(&random::<8>()?), cert: cert.build(), key })
    }
}

/// Where Sunshine's two pairing ports are reachable from here -- local ends
/// of SSH forwards (or the ports themselves in `--local` mode).
#[derive(Clone, Copy)]
pub struct Endpoints {
    pub http: u16,
    pub https: u16,
}

/// A 4-digit PIN. Nobody types it: it goes to Sunshine over SSH.
pub fn new_pin() -> Result<String, String> {
    let n = u16::from_le_bytes(random::<2>()?) % 10000;
    Ok(format!("{n:04}"))
}

/// Runs the whole handshake and returns Sunshine's certificate, pinned for
/// everything after. Blocking -- call it on a thread of its own.
/// `request_sent` fires once the first request is on the wire, which is the
/// moment to hand Sunshine the PIN.
pub fn pair(
    id: &Identity,
    at: &Endpoints,
    pin: &str,
    request_sent: impl FnOnce(),
) -> Result<X509, String> {
    let e = |e: openssl::error::ErrorStack| e.to_string();
    let uuid = hex(&random::<16>()?);
    let common = format!("uniqueid={}&uuid={uuid}&devicename=PulseTerm&updateState=1", id.unique_id);
    let client_pem = id.cert.to_pem().map_err(e)?;

    // 1. Our certificate out, the server's back -- once Sunshine has the PIN.
    let salt = random::<16>()?;
    let body = http_get(
        at.http,
        &format!("/pair?{common}&phrase=getservercert&salt={}&clientcert={}", hex(&salt), hex(&client_pem)),
        Some(request_sent),
    )?;
    expect_paired(&body, "getservercert")?;
    let server_cert = X509::from_pem(&unhex(&tag(&body, "plaincert").ok_or("no server certificate")?)?)
        .map_err(|e| format!("bad server certificate: {e}"))?;
    let key: [u8; 16] = {
        let mut seed = salt.to_vec();
        seed.extend_from_slice(pin.as_bytes());
        sha256(&seed)[..16].try_into().unwrap()
    };

    // 2. Our challenge.
    let client_challenge = random::<16>()?;
    let body = http_get(
        at.http,
        &format!("/pair?{common}&clientchallenge={}", hex(&aes(&key, Mode::Encrypt, &client_challenge)?)),
        None::<fn()>,
    )?;
    expect_paired(&body, "clientchallenge")?;
    let decrypted = aes(&key, Mode::Decrypt, &unhex(&tag(&body, "challengeresponse").ok_or("no challenge response")?)?)?;
    if decrypted.len() < 48 {
        return Err("short challenge response".into());
    }
    let (server_response, server_challenge) = (&decrypted[..32], &decrypted[32..48]);

    // 3. Our answer to theirs; their secret back.
    let client_secret = random::<16>()?;
    let mut answer = server_challenge.to_vec();
    answer.extend_from_slice(id.cert.signature().as_slice());
    answer.extend_from_slice(&client_secret);
    let body = http_get(
        at.http,
        &format!("/pair?{common}&serverchallengeresp={}", hex(&aes(&key, Mode::Encrypt, &sha256(&answer))?)),
        None::<fn()>,
    )?;
    expect_paired(&body, "serverchallengeresp")?;
    let secret = unhex(&tag(&body, "pairingsecret").ok_or("no pairing secret")?)?;
    if secret.len() <= 16 {
        return Err("short pairing secret".into());
    }
    let (server_secret, signature) = secret.split_at(16);
    let server_key = server_cert.public_key().map_err(e)?;
    let mut verifier = Verifier::new(MessageDigest::sha256(), &server_key).map_err(e)?;
    verifier.update(server_secret).map_err(e)?;
    if !verifier.verify(signature).map_err(e)? {
        return Err("the server's signature does not match its certificate -- someone in the middle?".into());
    }
    let mut expected = client_challenge.to_vec();
    expected.extend_from_slice(server_cert.signature().as_slice());
    expected.extend_from_slice(server_secret);
    if sha256(&expected)[..] != *server_response {
        return Err("the server did not prove it has the PIN -- wrong PIN, or someone in the middle".into());
    }

    // 4. Our secret, signed.
    let mut signer = Signer::new(MessageDigest::sha256(), &id.key).map_err(e)?;
    signer.update(&client_secret).map_err(e)?;
    let mut ours = client_secret.to_vec();
    ours.extend_from_slice(&signer.sign_to_vec().map_err(e)?);
    let body = http_get(at.http, &format!("/pair?{common}&clientpairingsecret={}", hex(&ours)), None::<fn()>)?;
    expect_paired(&body, "clientpairingsecret")?;

    // 5. Over TLS with our certificate: the server now knows it.
    let body = https_get(at.https, &format!("/pair?{common}&phrase=pairchallenge"), id, &server_cert)?;
    expect_paired(&body, "pairchallenge")?;
    Ok(server_cert)
}

/// Asks Sunshine over TLS, with our certificate, whether it considers us
/// paired -- the check that pairing really took.
pub fn is_paired(id: &Identity, at: &Endpoints, server_cert: &X509) -> Result<bool, String> {
    let uuid = hex(&random::<16>()?);
    let path = format!("/serverinfo?uniqueid={}&uuid={uuid}", id.unique_id);
    let body = https_get(at.https, &path, id, server_cert)?;
    Ok(tag(&body, "PairStatus").as_deref() == Some("1"))
}

// --- after pairing: what the host offers, and starting a session -----------

/// What `/serverinfo` says, as moonlight-common-c needs it.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub app_version: String,
    pub gfe_version: String,
    pub codec_support: i32,
    /// The app already running on the host (0: none). Closing the stream
    /// window leaves it running, so the next Connect resumes it.
    pub current_game: String,
}

pub fn server_info(id: &Identity, at: &Endpoints, server_cert: &X509) -> Result<ServerInfo, String> {
    let uuid = hex(&random::<16>()?);
    let body = https_get(at.https, &format!("/serverinfo?uniqueid={}&uuid={uuid}", id.unique_id), id, server_cert)?;
    Ok(ServerInfo {
        app_version: tag(&body, "appversion").ok_or("no appversion in serverinfo")?,
        gfe_version: tag(&body, "GfeVersion").unwrap_or_default(),
        codec_support: tag(&body, "ServerCodecModeSupport").and_then(|v| v.trim().parse().ok()).unwrap_or(1),
        current_game: tag(&body, "currentgame").unwrap_or_else(|| "0".into()),
    })
}

/// The host's apps as `(id, title)`. Sunshine's default list has "Desktop".
pub fn app_list(id: &Identity, at: &Endpoints, server_cert: &X509) -> Result<Vec<(String, String)>, String> {
    let uuid = hex(&random::<16>()?);
    let body = https_get(at.https, &format!("/applist?uniqueid={}&uuid={uuid}", id.unique_id), id, server_cert)?;
    Ok(body
        .split("<App>")
        .skip(1)
        .filter_map(|app| Some((tag(app, "ID")?, tag(app, "AppTitle")?)))
        .collect())
}

/// The key moonlight-common-c encrypts input with; sent to the host in the
/// launch request and to the stream process over its stdin -- never on a
/// command line, where `ps` would show it.
#[derive(Clone)]
pub struct InputKey {
    pub key: [u8; 16],
    pub key_id: i32,
}

impl InputKey {
    pub fn new() -> Result<Self, String> {
        Ok(Self { key: random::<16>()?, key_id: i32::from_le_bytes(random::<4>()?) & 0x7fff_ffff })
    }
    pub fn hex(&self) -> String {
        hex(&self.key)
    }
}

/// Starts `app_id` at `width`x`height`@`fps` -- or, with `resume`, rejoins
/// the app already running -- and returns the RTSP session URL. `extra` is
/// moonlight-common-c's `LiGetLaunchUrlQueryParameters()`.
#[allow(clippy::too_many_arguments)]
pub fn launch(
    id: &Identity,
    at: &Endpoints,
    server_cert: &X509,
    app_id: &str,
    (width, height, fps): (u32, u32, u32),
    input: &InputKey,
    audio_info: u32,
    extra: &str,
    resume: bool,
    // Keep the host's own sound playing (1), or let Sunshine take it (0).
    play_on_host: bool,
) -> Result<String, String> {
    let uuid = hex(&random::<16>()?);
    let verb = if resume { "resume" } else { "launch" };
    let path = format!(
        "/{verb}?uniqueid={}&uuid={uuid}&appid={app_id}&mode={width}x{height}x{fps}&additionalStates=1&sops=0\
         &rikey={}&rikeyid={}&localAudioPlayMode={}&surroundAudioInfo={audio_info}&remoteControllersBitmap=0&gcmap=0{extra}",
        id.unique_id,
        input.hex(),
        input.key_id,
        play_on_host as u8,
    );
    let body = https_get(at.https, &path, id, server_cert)?;
    if !body.contains("status_code=\"200\"") {
        return Err(format!("{verb} refused: {}", attr(&body, "status_message").unwrap_or_default()));
    }
    tag(&body, "sessionUrl0").ok_or_else(|| "Sunshine sent no session URL".into())
}

fn expect_paired(body: &str, step: &str) -> Result<(), String> {
    if !body.contains("status_code=\"200\"") {
        return Err(format!("{step}: Sunshine refused ({})", attr(body, "status_message").unwrap_or_default()));
    }
    match tag(body, "paired").as_deref() {
        Some("1") => Ok(()),
        _ => Err(format!("{step}: not paired")),
    }
}

// --- crypto and encoding ---------------------------------------------------

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut out = [0u8; N];
    rand_bytes(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// AES-128-ECB with no padding: every value this protocol encrypts is already
/// a whole number of blocks.
fn aes(key: &[u8; 16], mode: Mode, data: &[u8]) -> Result<Vec<u8>, String> {
    let e = |e: openssl::error::ErrorStack| e.to_string();
    let cipher = Cipher::aes_128_ecb();
    let mut crypter = Crypter::new(cipher, mode, key, None).map_err(e)?;
    crypter.pad(false);
    let mut out = vec![0u8; data.len() + cipher.block_size()];
    let mut n = crypter.update(data, &mut out).map_err(e)?;
    n += crypter.finalize(&mut out[n..]).map_err(e)?;
    out.truncate(n);
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return Err("odd-length hex".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| "bad hex".to_string()))
        .collect()
}

/// The text of `<name>…</name>` -- the replies are flat XML and this is all
/// of it that is needed.
fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(xml[start..end].to_string())
}

fn attr(xml: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = xml.find(&key)? + key.len();
    let end = xml[start..].find('"')? + start;
    Some(xml[start..end].to_string())
}

// --- HTTP, just enough ------------------------------------------------------

/// One `GET` on a fresh connection to `127.0.0.1:port`. The first pairing
/// request is held open by Sunshine until it has the PIN, hence the long read
/// timeout, and hence `sent` -- called once the request is written.
fn http_get(port: u16, path: &str, sent: Option<impl FnOnce()>) -> Result<String, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("cannot reach Sunshine: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    write_request(&mut stream, port, path)?;
    if let Some(sent) = sent {
        sent();
    }
    read_response(&mut stream)
}

fn https_get(port: u16, path: &str, id: &Identity, server_cert: &X509) -> Result<String, String> {
    let e = |e: openssl::error::ErrorStack| e.to_string();
    let mut builder = SslConnector::builder(SslMethod::tls_client()).map_err(e)?;
    builder.set_certificate(&id.cert).map_err(e)?;
    builder.set_private_key(&id.key).map_err(e)?;
    // Sunshine's certificate is self-signed; it is pinned below instead,
    // against the one it sent in step 1.
    builder.set_verify(SslVerifyMode::NONE);
    let connector = builder.build();
    let tcp = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("cannot reach Sunshine (TLS): {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let mut config = connector.configure().map_err(e)?;
    config.set_verify_hostname(false);
    config.set_use_server_name_indication(false);
    let mut tls = config.connect("localhost", tcp).map_err(|e| format!("TLS handshake failed: {e}"))?;
    let peer = tls.ssl().peer_certificate().ok_or("Sunshine sent no certificate")?;
    if peer.to_der().map_err(e)? != server_cert.to_der().map_err(e)? {
        return Err("Sunshine's TLS certificate is not the one it paired with".into());
    }
    write_request(&mut tls, port, path)?;
    read_response(&mut tls)
}

fn write_request(stream: &mut impl Write, port: u16, path: &str) -> Result<(), String> {
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).and_then(|_| stream.flush()).map_err(|e| e.to_string())
}

fn read_response(stream: &mut impl Read) -> Result<String, String> {
    let mut raw = Vec::new();
    // A TLS peer that closes without close_notify reports an error after the
    // data; whatever arrived is still the answer.
    if let Err(err) = stream.read_to_end(&mut raw) {
        if raw.is_empty() {
            return Err(format!("no answer from Sunshine: {err}"));
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").ok_or("malformed HTTP answer")?;
    if !head.lines().next().unwrap_or("").contains(" 200") {
        return Err(format!("Sunshine answered {}", head.lines().next().unwrap_or("?")));
    }
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        return Ok(dechunk(body));
    }
    Ok(body.to_string())
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else { break };
        if n == 0 || after.len() < n {
            break;
        }
        out.push_str(&after[..n]);
        rest = after[n..].trim_start_matches("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 0xab, 0xff];
        assert_eq!(hex(&bytes), "0001abff");
        assert_eq!(unhex("0001ABff").unwrap(), bytes);
        assert!(unhex("abc").is_err());
    }

    #[test]
    fn aes_ecb_round_trips_without_padding() {
        let key = [7u8; 16];
        let data = [1u8; 32];
        let enc = aes(&key, Mode::Encrypt, &data).unwrap();
        assert_eq!(enc.len(), 32, "no padding block added");
        assert_eq!(aes(&key, Mode::Decrypt, &enc).unwrap(), data);
    }

    #[test]
    fn reads_the_flat_xml() {
        let xml = r#"<?xml version="1.0"?><root status_code="200"><paired>1</paired><plaincert>4142</plaincert></root>"#;
        assert_eq!(tag(xml, "paired").as_deref(), Some("1"));
        assert_eq!(tag(xml, "plaincert").as_deref(), Some("4142"));
        assert!(expect_paired(xml, "x").is_ok());
        assert!(expect_paired(r#"<root status_code="400" status_message="nope"></root>"#, "x").is_err());
    }

    #[test]
    fn identity_and_pin_look_right() {
        let id = Identity::new().unwrap();
        assert_eq!(id.unique_id.len(), 16);
        assert!(id.cert.public_key().unwrap().public_eq(&id.key));
        let pin = new_pin().unwrap();
        assert_eq!(pin.len(), 4);
        assert!(pin.chars().all(|c| c.is_ascii_digit()));
    }

    /// The whole handshake against a real Sunshine on this machine, started
    /// exactly as the launcher starts it: the right PIN pairs (and Sunshine
    /// confirms it over TLS), a wrong one is refused. Opt-in, because it needs
    /// Sunshine installed: `cargo test -p pulseterm-remote -- --ignored`.
    #[test]
    #[ignore]
    fn pairs_with_the_right_pin_and_refuses_a_wrong_one() {
        use crate::bridge::Bridge;
        use futures::executor::block_on;

        let attempt = |wrong: bool| -> Result<bool, String> {
            let bridge = Bridge::local();
            let (instance, _displays) = block_on(crate::sunshine::start(
                &bridge,
                crate::sunshine::DEFAULT_PORT,
                Some("kms"),
                &crate::sunshine::Tuning::from(&crate::config::StreamConfig::default()),
            ))?;
            let at = Endpoints { http: instance.port, https: instance.port - 5 };
            let id = Identity::new()?;
            let pin = new_pin()?;
            let typed = if wrong { format!("{:04}", (pin.parse::<u16>().unwrap() + 1) % 10000) } else { pin.clone() };
            let result = pair(&id, &at, &pin, || {
                std::thread::sleep(Duration::from_millis(300));
                let _ = block_on(bridge.write(instance.handle, &format!("{typed}\n")));
            })
            .and_then(|cert| is_paired(&id, &at, &cert));
            // No pause before the next attempt: `stop` itself waits until the
            // instance is gone -- the bug a user hit (stop, then Connect).
            block_on(crate::sunshine::stop(&bridge, instance));
            result
        };

        assert_eq!(attempt(false), Ok(true), "the right PIN pairs and Sunshine confirms it");
        let refused = attempt(true);
        assert!(refused.is_err(), "a wrong PIN must not pair: {refused:?}");
    }

    #[test]
    fn reads_the_app_list() {
        let xml = "<root status_code=\"200\"><App><IsHdrSupported>0</IsHdrSupported><AppTitle>Desktop</AppTitle><ID>881448767</ID></App>\
                   <App><AppTitle>Steam Big Picture</AppTitle><ID>1093255277</ID></App></root>";
        let apps: Vec<(String, String)> = xml
            .split("<App>")
            .skip(1)
            .filter_map(|app| Some((tag(app, "ID")?, tag(app, "AppTitle")?)))
            .collect();
        assert_eq!(apps[0], ("881448767".into(), "Desktop".into()));
        assert_eq!(apps.len(), 2);
    }

    #[test]
    fn chunked_bodies_are_joined() {
        assert_eq!(dechunk("3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"), "abcde");
    }
}
