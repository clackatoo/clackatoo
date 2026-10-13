//! Notifications sent to devices through their browsers' push services,
//! as Web Push does it: each one sealed for its device alone (RFC 8291),
//! and signed with the daemon's own key (VAPID, RFC 8292), so a push
//! service takes it from this daemon and no other.
//!
//! The daemon makes its key the first time it's needed and keeps it in
//! the web app's directory, as `vapid.p8`, which only the user may read.
//! Sending takes a round trip to the push service, so it's done on a
//! thread of its own; a push service saying a device's subscription is
//! gone (404 or 410) is heard back, and the subscription dropped.
//!
//! What a notification says is a chat's number and a line about it, such
//! as its title and that it finished: never what the agent wrote, or any
//! code. A push service can't read it anyway, but a phone's lock screen
//! can be.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use ring::{aead, agreement, error, hkdf};

/// The key's file in the web app's directory.
const KEY_FILE: &str = "vapid.p8";

/// The record size a notification's content says: it's one record,
/// smaller than this.
const RECORD_SIZE: u32 = 4096;

/// The most a notification may say, sealed: what every push service
/// takes.
const MOST: usize = 3000;

/// Who to tell if notifications go wrong, as push services ask a sender
/// to say.
const CONTACT: &str = "https://github.com/clackatoo/clackatoo";

/// The push services notifications may go to, by the ends of their
/// hosts: browsers' own, so a device can't have the daemon send requests
/// anywhere else.
const PUSH_SERVICES: &[&str] = &[
    "push.apple.com",
    "fcm.googleapis.com",
    "push.services.mozilla.com",
    "notify.windows.com",
];

/// The daemon's key for signing notifications.
#[derive(Debug)]
pub(crate) struct Vapid {
    key: EcdsaKeyPair,
}

impl Vapid {
    /// The key kept in `dir`, made and kept there if there isn't one.
    ///
    /// # Errors
    ///
    /// If it can't be read or made.
    pub(crate) fn open(dir: &Path, random: &SystemRandom) -> Result<Self, String> {
        let path = dir.join(KEY_FILE);
        let pkcs8 = match fs::read(&path) {
            Ok(pkcs8) => pkcs8,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, random)
                    .map_err(|_| "a key for notifications couldn't be made".to_owned())?;
                let mut file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(&path)
                    .and_then(|mut file| file.write_all(pkcs8.as_ref()).map(|()| file))
                    .map_err(|err| format!("{} couldn't be written: {err}", path.display()))?;
                let _ = file.flush();
                pkcs8.as_ref().to_vec()
            }
            Err(err) => return Err(format!("{} couldn't be read: {err}", path.display())),
        };
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, random)
            .map_err(|_| format!("{} isn't a key for notifications", path.display()))?;
        Ok(Self { key })
    }

    /// The public half, in base64url, as a browser's `applicationServerKey`.
    pub(crate) fn public_key(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.key.public_key())
    }

    /// The `Authorization` header for a notification to `endpoint`, signed
    /// at `now` seconds since the Unix epoch, good for 12 hours.
    fn authorization(
        &self,
        endpoint: &str,
        now: u64,
        random: &dyn SecureRandom,
    ) -> Result<String, String> {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let audience = audience(endpoint).ok_or("not a push service's address")?;
        let header = b64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({
            "aud": audience,
            "exp": now + 12 * 60 * 60,
            "sub": CONTACT,
        });
        let claims = b64.encode(claims.to_string());
        let signed = format!("{header}.{claims}");
        let signature = (self.key.sign(random, signed.as_bytes()))
            .map_err(|_| "the notification couldn't be signed".to_owned())?;
        Ok(format!(
            "vapid t={signed}.{}, k={}",
            b64.encode(signature),
            self.public_key()
        ))
    }
}

/// The origin of `endpoint`, the audience its signature names, if it's
/// a push service's address: HTTPS, at a host of one of
/// [`PUSH_SERVICES`].
pub(crate) fn audience(endpoint: &str) -> Option<&str> {
    let rest = endpoint.strip_prefix("https://")?;
    let end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.split(':').next()?;
    let known = PUSH_SERVICES.iter().any(|service| {
        host == *service
            || host
                .strip_suffix(service)
                .is_some_and(|start| start.ends_with('.'))
    });
    let plain = (host.bytes()).all(|byte| byte.is_ascii_alphanumeric() || b".-".contains(&byte));
    (known && plain && endpoint.len() <= 2048).then(|| &endpoint[..8 + end])
}

/// Seals `plaintext` for the browser whose public key is `ua_public` and
/// secret `auth_secret`, as RFC 8291 says: a key of the daemon's own made
/// for this notification alone, then a salt, both from `random`, and the
/// whole sealed as one record of `aes128gcm` content (RFC 8188).
///
/// # Errors
///
/// If the browser's key isn't a P-256 point, or the notification is too
/// long.
pub(crate) fn encrypt(
    plaintext: &[u8],
    ua_public: &[u8],
    auth_secret: &[u8],
    random: &dyn SecureRandom,
) -> Result<Vec<u8>, error::Unspecified> {
    if plaintext.len() > MOST {
        return Err(error::Unspecified);
    }
    let as_private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, random)?;
    let as_public = as_private.compute_public_key()?;
    let mut salt = [0; 16];
    random.fill(&mut salt)?;
    let ua_key = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    let ikm = agreement::agree_ephemeral(as_private, &ua_key, |ecdh_secret| {
        let key_info = [b"WebPush: info\0", ua_public, as_public.as_ref()];
        let prk_key = hkdf::Salt::new(hkdf::HKDF_SHA256, auth_secret).extract(ecdh_secret);
        expand::<32>(&prk_key, &key_info)
    })??;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &salt).extract(&ikm);
    let cek = expand::<16>(&prk, &[b"Content-Encoding: aes128gcm\0"])?;
    let nonce = expand::<12>(&prk, &[b"Content-Encoding: nonce\0"])?;

    let mut message = Vec::with_capacity(86 + plaintext.len() + 17);
    message.extend_from_slice(&salt);
    message.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    message.push(u8::try_from(as_public.as_ref().len()).map_err(|_| error::Unspecified)?);
    message.extend_from_slice(as_public.as_ref());
    let header = message.len();
    message.extend_from_slice(plaintext);
    // The last record ends with 2, and no padding.
    message.push(2);
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek)?);
    let mut record = message.split_off(header);
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut record,
    )?;
    message.extend_from_slice(&record);
    Ok(message)
}

/// `N` bytes of HKDF-Expand from `prk` with `info`.
fn expand<const N: usize>(prk: &hkdf::Prk, info: &[&[u8]]) -> Result<[u8; N], error::Unspecified> {
    struct Len(usize);
    impl hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let mut out = [0; N];
    prk.expand(info, Len(N))?.fill(&mut out)?;
    Ok(out)
}

/// A notification for one device.
#[derive(Debug)]
pub(crate) struct Push {
    /// The device.
    pub(crate) device: u64,
    pub(crate) subscription: super::devices::Subscription,
    /// What it says, as JSON, for the app's service worker.
    pub(crate) payload: Vec<u8>,
    /// Whether it's urgent: a question waits for the user.
    pub(crate) urgent: bool,
    /// What it's about, so a newer one about the same takes its place
    /// while it waits at the push service.
    pub(crate) topic: String,
}

/// A device whose subscription a push service says is gone.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Gone {
    pub(crate) device: u64,
    pub(crate) endpoint: String,
}

/// The thread sending notifications.
#[derive(Debug)]
pub(crate) struct Pusher {
    jobs: Sender<Push>,
    gone: Receiver<Gone>,
}

impl Pusher {
    /// Starts the thread, signing with `vapid`, which calls `wake` when a
    /// push service says a subscription is gone.
    pub(crate) fn start(vapid: Arc<Vapid>, wake: impl Fn() + Send + 'static) -> Self {
        let (jobs, inbox) = mpsc::channel::<Push>();
        let (tell, gone) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("push".to_owned())
            .spawn(move || {
                let config = ureq::Agent::config_builder()
                    .user_agent(concat!("clackatoo/", env!("CARGO_PKG_VERSION")))
                    .timeout_connect(Some(Duration::from_secs(10)))
                    .timeout_global(Some(Duration::from_secs(20)))
                    .build();
                let agent = ureq::Agent::new_with_config(config);
                let random = SystemRandom::new();
                for push in inbox {
                    if let Some(gone) = send(&agent, &vapid, &random, &push) {
                        let _ = tell.send(gone);
                        wake();
                    }
                }
            });
        if let Err(err) = spawned {
            log::error!("the thread sending notifications couldn't start: {err}");
        }
        Self { jobs, gone }
    }

    /// Sends `push`, on the thread.
    pub(crate) fn send(&self, push: Push) {
        let _ = self.jobs.send(push);
    }

    /// The subscriptions push services have said are gone, since last
    /// asked.
    pub(crate) fn gone(&self) -> Vec<Gone> {
        self.gone.try_iter().collect()
    }
}

/// Sends `push`, and says if its subscription is gone.
fn send(agent: &ureq::Agent, vapid: &Vapid, random: &SystemRandom, push: &Push) -> Option<Gone> {
    let subscription = &push.subscription;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let sealed = encrypt(
        &push.payload,
        &subscription.p256dh,
        &subscription.auth,
        random,
    );
    let (Ok(body), Ok(authorization)) = (
        sealed,
        vapid.authorization(&subscription.endpoint, now, random),
    ) else {
        log::warn!("a notification for device {} couldn't be made", push.device);
        return None;
    };
    let sent = agent
        .post(&subscription.endpoint)
        .header("TTL", "86400")
        .header("Urgency", if push.urgent { "high" } else { "normal" })
        .header("Topic", &push.topic)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("Authorization", &authorization)
        .send(&body[..]);
    match sent {
        Ok(_) => {
            log::debug!("sent a notification to device {}", push.device);
            None
        }
        Err(ureq::Error::StatusCode(404 | 410)) => Some(Gone {
            device: push.device,
            endpoint: subscription.endpoint.clone(),
        }),
        Err(err) => {
            log::warn!("a notification to device {} failed: {err}", push.device);
            None
        }
    }
}

/// What the app's service worker is sent: the chat it's about, and what
/// to show.
pub(crate) fn payload(chat: u32, title: &str, body: &str) -> Vec<u8> {
    let cut = |text: &str, most: usize| -> String {
        let mut cut: String = text.chars().take(most).collect();
        if cut.len() < text.len() {
            cut.push('…');
        }
        cut
    };
    serde_json::json!({
        "chat": chat,
        "title": cut(title, 80),
        "body": cut(body, 120),
    })
    .to_string()
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(text: &str) -> Vec<u8> {
        let text: String = text.split_whitespace().collect();
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text)
            .unwrap()
    }

    /// RFC 8291, section 5 and appendix A.
    #[test]
    #[allow(
        deprecated,
        reason = "ring's fixed randomness is the only way to give it the RFC's key and salt"
    )]
    fn notifications_are_sealed_as_rfc_8291_says() {
        let plaintext = b64("V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24");
        assert_eq!(plaintext, b"When I grow up, I want to be a watermelon");
        let as_private = b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw");
        let as_public = b64("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIg
                             Dll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let ua_public = b64("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcx
                             aOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let salt = b64("DGv6ra1nlYgDCS1FRnbzlw");
        let auth_secret = b64("BTBZMqHH6r4Tts7J_aSIgg");
        let expected = b64(
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
             mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT
             pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN",
        );
        // The key ring makes from the RFC's private key is the RFC's.
        {
            let random = ring::test::rand::FixedSliceRandom { bytes: &as_private };
            let key =
                agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &random).unwrap();
            assert_eq!(key.compute_public_key().unwrap().as_ref(), &as_public[..]);
        }
        let random = ring::test::rand::FixedSliceSequenceRandom {
            bytes: &[&as_private, &salt],
            current: core::cell::UnsafeCell::new(0),
        };
        let sealed = encrypt(&plaintext, &ua_public, &auth_secret, &random).unwrap();
        // The header: the salt, the record size, and the key.
        assert_eq!(&sealed[..16], &salt[..]);
        assert_eq!(&sealed[16..21], &[0, 0, 16, 0, 65]);
        assert_eq!(&sealed[21..86], &as_public[..]);
        // 86 for the header, 42 for the text and its delimiter, 16 for the
        // tag: the RFC's message is these 144 bytes, though its example's
        // Content-Length says 145.
        assert_eq!(sealed.len(), 144);
        assert_eq!(sealed, expected);
    }

    /// Opens what [`encrypt`] sealed, as a browser does, with its private
    /// key: the other half of RFC 8291's sums.
    #[test]
    fn a_browser_can_open_what_is_sealed() {
        let random = SystemRandom::new();
        let ua_private =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &random).unwrap();
        let ua_public = ua_private.compute_public_key().unwrap();
        let auth_secret = [7; 16];
        let sealed = encrypt(b"{\"chat\":3}", ua_public.as_ref(), &auth_secret, &random).unwrap();
        let (salt, as_public) = (&sealed[..16], &sealed[21..86]);
        let as_key = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public);
        let ikm = agreement::agree_ephemeral(ua_private, &as_key, |ecdh_secret| {
            let key_info = [b"WebPush: info\0", ua_public.as_ref(), as_public];
            let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &auth_secret).extract(ecdh_secret);
            expand::<32>(&prk, &key_info)
        })
        .unwrap()
        .unwrap();
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
        let cek = expand::<16>(&prk, &[b"Content-Encoding: aes128gcm\0"]).unwrap();
        let nonce = expand::<12>(&prk, &[b"Content-Encoding: nonce\0"]).unwrap();
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
        let mut record = sealed[86..].to_vec();
        let opened = key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut record,
            )
            .unwrap();
        assert_eq!(opened, b"{\"chat\":3}\x02");
    }

    #[test]
    fn notifications_go_only_to_browsers_push_services() {
        assert_eq!(
            audience("https://web.push.apple.com/QGuQyavXutnMH"),
            Some("https://web.push.apple.com")
        );
        assert_eq!(
            audience("https://fcm.googleapis.com/fcm/send/abc"),
            Some("https://fcm.googleapis.com")
        );
        assert_eq!(
            audience("https://updates.push.services.mozilla.com/wpush/v2/x"),
            Some("https://updates.push.services.mozilla.com")
        );
        for refused in [
            "http://web.push.apple.com/x",
            "https://evilpush.apple.com.example/x",
            "https://notpush.apple.com/x",
            "https://169.254.169.254/latest",
            "https://localhost:8080/x",
            "https://user@web.push.apple.com/x",
        ] {
            assert_eq!(audience(refused), None, "{refused}");
        }
    }

    #[test]
    fn a_signature_names_the_push_service_and_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let random = SystemRandom::new();
        let vapid = Vapid::open(dir.path(), &random).unwrap();
        let again = Vapid::open(dir.path(), &random).unwrap();
        assert_eq!(vapid.public_key(), again.public_key(), "kept on disk");
        let header = vapid
            .authorization("https://web.push.apple.com/x", 1000, &random)
            .unwrap();
        let (token, key) = header
            .strip_prefix("vapid t=")
            .and_then(|rest| rest.split_once(", k="))
            .unwrap();
        assert_eq!(key, vapid.public_key());
        let parts: Vec<&str> = token.split('.').collect();
        let claims: serde_json::Value = serde_json::from_slice(&b64(parts[1])).unwrap();
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert_eq!(claims["exp"], 1000 + 12 * 60 * 60);
        let public = ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_FIXED,
            b64(key),
        );
        let signed = format!("{}.{}", parts[0], parts[1]);
        assert!(public.verify(signed.as_bytes(), &b64(parts[2])).is_ok());
    }

    #[test]
    fn a_payload_says_little() {
        let payload = payload(4, &"t".repeat(200), "finished");
        let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(value["chat"], 4);
        assert_eq!(value["title"].as_str().unwrap().chars().count(), 81);
        assert_eq!(value["body"], "finished");
    }
}
