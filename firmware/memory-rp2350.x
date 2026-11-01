/* Pico 2 W (RP2350A). Layout and boot sections follow embassy's rp235x
 * example; the Image Definition itself comes from embassy-rp. */
MEMORY {
    /* Keep in step with bootloader/memory.x. The bootloader sits at the start
     * of flash, and the last 16 KiB hold settings and BLE bonds. */
    BOOTLOADER       : ORIGIN = 0x10000000, LENGTH = 128K
    BOOTLOADER_STATE : ORIGIN = 0x10020000, LENGTH = 4K
    FLASH            : ORIGIN = 0x10021000, LENGTH = 1972K
    /* An update on its way in, or the previous firmware to go back to. */
    DFU              : ORIGIN = ORIGIN(FLASH) + LENGTH(FLASH), LENGTH = LENGTH(FLASH) + 4K
    /* SRAM0-7, striped. SRAM8/9 (direct-mapped, 4 KiB each) are unused. */
    RAM : ORIGIN = 0x20000000, LENGTH = 512K
}

SECTIONS {
    /* ### Boot ROM info
     *
     * Goes after .vector_table, to keep it in the first 4K of flash
     * where the Boot ROM (and picotool) can find it
     */
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH

} INSERT AFTER .vector_table;

/* Offsets in flash, for firmware/src/update.rs. */
__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOTLOADER);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOTLOADER);
__bootloader_active_start = ORIGIN(FLASH) - ORIGIN(BOOTLOADER);
__bootloader_active_end = ORIGIN(FLASH) + LENGTH(FLASH) - ORIGIN(BOOTLOADER);
__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOTLOADER);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOTLOADER);

/* move .text to start /after/ the boot info */
_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    /* ### Picotool 'Binary Info' Entries
     *
     * Picotool looks through this block (as we have pointers to it in our
     * header) to find interesting information.
     */
    .bi_entries : ALIGN(4)
    {
        /* We put this in the header */
        __bi_entries_start = .;
        /* Here are the entries */
        KEEP(*(.bi_entries));
        /* Keep this block a nice round size */
        . = ALIGN(4);
        /* We put this in the header */
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;

SECTIONS {
    /* ### Boot ROM extra info
     *
     * Goes after everything in our program, so it can contain a signature.
     */
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH

} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);
