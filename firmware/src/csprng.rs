//! ChaCha12 CSPRNG registered as the `embassy-crypto` RNG driver (used by the
//! BLE security manager for pairing keys) and exposed as [`Rng`] for TLS,
//! seeded from the RP2040's ring oscillator at startup.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use rand_chacha::ChaCha12Rng;
use rand_chacha::rand_core::{CryptoRng, RngCore, SeedableRng as _};

static RNG: Mutex<CriticalSectionRawMutex, RefCell<Option<ChaCha12Rng>>> = Mutex::new(RefCell::new(None));

/// Must be called before the BLE stack is created.
pub fn seed(seed: [u8; 32]) {
    RNG.lock(|rng| rng.replace(Some(ChaCha12Rng::from_seed(seed))));
}

fn fill(buf: &mut [u8]) {
    RNG.lock(|rng| {
        let mut rng = rng.borrow_mut();
        let rng = rng.as_mut().expect("CSPRNG used before it was seeded");
        rng.fill_bytes(buf);
    })
}

struct Driver;

impl embassy_crypto::driver::Rng for Driver {
    fn fill_bytes(buf: &mut [u8]) {
        fill(buf)
    }
}

/// Handle to the shared CSPRNG.
pub struct Rng;

impl RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        fill(&mut b);
        u32::from_le_bytes(b)
    }

    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        fill(&mut b);
        u64::from_le_bytes(b)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        fill(dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_chacha::rand_core::Error> {
        fill(dest);
        Ok(())
    }
}

impl CryptoRng for Rng {}

embassy_crypto::rng_impl!(Driver);

// AES-CMAC, AES-ECB and P-256 ECDH for pairing, in software (the RP2040 has
// no crypto accelerator). Referenced so the linker keeps its driver symbols.
use embassy_crypto_rustcrypto as _;
