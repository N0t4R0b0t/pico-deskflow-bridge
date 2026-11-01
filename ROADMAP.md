# Roadmap

What has been built, and what might come next. Contributions to either list
are welcome (see [CONTRIBUTING.md](CONTRIBUTING.md)). Hardware results go into
[TESTING.md](TESTING.md).

## Done

1. **Stored settings and pairings that don't break.** Settings are kept in the
   settings page's own form format and applied over the defaults at boot, so
   new fields never reset old ones. One HID report map (keyboard, absolute and
   relative mouse, media keys).
2. **Crypto on the second core.** Core 1 runs the whole TLS connection;
   core 0 keeps the network stack and pipes ciphertext to it
   (`firmware/src/tls_offload.rs`, tested on the host by `tools/tls-offload-test`).
3. **Keys from macOS, Windows and Linux servers**, by physical key, plus media
   keys.
4. **Clipboard typing**: laptop layouts (US, UK, German, French, Spanish,
   Swedish/Finnish) generated from xkeyboard-config, an adjustable speed and a
   64 KB limit (16 KB on the Pico W).
5. **Settings page**: optional password, diagnostics (signal strength,
   latency, backlog, heap and stack), absolute or relative mouse.
6. **Several paired laptops** (up to 4), one active at a time, switched from the
   page or with Ctrl+Alt+1…4.
7. **Optional buttons and LEDs**, with a virtual panel on the page that runs
   the same actions ([ASSEMBLY.md](ASSEMBLY.md)).
8. **Firmware updates from the page** on the Pico 2 W, through an embassy-boot
   bootloader that swaps the new image in and rolls back if it doesn't prove
   itself.

## Ideas

- **Setup hotspot (Wi-Fi).** The Pico 2 W starts an access point serving the
  settings page, for Macs that block new USB network adapters or for setup
  from a phone. Off by default; a button long-press or a page setting turns
  it on.
- **Two laptops at once.** Two simultaneous BLE connections, registered with
  Deskflow as two screens, so one bridge drives two laptops. The largest item.
- **Firmware updates on the Pico W.** Needs more flash than its 2 MiB, so
  probably a smaller image or a compressed update slot.
- **Hardware reports.** The wiring in ASSEMBLY.md, the update and rollback
  paths, the Pico W, and macOS and Windows servers have all been checked
  against code and tests but not (or barely) on hardware.
