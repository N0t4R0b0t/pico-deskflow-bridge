//! The settings page's optional password.
//!
//! The password is stored as PBKDF2-HMAC-SHA256 with a random salt. Logging
//! in sets a session cookie holding a random nonce and its HMAC under a key
//! kept with the password, so sessions survive the restarts that saving
//! settings causes, and changing or removing the password ends them all.
//!
//! [`Hasher`] works in steps so the firmware can let other tasks run while
//! a password is hashed.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::settings::FieldError;
use crate::web::{Method, form_value};

pub const SALT_LEN: usize = 16;
pub const HASH_LEN: usize = 32;
pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 16;
const TAG_LEN: usize = 16;
/// A token in the cookie: nonce and tag, hex.
pub const TOKEN_HEX_LEN: usize = 2 * (NONCE_LEN + TAG_LEN);
pub const PASSWORD_MIN: usize = 4;
pub const PASSWORD_MAX: usize = 64;
pub const COOKIE: &str = "bridge_session";

type HmacSha256 = Hmac<Sha256>;

/// PBKDF2-HMAC-SHA256 with a 32-byte output (a single block), run in steps.
pub struct Hasher {
    mac: HmacSha256,
    u: [u8; HASH_LEN],
    t: [u8; HASH_LEN],
    left: u32,
}

impl Hasher {
    pub fn new(password: &[u8], salt: &[u8; SALT_LEN], rounds: u32) -> Self {
        let mac = HmacSha256::new_from_slice(password).expect("HMAC takes any key length");
        let mut first = mac.clone();
        first.update(salt);
        first.update(&1u32.to_be_bytes());
        let u: [u8; HASH_LEN] = first.finalize().into_bytes().into();
        Hasher { mac, u, t: u, left: rounds.max(1) - 1 }
    }

    /// Runs up to `rounds` more iterations; the hash once all are done.
    pub fn step(&mut self, rounds: u32) -> Option<[u8; HASH_LEN]> {
        for _ in 0..rounds.min(self.left) {
            let mut m = self.mac.clone();
            m.update(&self.u);
            self.u = m.finalize().into_bytes().into();
            for (t, u) in self.t.iter_mut().zip(self.u) {
                *t ^= u;
            }
            self.left -= 1;
        }
        (self.left == 0).then_some(self.t)
    }

    /// The whole hash at once.
    pub fn hash(password: &[u8], salt: &[u8; SALT_LEN], rounds: u32) -> [u8; HASH_LEN] {
        Hasher::new(password, salt, rounds).step(u32::MAX).unwrap()
    }
}

/// A password as stored in flash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Password {
    pub rounds: u32,
    pub salt: [u8; SALT_LEN],
    pub hash: [u8; HASH_LEN],
    /// Signs session tokens; new with every password.
    pub session_key: [u8; KEY_LEN],
    /// Whether viewing the page (status and log) needs the password too, not
    /// only changing things.
    pub protect_view: bool,
}

const RECORD_VERSION: u8 = 1;
/// Version, flags, rounds, salt, hash, session key.
pub const RECORD_LEN: usize = 1 + 1 + 4 + SALT_LEN + HASH_LEN + KEY_LEN;

impl Password {
    /// `random` provides the salt and the session key; `hash` is the
    /// password's [`Hasher`] output with that salt and `rounds`.
    pub fn new(
        random: [u8; SALT_LEN + KEY_LEN],
        rounds: u32,
        hash: impl FnOnce(&[u8; SALT_LEN]) -> [u8; HASH_LEN],
        protect_view: bool,
    ) -> Self {
        let mut salt = [0; SALT_LEN];
        salt.copy_from_slice(&random[..SALT_LEN]);
        let mut session_key = [0; KEY_LEN];
        session_key.copy_from_slice(&random[SALT_LEN..]);
        Password { rounds, hash: hash(&salt), salt, session_key, protect_view }
    }

    /// Whether `hash` (of a typed password, with this salt and rounds) matches.
    pub fn matches(&self, hash: &[u8; HASH_LEN]) -> bool {
        ct_eq(hash, &self.hash)
    }

    pub fn to_bytes(&self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[0] = RECORD_VERSION;
        out[1] = self.protect_view as u8;
        out[2..6].copy_from_slice(&self.rounds.to_le_bytes());
        out[6..22].copy_from_slice(&self.salt);
        out[22..54].copy_from_slice(&self.hash);
        out[54..].copy_from_slice(&self.session_key);
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != RECORD_LEN || b[0] != RECORD_VERSION {
            return None;
        }
        Some(Password {
            protect_view: b[1] & 1 != 0,
            rounds: u32::from_le_bytes(b[2..6].try_into().ok()?),
            salt: b[6..22].try_into().ok()?,
            hash: b[22..54].try_into().ok()?,
            session_key: b[54..].try_into().ok()?,
        })
    }

    /// A new session token (for the cookie) from a random nonce.
    pub fn token(&self, nonce: [u8; NONCE_LEN]) -> heapless::String<TOKEN_HEX_LEN> {
        let mut out = heapless::String::new();
        for b in nonce.iter().chain(&self.tag(&nonce)) {
            let _ = core::fmt::write(&mut out, format_args!("{:02x}", b));
        }
        out
    }

    /// Whether a token from a cookie is one of ours.
    pub fn token_valid(&self, token: &str) -> bool {
        let mut raw = [0u8; NONCE_LEN + TAG_LEN];
        if !decode_hex(token, &mut raw) {
            return false;
        }
        let (nonce, tag) = raw.split_at(NONCE_LEN);
        ct_eq(&self.tag(nonce), tag)
    }

    fn tag(&self, nonce: &[u8]) -> [u8; TAG_LEN] {
        let mut mac = HmacSha256::new_from_slice(&self.session_key).expect("HMAC takes any key length");
        mac.update(b"session");
        mac.update(nonce);
        let full = mac.finalize().into_bytes();
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&full[..TAG_LEN]);
        tag
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn decode_hex(s: &str, out: &mut [u8]) -> bool {
    let s = s.as_bytes();
    if s.len() != 2 * out.len() {
        return false;
    }
    for (o, pair) in out.iter_mut().zip(s.chunks(2)) {
        let digit = |c: u8| (c as char).to_digit(16);
        match (digit(pair[0]), digit(pair[1])) {
            (Some(hi), Some(lo)) => *o = (hi << 4 | lo) as u8,
            _ => return false,
        }
    }
    true
}

/// The session token in a `Cookie` header, if any.
pub fn cookie_token(cookie: Option<&str>) -> Option<&str> {
    cookie?.split(';').find_map(|c| c.trim().strip_prefix(COOKIE)?.strip_prefix('='))
}

/// What a request may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Allowed,
    /// Answer 401: the page asks for the password.
    LoginRequired,
}

/// Whether a request may go ahead. `password` is the stored one, `None` if
/// the page is open; `logged_in` whether the request carried a valid token.
pub fn access(method: Method, path: &str, password: Option<&Password>, logged_in: bool) -> Access {
    let Some(password) = password else { return Access::Allowed };
    // The page itself (it asks for the password) and logging in are always
    // reachable; viewing is too unless protected; everything else needs a
    // session.
    let open = match (method, path) {
        (Method::Get, "/") | (Method::Post, "/api/login") | (Method::Post, "/api/logout") => true,
        (Method::Get, "/api/state" | "/api/log" | "/api/panel") => !password.protect_view,
        _ => false,
    };
    if open || logged_in { Access::Allowed } else { Access::LoginRequired }
}

/// A submitted change of password (`/api/password`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordForm {
    /// Must match the current password, if there is one.
    pub current: heapless::String<PASSWORD_MAX>,
    pub change: PasswordChange,
    pub protect_view: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordChange {
    Keep,
    Set(heapless::String<PASSWORD_MAX>),
    Remove,
}

impl PasswordForm {
    /// Fields: `current`, `new` (empty keeps the password), `remove=1`,
    /// `protect_view=1`.
    pub fn parse(body: &[u8]) -> Result<Self, FieldError> {
        let mut buf = [0u8; PASSWORD_MAX];
        let text = |key: &'static str, buf: &mut [u8; PASSWORD_MAX]| -> Result<heapless::String<PASSWORD_MAX>, FieldError> {
            match form_value(body, key, buf) {
                None => Ok(heapless::String::new()),
                Some(Some(v)) => Ok(heapless::String::try_from(v).unwrap()),
                Some(None) => Err(FieldError { field: key, message: "at most 64 bytes" }),
            }
        };
        let current = text("current", &mut buf)?;
        let new = text("new", &mut buf)?;
        let flag = |key: &str| matches!(form_value(body, key, &mut [0u8; 4]), Some(Some("1" | "on")));
        let change = if flag("remove") {
            PasswordChange::Remove
        } else if new.is_empty() {
            PasswordChange::Keep
        } else if new.chars().count() < PASSWORD_MIN {
            return Err(FieldError { field: "new", message: "at least 4 characters" });
        } else {
            PasswordChange::Set(new)
        };
        Ok(PasswordForm { current, change, protect_view: flag("protect_view") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> std::string::String {
        b.iter().map(|b| format!("{:02x}", b)).collect()
    }

    #[test]
    fn pbkdf2_known_answers() {
        // The commonly published PBKDF2-HMAC-SHA256 vectors check the
        // reference; the reference checks the stepped hasher.
        let salt = |s: &[u8]| -> [u8; SALT_LEN] {
            let mut out = [0; SALT_LEN];
            out[..s.len()].copy_from_slice(s);
            out
        };
        assert_eq!(
            hex(&Hasher::hash(b"password", &salt(b"saltSALTsaltSALT"), 4096)),
            hex(&reference(b"password", b"saltSALTsaltSALT", 4096))
        );
        assert_eq!(
            hex(&reference(b"password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex(&reference(b"password", b"salt", 4096)),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
    }

    /// PBKDF2 with an arbitrary salt, written out plainly, to check
    /// [`Hasher`] against.
    fn reference(password: &[u8], salt: &[u8], rounds: u32) -> [u8; HASH_LEN] {
        let mut m = HmacSha256::new_from_slice(password).unwrap();
        m.update(salt);
        m.update(&[0, 0, 0, 1]);
        let mut u: [u8; 32] = m.finalize().into_bytes().into();
        let mut t = u;
        for _ in 1..rounds {
            let mut m = HmacSha256::new_from_slice(password).unwrap();
            m.update(&u);
            u = m.finalize().into_bytes().into();
            t.iter_mut().zip(u).for_each(|(t, u)| *t ^= u);
        }
        t
    }

    #[test]
    fn stepping_gives_the_same_hash() {
        let salt = [9; SALT_LEN];
        let mut h = Hasher::new(b"hunter2", &salt, 1000);
        let mut steps = 0;
        let out = loop {
            steps += 1;
            if let Some(out) = h.step(64) {
                break out;
            }
        };
        assert_eq!(steps, 16);
        assert_eq!(out, Hasher::hash(b"hunter2", &salt, 1000));
        assert_eq!(out, reference(b"hunter2", &salt, 1000));
    }

    fn password(protect_view: bool) -> Password {
        Password::new([7; SALT_LEN + KEY_LEN], 100, |salt| Hasher::hash(b"secret", salt, 100), protect_view)
    }

    #[test]
    fn record_round_trips() {
        let p = password(true);
        assert!(p.matches(&Hasher::hash(b"secret", &p.salt, p.rounds)));
        assert!(!p.matches(&Hasher::hash(b"Secret", &p.salt, p.rounds)));
        assert_eq!(Password::from_bytes(&p.to_bytes()), Some(p));
        assert_eq!(Password::from_bytes(&p.to_bytes()[1..]), None);
        let mut future = p.to_bytes();
        future[0] = 2;
        assert_eq!(Password::from_bytes(&future), None);
    }

    #[test]
    fn session_tokens() {
        let p = password(false);
        let token = p.token([1; NONCE_LEN]);
        assert_eq!(token.len(), TOKEN_HEX_LEN);
        assert!(p.token_valid(&token));
        // Tampered, truncated, from another password, garbage.
        let mut bad = std::string::String::from(token.as_str());
        bad.replace_range(40..41, if &token[40..41] == "0" { "1" } else { "0" });
        assert!(!p.token_valid(&bad));
        assert!(!p.token_valid(&token[..TOKEN_HEX_LEN - 2]));
        let other = Password { session_key: [8; KEY_LEN], ..p };
        assert!(!other.token_valid(&token));
        assert!(!p.token_valid(&"zz".repeat(TOKEN_HEX_LEN / 2)));
    }

    #[test]
    fn cookie_parsing() {
        assert_eq!(cookie_token(Some("bridge_session=abc")), Some("abc"));
        assert_eq!(cookie_token(Some("a=1; bridge_session=abc; b=2")), Some("abc"));
        assert_eq!(cookie_token(Some("bridge_sessionx=abc")), None);
        assert_eq!(cookie_token(Some("a=1")), None);
        assert_eq!(cookie_token(None), None);
    }

    #[test]
    fn access_rules() {
        use Access::*;
        use Method::*;
        assert_eq!(access(Post, "/api/settings", None, false), Allowed);
        let open_view = password(false);
        let closed_view = password(true);
        for p in [&open_view, &closed_view] {
            assert_eq!(access(Get, "/", Some(p), false), Allowed);
            assert_eq!(access(Post, "/api/login", Some(p), false), Allowed);
            assert_eq!(access(Post, "/api/settings", Some(p), false), LoginRequired);
            assert_eq!(access(Post, "/api/restart", Some(p), false), LoginRequired);
            assert_eq!(access(Post, "/api/password", Some(p), false), LoginRequired);
            assert_eq!(access(Post, "/api/settings", Some(p), true), Allowed);
            assert_eq!(access(Get, "/api/state", Some(p), true), Allowed);
        }
        assert_eq!(access(Get, "/api/state", Some(&open_view), false), Allowed);
        assert_eq!(access(Get, "/api/log", Some(&open_view), false), Allowed);
        assert_eq!(access(Get, "/api/state", Some(&closed_view), false), LoginRequired);
        assert_eq!(access(Get, "/api/log", Some(&closed_view), false), LoginRequired);
        assert_eq!(access(Get, "/api/panel", Some(&open_view), false), Allowed);
        assert_eq!(access(Get, "/api/panel", Some(&closed_view), false), LoginRequired);
        assert_eq!(access(Post, "/api/panel/press", Some(&open_view), false), LoginRequired);
    }

    #[test]
    fn password_form() {
        let f = PasswordForm::parse(b"current=&new=hunter2&protect_view=1").unwrap();
        assert_eq!(f.change, PasswordChange::Set(heapless::String::try_from("hunter2").unwrap()));
        assert!(f.protect_view);
        let f = PasswordForm::parse(b"current=hunter2&new=").unwrap();
        assert_eq!((f.current.as_str(), &f.change, f.protect_view), ("hunter2", &PasswordChange::Keep, false));
        let f = PasswordForm::parse(b"current=hunter2&new=whatever&remove=1").unwrap();
        assert_eq!(f.change, PasswordChange::Remove);
        assert_eq!(PasswordForm::parse(b"new=abc").unwrap_err().field, "new");
        // Four characters, not bytes.
        assert!(PasswordForm::parse("new=%C3%A9%C3%A9%C3%A9%C3%A9".as_bytes()).is_ok());
        let long = format!("new={}", "a".repeat(65));
        assert_eq!(PasswordForm::parse(long.as_bytes()).unwrap_err().field, "new");
    }
}
