//! The settings page's optional password (see `deskflow_proto::auth`): the
//! live copy, hashing without stalling the other tasks, and recovery.
//!
//! Forgotten password: with the bridge running, hold BOOTSEL for 5 seconds.
//! (Holding it while plugging in starts the UF2 bootloader instead.)

use core::cell::Cell;

use deskflow_proto::auth::{self, Hasher, Password};
use embassy_futures::yield_now;
use embassy_rp::Peri;
use embassy_rp::peripherals::BOOTSEL;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Timer;
use rand_chacha::rand_core::RngCore as _;

use crate::storage::{self, SharedStorage};
use crate::{board, csprng};

static PASSWORD: Mutex<CriticalSectionRawMutex, Cell<Option<Password>>> = Mutex::new(Cell::new(None));

/// Iterations between yields while hashing.
const STEP: u32 = 250;

pub fn current() -> Option<Password> {
    PASSWORD.lock(Cell::get)
}

pub async fn load(storage: &SharedStorage) {
    let password = storage::load_password(&mut *storage.lock().await).await;
    if password.is_some() {
        log::info!("web: the settings page has a password");
    }
    PASSWORD.lock(|c| c.set(password));
}

/// Stores and applies a new password, or none.
pub async fn set(storage: &SharedStorage, password: Option<Password>) -> Result<(), ()> {
    storage::save_password(&mut *storage.lock().await, password.as_ref()).await?;
    PASSWORD.lock(|c| c.set(password));
    Ok(())
}

pub async fn hash(password: &str, salt: &[u8; auth::SALT_LEN], rounds: u32) -> [u8; auth::HASH_LEN] {
    let mut hasher = Hasher::new(password.as_bytes(), salt, rounds);
    loop {
        if let Some(hash) = hasher.step(STEP) {
            return hash;
        }
        yield_now().await;
    }
}

/// Whether `typed` is the password `p`.
pub async fn check(p: &Password, typed: &str) -> bool {
    p.matches(&hash(typed, &p.salt, p.rounds).await)
}

/// A new password record with a fresh salt and session key.
pub async fn create(typed: &str, protect_view: bool) -> Password {
    let mut random = [0u8; auth::SALT_LEN + auth::KEY_LEN];
    csprng::Rng.fill_bytes(&mut random);
    let mut salt = [0u8; auth::SALT_LEN];
    salt.copy_from_slice(&random[..auth::SALT_LEN]);
    let hash = hash(typed, &salt, board::PBKDF2_ROUNDS).await;
    Password::new(random, board::PBKDF2_ROUNDS, |_| hash, protect_view)
}

pub fn new_token(p: &Password) -> heapless::String<{ auth::TOKEN_HEX_LEN }> {
    let mut nonce = [0u8; 16];
    csprng::Rng.fill_bytes(&mut nonce);
    p.token(nonce)
}

/// Removes the password when BOOTSEL is held for 5 seconds. Reading the
/// button briefly stops flash access (and core 1), so it's only polled while
/// there is a password.
#[embassy_executor::task]
pub async fn bootsel_task(mut bootsel: Peri<'static, BOOTSEL>, storage: &'static SharedStorage) -> ! {
    const POLL_MS: u64 = 250;
    const HOLD_POLLS: u32 = 5000 / POLL_MS as u32;
    let mut held = 0;
    loop {
        Timer::after_millis(POLL_MS).await;
        if current().is_none() {
            held = 0;
            continue;
        }
        if !embassy_rp::bootsel::is_bootsel_pressed(bootsel.reborrow()) {
            held = 0;
            continue;
        }
        held += 1;
        if held == HOLD_POLLS {
            match set(storage, None).await {
                Ok(()) => log::warn!("web: BOOTSEL held; the settings page password is removed"),
                Err(()) => log::error!("web: couldn't remove the password"),
            }
        }
    }
}
