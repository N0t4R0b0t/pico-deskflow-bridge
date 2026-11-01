/* Pico W (RP2040). */
MEMORY {
    BOOT2 : ORIGIN = 0x10000000, LENGTH = 0x100
    /* 2 MiB flash minus BOOT2 and the last 16 KiB, which hold settings and BLE bonds. */
    FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 0x100 - 16K
    RAM   : ORIGIN = 0x20000000, LENGTH = 264K
}
