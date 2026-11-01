# Assembly

The bridge works as a bare Pico 2 W (or Pico W) on a USB cable, with nothing
to solder. Everything here is optional: three buttons and four LEDs give you
physical controls and status lights. The settings page has a virtual version of
all of them except the factory reset, so skip this if you don't want them.

These instructions come from the firmware's pin assignments. The wiring hasn't
been built and tested yet, so check each step with the tests at the end.

## What you need

| Qty | Part | Notes |
|---|---|---|
| 1 | Pico 2 W or Pico W | With headers if you're using a breadboard |
| 1 | Micro-USB data cable | Charge-only cables won't work |
| 3 | Momentary push buttons | Normally open, for example 6 mm tactile switches |
| 4 | LEDs | Any 3 mm or 5 mm colour |
| 4 | 330 Ω resistors | One per LED |
| | Breadboard and jumper wires, or stripboard and hook-up wire | |

You can fit any subset: for example, only the LEDs, or only button 1.

## Before you start

1. Flash the firmware and check that it works on its own (see
   [TESTING.md](TESTING.md)).
2. Unplug the USB cable. Don't wire anything while the board is powered.

## Wiring

Pin numbers are the board's physical pins, counted from pin 1 at the USB end
and running down one side, then back up the other. Check them against the
labels printed on the underside of your board.

| Part | GPIO | Pin | Wire to |
|---|---|---|---|
| Button 1 | GP13 | 17 | Button, then GND (pin 18) |
| Button 2 | GP14 | 19 | Button, then GND (pin 18) |
| Button 3 | GP15 | 20 | Button, then GND (pin 18) |
| LED Deskflow | GP16 | 21 | 330 Ω, LED, then GND (pin 23) |
| LED Bluetooth | GP17 | 22 | 330 Ω, LED, then GND (pin 23) |
| LED Activity | GP18 | 24 | 330 Ω, LED, then GND (pin 23) |
| LED Attention | GP19 | 25 | 330 Ω, LED, then GND (pin 23) |

```text
GP13 (pin 17) ──── button 1 ───┐
GP14 (pin 19) ──── button 2 ───┼── GND (pin 18)
GP15 (pin 20) ──── button 3 ───┘

GP16 (pin 21) ──── 330 Ω ──── LED ▶ ───┐
GP17 (pin 22) ──── 330 Ω ──── LED ▶ ───┤
GP18 (pin 24) ──── 330 Ω ──── LED ▶ ───┼── GND (pin 23)
GP19 (pin 25) ──── 330 Ω ──── LED ▶ ───┘
```

### Buttons

1. Connect one side of each button to its GPIO pin.
2. Connect the other side of every button to GND (pin 18). They can share a
   single GND wire.

The firmware uses the chip's internal pull-ups, so buttons need no resistors.
Connect them to GND, never to 3V3.

A four-legged tactile switch has two pairs of legs joined inside. Use two legs
at opposite corners, and you'll get the switching side right whichever way round
it sits.

### LEDs

1. Connect a 330 Ω resistor to each GPIO pin.
2. Connect the other end of the resistor to the LED's **long leg** (the anode).
3. Connect the LED's **short leg** (the cathode, usually beside the flat edge of
   the LED) to GND (pin 23). The LEDs can share a single GND wire.

An LED that's in backwards just stays dark and isn't harmed, so turn it round.
Don't leave out the resistors: they protect the GPIO pins as well as the LEDs.

### Enclosure

The radio antenna is on the end of the board opposite the USB connector. Keep
metal, including wires and metal cases, away from that end. Use a plastic
enclosure, and leave the BOOTSEL button reachable: you'll need it to recover the
board, and to remove a forgotten password.

## Check your work

Before plugging in, check that no bare wire or leg is touching a neighbour, and
that nothing connects to VBUS, VSYS or 3V3.

1. Plug the board into the Deskflow server. The bridge should start as usual, and
   no action should fire by itself: the log shouldn't show any `panel:` lines.
2. Open <http://192.168.77.1/> and expand **Panel**. Each real LED should match
   the virtual one with the same name.
3. Press each button briefly, then hold it. The log should show a `panel:` line
   for each press. A long press takes 1 s, or 5 s if it restarts the bridge.

| LED | Meaning |
|---|---|
| Deskflow | On when connected, blinks while connecting |
| Bluetooth | On when a laptop is connected, blinks while pairing is open |
| Activity | Flickers with input |
| Attention | Blinks on a problem the log explains |

| Button | Short press | Long press |
|---|---|---|
| 1 | Type clipboard | Stop typing |
| 2 | Next laptop | Release all keys |
| 3 | Lock laptop (Super+L) | Restart the bridge (hold 5 s) |

These are the defaults. Change them under **Settings** on the page.

### If something's wrong

- **An LED stays dark:** check its polarity, then its GND wire.
- **A button does nothing:** a four-legged switch may be using two joined legs, so
  use legs at opposite corners. Otherwise check its GND wire.
- **A button seems stuck pressed, or actions fire by themselves:** it may be
  wired across the joined legs, or a wire is touching GND. Unplug and check.
- **Poor Bluetooth range:** look for wires or metal near the antenna end.

## Factory reset

Hold buttons 1 and 3 while plugging the board in, and keep them held for 3 s.
Settings, pairings and the password are cleared. The TLS key is kept, so Deskflow
still trusts the bridge. Remove the old pairing on each laptop before pairing it
again.

Without buttons fitted, wire GP13 and GP15 to GND before plugging in and keep
them connected for those 3 s. Remove the wires afterwards.
