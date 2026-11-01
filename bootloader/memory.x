/* Pico 2 W flash layout. Keep in step with firmware/memory-rp2350.x. */
MEMORY {
    /* This bootloader. */
    FLASH            : ORIGIN = 0x10000000, LENGTH = 128K
    BOOTLOADER_STATE : ORIGIN = 0x10020000, LENGTH = 4K
    /* The firmware that runs. */
    ACTIVE           : ORIGIN = 0x10021000, LENGTH = 1972K
    /* An update on its way in, or the previous firmware to go back to. One
       page larger than ACTIVE. The last 16 KiB of flash after it are the
       firmware's settings. */
    DFU              : ORIGIN = ORIGIN(ACTIVE) + LENGTH(ACTIVE), LENGTH = LENGTH(ACTIVE) + 4K
    RAM              : ORIGIN = 0x20000000, LENGTH = 512K
}

SECTIONS {
    /* Goes after .vector_table, to keep it in the first 4K of flash where
     * the boot ROM can find it. */
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH
} INSERT AFTER .vector_table;

_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH
} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);

/* Read by BootLoaderConfig::from_linkerfile_blocking: offsets in flash. */
__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(FLASH);
__bootloader_active_end = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(FLASH);
__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH);
