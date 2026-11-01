//! TLS for the Deskflow connection, as an [`embedded_tls::CryptoProvider`].
//!
//! Deskflow servers use a self-signed RSA certificate (2048-bit by default)
//! and are trusted by the SHA-256 fingerprint of that certificate, which the
//! server GUI shows. With the default "check peer fingerprints" setting the
//! server also asks for a client certificate and trusts *it* by fingerprint,
//! so the client presents a self-signed P-256 certificate of its own
//! ([`ClientIdentity`]).
//!
//! The server is authenticated by pinning ([`ServerTrust::Pinned`]) and by
//! checking its TLS 1.3 `CertificateVerify` signature against the key in the
//! certificate, which proves it holds the private key rather than a copy of
//! the certificate.

use core::fmt;

use embedded_tls::{
    Aes128GcmSha256, Certificate, CertificateEntryRef, CertificateRef, CertificateVerifyRef, CryptoProvider,
    CryptoRngCore, SignatureScheme, TlsError, TlsVerifier,
};
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{DerSignature, Signature, SigningKey};
use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey as _;
use rsa::signature::Verifier as _;
use sha2::{Digest as _, Sha256, Sha384, Sha512};

/// SHA-256 of a certificate's DER encoding.
pub type Fingerprint = [u8; 32];

pub fn fingerprint(cert_der: &[u8]) -> Fingerprint {
    Sha256::digest(cert_der).into()
}

/// Parses a fingerprint as Deskflow displays it (`AB:CD:…`), with or without
/// the colons and in either case. Panics on malformed input, so a bad
/// fingerprint in a `const` is a compile error.
pub const fn parse_fingerprint(s: &str) -> Fingerprint {
    match try_parse_fingerprint(s) {
        Some(fp) => fp,
        None => panic!("fingerprint: expected 32 hex bytes"),
    }
}

/// [`parse_fingerprint`] for untrusted input.
pub const fn try_parse_fingerprint(s: &str) -> Option<Fingerprint> {
    const fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let s = s.as_bytes();
    let mut out = [0u8; 32];
    let mut n = 0;
    let mut i = 0;
    while i < s.len() {
        if s[i] == b':' {
            i += 1;
            continue;
        }
        if n == 32 || i + 1 >= s.len() {
            return None;
        }
        let (Some(hi), Some(lo)) = (nibble(s[i]), nibble(s[i + 1])) else { return None };
        out[n] = hi << 4 | lo;
        n += 1;
        i += 2;
    }
    if n == 32 { Some(out) } else { None }
}

/// Displays a fingerprint the way the Deskflow GUI does: `AB:CD:…`.
pub struct DisplayFingerprint<'a>(pub &'a Fingerprint);

impl fmt::Display for DisplayFingerprint<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{:02X}", b)?;
        }
        Ok(())
    }
}

/// Displays a fingerprint as a line of Deskflow's `trusted-clients` /
/// `trusted-servers` files: `v2:sha256:abcd…`.
pub struct DbLine<'a>(pub &'a Fingerprint);

impl fmt::Display for DbLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("v2:sha256:")?;
        for b in self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerTrust {
    /// Accept the certificate with this fingerprint only.
    Pinned(Fingerprint),
    /// Accept any certificate (still checking that the server holds its key).
    /// Encrypted, but not authenticated.
    Any,
}

/// Why the handshake failed, beyond what [`TlsError`] can say.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rejection {
    /// The server's certificate isn't the pinned one.
    FingerprintMismatch,
    /// The certificate isn't an RSA key we can parse (or is over 4096 bits).
    UnsupportedKey,
    /// The server offered a signature scheme other than RSA-PSS.
    UnsupportedScheme(SignatureScheme),
    BadSignature,
}

/// Verifies the server: pinned fingerprint plus `CertificateVerify`.
pub struct Verifier {
    trust: ServerTrust,
    server: Option<Fingerprint>,
    key: Option<RsaPublicKey>,
    transcript: Option<Sha256>,
    rejection: Option<Rejection>,
}

impl Verifier {
    fn new(trust: ServerTrust) -> Self {
        Self { trust, server: None, key: None, transcript: None, rejection: None }
    }

    fn reject(&mut self, why: Rejection) -> TlsError {
        self.rejection = Some(why);
        match why {
            Rejection::BadSignature => TlsError::InvalidSignature,
            Rejection::UnsupportedScheme(_) => TlsError::InvalidSignatureScheme,
            _ => TlsError::InvalidCertificate,
        }
    }
}

impl TlsVerifier<Aes128GcmSha256> for Verifier {
    fn set_hostname_verification(&mut self, _hostname: &str) -> Result<(), TlsError> {
        // Deskflow certificates carry no meaningful names; trust is by fingerprint.
        Ok(())
    }

    fn verify_certificate(&mut self, transcript: &Sha256, cert: CertificateRef) -> Result<(), TlsError> {
        let Some(CertificateEntryRef::X509(leaf)) = cert.entries.first() else {
            return Err(self.reject(Rejection::UnsupportedKey));
        };
        let seen = fingerprint(leaf);
        self.server = Some(seen);
        if let ServerTrust::Pinned(pinned) = self.trust
            && pinned != seen
        {
            return Err(self.reject(Rejection::FingerprintMismatch));
        }
        let key = subject_public_key_info(leaf).and_then(|spki| RsaPublicKey::from_public_key_der(spki).ok());
        let Some(key) = key else {
            return Err(self.reject(Rejection::UnsupportedKey));
        };
        self.key = Some(key);
        self.transcript = Some(transcript.clone());
        Ok(())
    }

    fn verify_signature(&mut self, verify: CertificateVerifyRef) -> Result<(), TlsError> {
        let (Some(key), Some(transcript)) = (self.key.take(), self.transcript.take()) else {
            return Err(TlsError::InvalidHandshake);
        };
        let message = certificate_verify_message(&transcript.finalize().into());
        match verify_rsa_pss(key, verify.signature_scheme, &message, verify.signature) {
            Ok(()) => Ok(()),
            Err(why) => Err(self.reject(why)),
        }
    }
}

/// What the server signs in TLS 1.3 (RFC 8446 §4.4.3): 64 spaces, a context
/// string, a zero byte, then the handshake transcript hash.
fn certificate_verify_message(transcript_hash: &[u8; 32]) -> [u8; 130] {
    const CONTEXT: &[u8; 34] = b"TLS 1.3, server CertificateVerify\0";
    let mut msg = [0x20u8; 130];
    msg[64..98].copy_from_slice(CONTEXT);
    msg[98..].copy_from_slice(transcript_hash);
    msg
}

/// TLS 1.3 only allows PSS for RSA, with the salt as long as the hash.
fn verify_rsa_pss(key: RsaPublicKey, scheme: SignatureScheme, message: &[u8], signature: &[u8]) -> Result<(), Rejection> {
    use rsa::pss::{Signature, VerifyingKey};
    let signature = Signature::try_from(signature).map_err(|_| Rejection::BadSignature)?;
    let ok = match scheme {
        SignatureScheme::RsaPssRsaeSha256 => VerifyingKey::<Sha256>::new(key).verify(message, &signature),
        SignatureScheme::RsaPssRsaeSha384 => VerifyingKey::<Sha384>::new(key).verify(message, &signature),
        SignatureScheme::RsaPssRsaeSha512 => VerifyingKey::<Sha512>::new(key).verify(message, &signature),
        other => return Err(Rejection::UnsupportedScheme(other)),
    };
    ok.map_err(|_| Rejection::BadSignature)
}

/// The client's key pair and the self-signed certificate it presents.
pub struct ClientIdentity {
    key: SigningKey,
    cert: heapless::Vec<u8, CERT_MAX>,
}

const CERT_MAX: usize = 512;

impl ClientIdentity {
    /// Builds the identity from a stored P-256 secret scalar. The certificate
    /// is deterministic (RFC 6979 signature, fixed validity), so the
    /// fingerprint the server trusts stays the same across reboots as long
    /// as the secret and `common_name` do. `None` if the secret is not a
    /// valid scalar (zero or ≥ the curve order) or the name is too long.
    pub fn new(secret: &[u8; 32], common_name: &str) -> Option<Self> {
        let key = SigningKey::from_bytes(secret.into()).ok()?;
        let cert = self_signed_certificate(&key, common_name)?;
        Some(Self { key, cert })
    }

    pub fn certificate(&self) -> &[u8] {
        &self.cert
    }

    pub fn fingerprint(&self) -> Fingerprint {
        fingerprint(&self.cert)
    }
}

/// Generates a fresh P-256 secret scalar for [`ClientIdentity::new`]. Cheap:
/// no curve arithmetic, unlike building the identity from it.
pub fn generate_secret(rng: &mut impl CryptoRngCore) -> [u8; 32] {
    p256::NonZeroScalar::random(rng).to_bytes().into()
}

/// The [`CryptoProvider`] for a Deskflow connection.
pub struct Provider<'a, R> {
    rng: R,
    verifier: Verifier,
    client: Option<&'a ClientIdentity>,
}

impl<'a, R: CryptoRngCore> Provider<'a, R> {
    /// `client` is presented if the server asks for a certificate.
    pub fn new(rng: R, trust: ServerTrust, client: Option<&'a ClientIdentity>) -> Self {
        Self { rng, verifier: Verifier::new(trust), client }
    }

    /// Fingerprint of the certificate the server presented, once it has.
    pub fn server_fingerprint(&self) -> Option<Fingerprint> {
        self.verifier.server
    }

    /// Set when the handshake failed because of the server's certificate.
    pub fn rejection(&self) -> Option<Rejection> {
        self.verifier.rejection
    }
}

impl<R: CryptoRngCore> CryptoProvider for Provider<'_, R> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = DerSignature;

    fn rng(&mut self) -> impl CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Aes128GcmSha256>, TlsError> {
        Ok(&mut self.verifier)
    }

    fn signer(&mut self) -> Result<(impl rsa::signature::SignerMut<DerSignature>, SignatureScheme), TlsError> {
        let client = self.client.ok_or(TlsError::InvalidPrivateKey)?;
        Ok((client.key.clone(), SignatureScheme::EcdsaSecp256r1Sha256))
    }

    fn client_cert(&mut self) -> Option<Certificate<impl AsRef<[u8]>>> {
        self.client.map(|c| Certificate::X509(c.certificate()))
    }
}

// --- DER ---

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const BIT_STRING: u8 = 0x03;
const OID: u8 = 0x06;
const UTF8_STRING: u8 = 0x0c;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const VERSION_TAG: u8 = 0xa0;

/// Splits one DER TLV off the front of `input`: (tag, whole TLV, rest).
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 3 || rest.len() < n {
            return None;
        }
        let len = rest[..n].iter().fold(0usize, |acc, &b| acc << 8 | b as usize);
        (len, &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    let header = input.len() - rest.len();
    Some((tag, &input[..header + len], &rest[len..]))
}

/// The content of a constructed TLV with the expected tag.
fn contents(tlv_bytes: &[u8]) -> &[u8] {
    let header = if tlv_bytes[1] < 0x80 { 2 } else { 2 + (tlv_bytes[1] & 0x7f) as usize };
    &tlv_bytes[header..]
}

/// The `SubjectPublicKeyInfo` TLV of an X.509 certificate.
fn subject_public_key_info(cert: &[u8]) -> Option<&[u8]> {
    let (SEQUENCE, cert, _) = tlv(cert)? else { return None };
    let (SEQUENCE, tbs, _) = tlv(contents(cert))? else { return None };
    let mut rest = contents(tbs);
    if rest.first() == Some(&VERSION_TAG) {
        rest = tlv(rest)?.2;
    }
    // serialNumber, signature, issuer, validity, subject
    for _ in 0..5 {
        rest = tlv(rest)?.2;
    }
    let (SEQUENCE, spki, _) = tlv(rest)? else { return None };
    Some(spki)
}

/// Appends a TLV with definite-length encoding.
fn put(out: &mut heapless::Vec<u8, CERT_MAX>, tag: u8, content: &[u8]) -> Option<()> {
    out.push(tag).ok()?;
    let len = content.len();
    match len {
        0..0x80 => out.push(len as u8).ok()?,
        0x80..0x100 => out.extend_from_slice(&[0x81, len as u8]).ok()?,
        _ => out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]).ok()?,
    }
    out.extend_from_slice(content).ok()
}

/// Same as `put`, with the content built by `f`.
fn put_with(
    out: &mut heapless::Vec<u8, CERT_MAX>,
    tag: u8,
    f: impl FnOnce(&mut heapless::Vec<u8, CERT_MAX>) -> Option<()>,
) -> Option<()> {
    let mut content = heapless::Vec::new();
    f(&mut content)?;
    put(out, tag, &content)
}

/// 1.2.840.10045.4.3.2
const ECDSA_WITH_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
/// 1.2.840.10045.2.1
const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
/// 1.2.840.10045.3.1.7
const PRIME256V1: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
/// 2.5.4.3
const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];

/// A minimal X.509 v1 certificate (no extensions), self-signed with
/// ECDSA-SHA256. Deskflow doesn't validate the chain, names or dates, only
/// the fingerprint, but OpenSSL still has to parse it.
fn self_signed_certificate(key: &SigningKey, common_name: &str) -> Option<heapless::Vec<u8, CERT_MAX>> {
    if common_name.len() > 64 {
        return None;
    }
    let point = key.verifying_key().to_encoded_point(false);
    let point = point.as_bytes();

    // Positive and with no leading zero, so it's a minimal DER INTEGER as-is.
    let mut serial: [u8; 16] = Sha256::digest(point)[..16].try_into().unwrap();
    serial[0] = serial[0] & 0x7f | 0x40;

    let algorithm = |out: &mut heapless::Vec<u8, CERT_MAX>| put_with(out, SEQUENCE, |s| put(s, OID, ECDSA_WITH_SHA256));
    let name = |out: &mut heapless::Vec<u8, CERT_MAX>| {
        put_with(out, SEQUENCE, |rdns| {
            put_with(rdns, SET, |rdn| {
                put_with(rdn, SEQUENCE, |atv| {
                    put(atv, OID, COMMON_NAME)?;
                    put(atv, UTF8_STRING, common_name.as_bytes())
                })
            })
        })
    };

    let mut tbs = heapless::Vec::new();
    put_with(&mut tbs, SEQUENCE, |t| {
        put(t, INTEGER, &serial)?;
        algorithm(t)?;
        name(t)?;
        put_with(t, SEQUENCE, |v| {
            put(v, UTC_TIME, b"250101000000Z")?;
            // RFC 5280's "no well-defined expiration date".
            put(v, GENERALIZED_TIME, b"99991231235959Z")
        })?;
        name(t)?;
        put_with(t, SEQUENCE, |spki| {
            put_with(spki, SEQUENCE, |alg| {
                put(alg, OID, EC_PUBLIC_KEY)?;
                put(alg, OID, PRIME256V1)
            })?;
            put_with(spki, BIT_STRING, |bits| {
                bits.push(0).ok()?;
                bits.extend_from_slice(point).ok()
            })
        })
    })?;

    let signature: Signature = key.sign(&tbs);
    let signature = signature.to_der();

    let mut cert = heapless::Vec::new();
    put_with(&mut cert, SEQUENCE, |c| {
        c.extend_from_slice(&tbs).ok()?;
        algorithm(c)?;
        put_with(c, BIT_STRING, |bits| {
            bits.push(0).ok()?;
            bits.extend_from_slice(signature.as_bytes()).ok()
        })
    })?;
    Some(cert)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_deskflow_fingerprint_format() {
        let text = "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff";
        let fp = parse_fingerprint(text);
        assert_eq!(fp[0], 0x00);
        assert_eq!(fp[10], 0xaa);
        assert_eq!(fp[31], 0xff);
        assert_eq!(parse_fingerprint(&text.replace(':', "")), fp);
        assert_eq!(alloc::format!("{}", DisplayFingerprint(&fp)), text.to_uppercase());
        assert_eq!(alloc::format!("{}", DbLine(&fp)), alloc::format!("v2:sha256:{}", text.replace(':', "").to_lowercase()));
    }

    #[test]
    #[should_panic]
    fn rejects_short_fingerprint() {
        parse_fingerprint("00:11");
    }

    #[test]
    fn try_parse_rejects_garbage() {
        assert_eq!(try_parse_fingerprint(""), None);
        assert_eq!(try_parse_fingerprint(&"zz".repeat(32)), None);
        assert_eq!(try_parse_fingerprint(&"ab".repeat(33)), None);
        assert_eq!(try_parse_fingerprint(&"ab".repeat(31)), None);
        assert_eq!(try_parse_fingerprint(&("ab".repeat(31) + "a")), None);
        assert_eq!(try_parse_fingerprint(&"ab".repeat(32)), Some([0xab; 32]));
    }

    #[test]
    fn certificate_verify_message_layout() {
        let msg = certificate_verify_message(&[7; 32]);
        assert!(msg[..64].iter().all(|&b| b == b' '));
        assert_eq!(&msg[64..98], b"TLS 1.3, server CertificateVerify\0");
        assert_eq!(&msg[98..], &[7; 32]);
    }

    #[test]
    fn client_certificate_is_stable_and_self_consistent() {
        let secret = [0x42; 32];
        let a = ClientIdentity::new(&secret, "arch-laptop").unwrap();
        let b = ClientIdentity::new(&secret, "arch-laptop").unwrap();
        assert_eq!(a.certificate(), b.certificate());
        assert_ne!(a.fingerprint(), ClientIdentity::new(&[0x43; 32], "arch-laptop").unwrap().fingerprint());

        // The SPKI we wrote is the one we read back, and it holds our key.
        let spki = subject_public_key_info(a.certificate()).unwrap();
        let key = p256::PublicKey::from_public_key_der(spki).unwrap();
        assert_eq!(key, p256::PublicKey::from(a.key.verifying_key()));
    }

    #[test]
    fn rsa_pss_signature_must_match() {
        use rsa::signature::{RandomizedSigner as _, SignatureEncoding as _};
        let mut rng = rand_core::OsRng;
        let private = rsa::RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let message = certificate_verify_message(&[1; 32]);
        let signature = rsa::pss::SigningKey::<Sha256>::new(private.clone()).sign_with_rng(&mut rng, &message).to_vec();
        let public = || private.to_public_key();

        assert_eq!(verify_rsa_pss(public(), SignatureScheme::RsaPssRsaeSha256, &message, &signature), Ok(()));
        let other = certificate_verify_message(&[2; 32]);
        assert_eq!(verify_rsa_pss(public(), SignatureScheme::RsaPssRsaeSha256, &other, &signature), Err(Rejection::BadSignature));
        assert_eq!(verify_rsa_pss(public(), SignatureScheme::RsaPssRsaeSha384, &message, &signature), Err(Rejection::BadSignature));
        assert_eq!(
            verify_rsa_pss(public(), SignatureScheme::RsaPkcs1Sha256, &message, &signature),
            Err(Rejection::UnsupportedScheme(SignatureScheme::RsaPkcs1Sha256))
        );
    }

    #[test]
    fn rejects_invalid_secret() {
        assert!(ClientIdentity::new(&[0; 32], "x").is_none());
        assert!(ClientIdentity::new(&[0xff; 32], "x").is_none());
    }

    #[test]
    fn der_length_forms() {
        let mut out = heapless::Vec::<u8, CERT_MAX>::new();
        put(&mut out, OID, &[0; 0x7f]).unwrap();
        assert_eq!(&out[..2], &[OID, 0x7f]);
        out.clear();
        put(&mut out, OID, &[0; 0x80]).unwrap();
        assert_eq!(&out[..3], &[OID, 0x81, 0x80]);
        let (tag, whole, rest) = tlv(&out).unwrap();
        assert_eq!((tag, whole.len(), rest.len()), (OID, 0x83, 0));
        assert!(tlv(&[SEQUENCE, 0x05, 0, 0]).is_none());
    }
}
