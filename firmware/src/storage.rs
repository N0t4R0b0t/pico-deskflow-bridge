//! Settings that survive a reboot, kept as a `sequential-storage` map in the
//! last 16 KiB of flash (excluded from FLASH in memory.x).

use deskflow_proto::auth::Password;
use deskflow_proto::pairing::MAX_LAPTOPS;
use deskflow_proto::settings::{FORM_MAX, Settings};
use embassy_rp::flash::{Async, Error as FlashError, Flash};
use embassy_rp::peripherals::FLASH;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use heapless::Vec;
use sequential_storage::cache::NoCache;
use sequential_storage::map::{MapConfig, MapStorage, PostcardValue};
use serde::{Deserialize, Serialize};
use trouble_host::prelude::BondInformation;

use crate::board::FLASH_SIZE;

const STORAGE_SIZE: u32 = 16 * 1024;

pub type StorageFlash = Flash<'static, FLASH, Async, FLASH_SIZE>;
/// Settings and firmware updates both write flash.
pub type SharedFlash = Mutex<CriticalSectionRawMutex, StorageFlash>;
pub type Storage = MapStorage<u8, Area, NoCache>;

/// A part of the flash, addressed from its start.
pub struct Area {
    flash: &'static SharedFlash,
    start: u32,
    size: u32,
}

impl Area {
    pub const fn new(flash: &'static SharedFlash, start: u32, size: u32) -> Self {
        Area { flash, start, size }
    }
}

impl ErrorType for Area {
    type Error = FlashError;
}

impl ReadNorFlash for Area {
    const READ_SIZE: usize = <StorageFlash as ReadNorFlash>::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), FlashError> {
        ReadNorFlash::read(&mut *self.flash.lock().await, self.start + offset, bytes).await
    }

    fn capacity(&self) -> usize {
        self.size as usize
    }
}

impl NorFlash for Area {
    const WRITE_SIZE: usize = <StorageFlash as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <StorageFlash as NorFlash>::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), FlashError> {
        NorFlash::erase(&mut *self.flash.lock().await, self.start + from, self.start + to).await
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), FlashError> {
        NorFlash::write(&mut *self.flash.lock().await, self.start + offset, bytes).await
    }
}

impl MultiwriteNorFlash for Area {}
/// The BLE task and the web server both write.
pub type SharedStorage = Mutex<CriticalSectionRawMutex, Storage>;

/// Map keys. 0 (a single bond) and 2 (settings as postcard) held earlier
/// formats and are no longer read.
pub const TLS_CLIENT_KEY: u8 = 1;
const SETTINGS: u8 = 3;
const BLE_BONDS: u8 = 4;
const PASSWORD: u8 = 5;
const ACTIVE_LAPTOP: u8 = 6;

/// Paired laptops, oldest first; the position is what the hotkeys go by.
pub type Bonds = Vec<BondInformation, MAX_LAPTOPS>;

pub fn new(flash: &'static SharedFlash) -> Storage {
    let area = Area::new(flash, FLASH_SIZE as u32 - STORAGE_SIZE, STORAGE_SIZE);
    MapStorage::new(area, MapConfig::new(0..STORAGE_SIZE), NoCache::new())
}

const SETTINGS_MAX: usize = FORM_MAX;
/// For every read and write: walking the map reads each item it passes, so
/// even fetching a small value fails if this can't hold the largest item,
/// and a failed fetch must not pass for "nothing stored".
pub const ITEM_BUF: usize = SETTINGS_MAX + 128;

/// The saved settings applied over `defaults` (see `deskflow_proto::settings`
/// for the format), or `defaults` if none are saved.
pub async fn load_settings(storage: &mut Storage, defaults: Settings) -> Settings {
    let mut buf = [0u8; ITEM_BUF];
    match storage.fetch_item::<&[u8]>(&mut buf, &SETTINGS).await {
        Ok(Some(stored)) => {
            let (settings, skipped) = defaults.with_stored(stored);
            log::info!("settings: using saved settings");
            for field in skipped {
                log::warn!("settings: saved '{}' isn't valid; using the default", field);
            }
            settings
        }
        Ok(None) => defaults,
        Err(_) => {
            log::warn!("settings: stored settings unreadable; using defaults");
            defaults
        }
    }
}

pub async fn save_settings(storage: &mut Storage, settings: &Settings) -> Result<(), ()> {
    let mut form = heapless::String::<SETTINGS_MAX>::new();
    settings.write_form(&mut form).map_err(|_| ())?;
    let mut buf = [0u8; ITEM_BUF];
    storage.store_item(&mut buf, &SETTINGS, &form.as_bytes()).await.map_err(|_| ())
}

#[derive(Serialize, Deserialize)]
struct StoredBonds(Bonds);
impl PostcardValue<'_> for StoredBonds {}

pub async fn load_bonds(storage: &mut Storage) -> Bonds {
    let mut buf = [0u8; ITEM_BUF];
    match storage.fetch_item::<StoredBonds>(&mut buf, &BLE_BONDS).await {
        Ok(stored) => stored.map(|StoredBonds(b)| b).unwrap_or_default(),
        Err(_) => {
            log::warn!("ble: pairing storage unreadable; waiting for pairing");
            Bonds::new()
        }
    }
}

pub async fn save_bonds(storage: &mut Storage, bonds: &Bonds) -> Result<(), ()> {
    let mut buf = [0u8; ITEM_BUF];
    storage.store_item(&mut buf, &BLE_BONDS, &StoredBonds(bonds.clone())).await.map_err(|_| ())
}

/// The Bluetooth address of the laptop that had the keyboard last.
pub async fn load_active(storage: &mut Storage) -> Option<[u8; 6]> {
    let mut buf = [0u8; ITEM_BUF];
    let stored = storage.fetch_item::<&[u8]>(&mut buf, &ACTIVE_LAPTOP).await.ok()??;
    stored.try_into().ok()
}

pub async fn save_active(storage: &mut Storage, address: &[u8; 6]) -> Result<(), ()> {
    let mut buf = [0u8; ITEM_BUF];
    storage.store_item(&mut buf, &ACTIVE_LAPTOP, &&address[..]).await.map_err(|_| ())
}

/// Clears settings, pairings and the password. The TLS key stays, so
/// Deskflow still trusts the bridge.
pub async fn factory_reset(storage: &mut Storage) -> Result<(), ()> {
    let mut buf = [0u8; ITEM_BUF];
    for key in [SETTINGS, BLE_BONDS, PASSWORD, ACTIVE_LAPTOP] {
        storage.remove_item(&mut buf, &key).await.map_err(|_| ())?;
    }
    Ok(())
}

/// Forgets every paired laptop. Takes effect after a restart (the BLE stack
/// keeps its copy until then).
pub async fn forget_bonds(storage: &mut Storage) -> Result<(), ()> {
    let mut buf = [0u8; ITEM_BUF];
    storage.remove_item(&mut buf, &BLE_BONDS).await.map_err(|_| ())
}

/// The settings page's password, if one is set.
pub async fn load_password(storage: &mut Storage) -> Option<Password> {
    let mut buf = [0u8; ITEM_BUF];
    match storage.fetch_item::<&[u8]>(&mut buf, &PASSWORD).await {
        Ok(Some(stored)) => {
            let password = Password::from_bytes(stored);
            if password.is_none() {
                log::warn!("web: stored password unreadable; the page is open");
            }
            password
        }
        Ok(None) => None,
        Err(_) => {
            log::warn!("web: password storage unreadable; the page is open");
            None
        }
    }
}

/// Stores the password, or removes it (`None`).
pub async fn save_password(storage: &mut Storage, password: Option<&Password>) -> Result<(), ()> {
    let mut buf = [0u8; ITEM_BUF];
    match password {
        Some(p) => storage.store_item(&mut buf, &PASSWORD, &&p.to_bytes()[..]).await,
        None => storage.remove_item(&mut buf, &PASSWORD).await,
    }
    .map_err(|_| ())
}
