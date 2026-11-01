# Security

## What this protects, and what it doesn't

The settings page is only reachable over the USB cable to the Deskflow server
(`192.168.77.1`), and only answers requests addressed to that address. Changing
anything needs a header that other websites can't send. An optional password
(PBKDF2-SHA256, with a signed session cookie) keeps other programs on the server
out. See the README for details.

Deskflow runs over TLS 1.3 by default. The bridge accepts any server
certificate unless you pin one on the settings page, because the link is a
USB cable straight into the server. With TLS off, other programs on the
server can read the traffic.

The bridge is a Bluetooth keyboard and mouse: whoever is paired can type on the
laptop, and anyone who can get into the server's session can type on it
through the bridge. Pairing is open only for two minutes after
**Pair a new laptop**, or while no laptop is paired.

Firmware updates uploaded to the page aren't signed. Anyone who can reach the
page (and the password, if set) can install any image that boots.

## Reporting a vulnerability

Please report it privately instead of opening a public issue: use GitHub's
**Security → Report a vulnerability** on this repository. If that isn't possible, email
contact@rsalvador.dev. Include the firmware
version, what you did, and what happened. I'll answer within a week or so and
will credit you in the fix unless you'd rather not be named.

Only the latest release is supported.
