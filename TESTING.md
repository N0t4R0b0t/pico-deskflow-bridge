# Hardware test checklist

What CI and `cargo test` can't cover: everything here needs a Pico 2 W (or
Pico W), a Mac running the Deskflow server, and a Linux laptop with Bluetooth. Each step only
depends on the ones before it, so the first failure tells you which layer
broke.

If something fails, grab the log from the [settings page](README.md#settings-page)
(or from the serial port if the page won't load) and note the step and what the
LED was doing.

## 1. Flash

1. Build the board's UF2: `./scripts/uf2.sh` → `pico-deskflow-bridge-pico2-w.uf2`
   for a Pico 2 W, `./scripts/uf2.sh pico-w` → `pico-deskflow-bridge-pico-w.uf2`
   for a Pico W (or the VS Code tasks, or a CI artifact). Copy it to the Mac.
2. Hold **BOOTSEL** while plugging the Pico into the Mac; a drive mounts
   (`RP2350` on a Pico 2 W, `RPI-RP2` on a Pico W).
3. Copy the `.uf2` onto it. It ejects and the Pico reboots.

- [ ] The log's first lines include `board: Pico 2 W, 200 MHz` (or `Pico W`, at
      its stock clock): the right image for the board.

## 2. USB network and log

- [ ] System Settings → Network shows **Deskflow Bridge** with `192.168.77.2`.
      (No interface at all: USB enumeration failed.)
- [ ] `screen /dev/tty.usbmodem* 115200` shows log lines (Ctrl-A K to quit).
- [ ] The LED gives a **short blip every 2 s**. (No LED at all: the wireless
      chip didn't start; look for `cyw43 up` in the log. On a Pico 2 W this is
      also the check that the radio's SPI clock divider is right.)
- [ ] First boot logs `deskflow: generated a TLS client key`, then
      `deskflow: client fingerprint …`, then `web: settings page at http://192.168.77.1/`.

## 3. Settings page

- [ ] <http://192.168.77.1/> loads: Status, Clipboard, Settings, and a log that
      updates live.
- [ ] Note **This bridge's certificate** for the next step.

## 4. Deskflow

1. Add a screen named **`arch-laptop`** to the server layout. Leave TLS and
   "check peer fingerprints" on.
2. Allow incoming connections for Deskflow in the macOS firewall if asked.

- [ ] Log: `deskflow: TLS up, server fingerprint …`, then `server speaks protocol 1.x`.
- [ ] Deskflow asks to trust the new client; the fingerprint matches the page.
      (No prompt, and the log says *"server closed the connection; if it checks
      client certificates, trust …"*: add the `v2:sha256:…` line from the page to
      `tls/trusted-clients` in Deskflow's settings directory and restart the server.)
- [ ] Page: Deskflow **Connected**. LED: **slow blink**.
- [ ] **Handshake time:** the log's `TLS handshake took … ms` (also "Last TLS
      handshake" on the page): ________ ms. It runs on core 1, so even a slow
      one shouldn't freeze anything.
- [ ] While Deskflow reconnects (restart the server), the settings page keeps
      updating and the laptop's Bluetooth link stays up.

## 5. Bluetooth

If the laptop was paired with an earlier build, remove that pairing first
(`bluetoothctl remove <addr>`): the HID layout changed (media keys and a
relative pointer were added, and the two pointers became separate input
devices), and the laptop keeps the old one cached.

On the laptop:

```sh
bluetoothctl
  scan on          # wait for "Deskflow Bridge"
  pair <addr>
  trust <addr>
  connect <addr>
```

- [ ] Log: `ble: paired`, then `ble: pairing saved (1 laptops)`.
- [ ] `evtest` or `libinput list-devices` on the laptop shows the bridge's
      keyboard, two pointers and a consumer-control device.
- [ ] Page: Bluetooth **Connected**. LED: **solid**.

## 6. Input

Move the cursor off the Mac's screen edge toward the laptop.

- [ ] Cursor appears on the laptop; log `cursor entered`; page "Connected, cursor here".
- [ ] LED **flickers** while moving the mouse or typing.
- [ ] **Keyboard** on the settings page matches the server's OS (macOS,
      Windows or Linux).
- [ ] Typing uses the laptop's layout; Shift and Alt/Option combinations work;
      Cmd/Windows key acts as Super.
- [ ] Arrows, Home/End, Page Up/Down, Insert/Delete, F1–F12, the keypad
      (Num Lock on and off), Print Screen and Pause all reach the laptop.
- [ ] Media keys: volume up/down/mute, play/pause, next/previous, and screen
      brightness on the laptop.
- [ ] The cursor moves in Absolute mode. (Clicks work but no movement, on an
      X11 laptop, while `libinput debug-events` shows `POINTER_MOTION_ABSOLUTE`:
      check X's input drivers. `grep 'Using input driver' /var/log/Xorg.0.log`
      shows which one the bridge got; install what's missing, e.g.
      `xf86-input-libinput`, and log out and back in.)
- [ ] Clicks and vertical scroll work.
- [ ] **Horizontal scroll** goes the right way (unverified; if reversed, flip
      `invert_pan` in `deskflow-proto/src/hid.rs`).
- [ ] Moving back to the Mac releases everything: no stuck keys or buttons.

## 7. Clipboard

- [ ] Copy a short text on the Mac, move to the laptop. Log:
      `clipboard: N characters from the server`; the page's Clipboard section
      shows the count.
- [ ] Set **Laptop keyboard layout** on the settings page to the laptop's layout.
- [ ] In a text field on the laptop, press **Ctrl+Alt+V** (Ctrl+Option+V on a
      Mac keyboard). The text is typed;
      the hotkey itself doesn't reach the laptop; log `clipboard: typed N characters`.
- [ ] Typing starts only after you let go of Ctrl and Option (no Ctrl/Alt
      shortcuts fire on the laptop).
- [ ] Symbols (`!@#$%^&*()_+{}|:"<>?~€`) and newlines come out right.
- [ ] Letters the layout has (`é` on French, `ä ö ü ß` on German, `ñ` on Spanish,
      `å ä ö` on Swedish) are typed; emoji are skipped and the log says how many.
- [ ] **Type on laptop** on the page does the same; **Stop** halts it, and
      pressing any key on the Mac while it's typing stops it too.
- [ ] Copy something over 64 KB (16 KB on a Pico W): the page says "(first
      part only)" and only that part is typed.
- [ ] Long text arrives without dropped or doubled characters; if it doesn't,
      lower **Typing speed** on the page.
- [ ] Copy something new on the Mac while it's typing: typing stops.
- [ ] Paste text into the box on the page and click **Send and type**, with a
      text field focused on the laptop: it's typed there. **Send only** keeps it
      for Ctrl+Alt+V. Works on a Wayland server too, where Deskflow's clipboard
      may never arrive (the page stays on "Nothing copied yet").

## 8. Reconnect and persistence

- [ ] After saving settings and restarting, **This bridge's certificate** on
      the page is unchanged, Deskflow doesn't ask to trust it again, and a
      password set on the page still applies.
- [ ] Windows laptop: disconnect and reconnect, and restart the Pico; input
      works without re-pairing.
- [ ] Power-cycle the Pico: the laptop reconnects without re-pairing; Deskflow
      reconnects without a new trust prompt; the client fingerprint is unchanged.
- [ ] Restart the Deskflow server: the Pico reconnects within a few seconds.
- [ ] Toggle Bluetooth on the laptop: it reconnects and input works.

## 9. Settings page actions

- [ ] Change the **screen name**, then Save and restart: the page reloads with it;
      Deskflow reports an unknown client until the layout is renamed too, and the
      log says so.
- [ ] Set the **resolution** to the laptop's real (scaled) one: cursor speed
      across the edge feels consistent.
- [ ] TLS → **only the server certificate below**, then "Use the one the
      server presented", then Save: still connects.
- [ ] Change one character of the pinned fingerprint, then Save: refused, and
      the log shows the real fingerprint. Then fix it.
- [ ] A bad value (screen name with a space) is highlighted and nothing restarts.
- [ ] **Restart**: the USB network drops (`dmesg` shows a disconnect) and comes
      back with its address, and the page reconnects by itself afterwards.
- [ ] **Forget paired laptop**: after the restart the page shows "Waiting for
      pairing"; remove the old pairing on the laptop (`bluetoothctl remove <addr>`)
      and pair again.
- [ ] **Mouse → Relative**, Save: the cursor moves, clicks and scrolls on the
      laptop (speed follows the laptop's pointer settings). Back to Absolute.
- [ ] Diagnostics show a connection interval of 15 ms or less a few seconds
      after connecting, ideally 7.5 ms (it can take four tries, 2 s apart; the log shows
      `ble: asking for a connection interval …` and `ble: connection interval …`).
      A slower laptop may settle on up to 26 ms. Fast mouse circles on the
      laptop keep up with no lag, and clicks land where the cursor is.
- [ ] Long, continuous mouse movements don't build up lag. With TLS on, a
      fast mouse pinned **Inbound backlog** at 2048 B at 150 MHz; at 200 MHz it
      should stay well below, and TLS off cures it if not. **Packets in
      Bluetooth chip** stays around 1 either way.

## 10. Diagnostics and password

- [ ] Diagnostics show a Bluetooth signal (around −40 to −80 dBm) and a
      connection interval while the laptop is connected; "not connected" after
      turning Bluetooth off on the laptop.
- [ ] Input latency appears after some typing or mouse movement; note the
      average and worst: ______ / ______.
- [ ] Connection counts go up when Deskflow or the laptop reconnects.
- [ ] Free heap and unused stack look sane (stack well above zero on both
      cores), and stay that way after a TLS reconnect.
- [ ] Set a password: logging in takes under a second (note: ______ ms on
      this board); the page stays usable meanwhile and the mouse doesn't stutter.
- [ ] Log out: settings and buttons are hidden, status and log still update.
      A wrong password is refused after a 1 s pause.
- [ ] Log in, Save and restart settings: still logged in after the restart.
- [ ] Tick "Also needed to see status and the log", log out: only the login
      box shows. `curl http://192.168.77.1/api/state` answers 401.
- [ ] Hold BOOTSEL for 5 s with the bridge running: log `password is removed`,
      the page is open again. A short press does nothing.

## 11. Several laptops

- [ ] With one laptop paired, click **Pair a new laptop**: the first one drops,
      and the second can pair as usual. It becomes laptop 2 and active.
- [ ] **Switch** on laptop 1: laptop 2 drops, laptop 1 reconnects within a few
      seconds and gets input. Laptop 2 stays refused, not flapping
      (`bluetoothctl info` shows it not connected).
- [ ] **Ctrl+Alt+1** / **Ctrl+Alt+2** with the cursor on a laptop switch too, and
      the digit isn't typed. No Ctrl or Alt stays stuck afterwards.
- [ ] Restart the Pico: the same laptop is active.
- [ ] **Forget** on the active laptop: the other takes over. Numbers stay in
      order. Forget the last one: pairing opens by itself.
- [ ] A laptop using a private address (`bluetoothctl show` lists a `Privacy`
      other than off, or Windows with one): it still reconnects when active. If
      it can't, **Pair a new laptop** lets it back in; note it here.

## 12. Buttons and LEDs

With nothing fitted:

- [ ] Open **Panel** on the page: the LEDs follow Deskflow and Bluetooth, and
      Activity flickers with input.
- [ ] **Button 1** with text held: it's typed. **Button 2**: moves to the next
      laptop. **Button 3**: locks the laptop. Holding **Button 3** for 5 s restarts.
- [ ] Change a button's action in Settings and Save: the new action runs.
- [ ] Nothing presses by itself over a long session (log has no `panel:` lines).

With buttons and LEDs fitted (see the README's wiring):

- [ ] Each LED matches the page's. Each button does what the page's does.
- [ ] Hold buttons 1 and 3 from power-on for 3 s: the log says `factory reset`;
      settings and pairings are gone, and Deskflow still trusts the bridge.

## 13. Firmware updates (Pico 2 W)

- [ ] Flash `pico-deskflow-bridge-pico2-w.uf2` with BOOTSEL once (it now includes
      the bootloader). Settings, pairings and the fingerprint survive.
- [ ] The log says `firmware <version>`, and the page shows the same.
- [ ] Install `pico-deskflow-bridge-pico2-w-update.uf2` of a newer build from the
      page. USB drops, returns within a couple of minutes, and the page reloads with
      the new version. The log says `update: confirmed`.
- [ ] Mouse and keyboard work during the upload's aftermath, and afterwards.
- [ ] A wrong file (any other file, or the Pico W's UF2) is refused with a message,
      and nothing restarts.
- [ ] Rollback: install a build that never serves the page or reaches Deskflow, or
      just unplug the USB cable from the server side within 5 minutes of an update
      while powered another way: the old version comes back.
- [ ] Holding BOOTSEL while plugging in still gives the `RP2350` drive.
