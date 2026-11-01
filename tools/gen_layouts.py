#!/usr/bin/env python3
"""Generate deskflow-proto/src/layouts.rs from xkeyboard-config.

Typing the clipboard means turning characters into key presses on the
laptop's layout. Rather than hand-write those tables, this reads the XKB
symbol files Linux itself uses (pinned below) and emits, per layout, every
character reachable on the main block with no modifier, Shift, AltGr or
Shift+AltGr. Dead keys are skipped: they don't type a character by themselves.

    tools/gen_layouts.py [--cache DIR]      # rewrites deskflow-proto/src/layouts.rs
"""

import argparse
import os
import re
import sys
import urllib.request

XKB_TAG = "xkeyboard-config-2.48"
URL = "https://gitlab.freedesktop.org/xkeyboard-config/xkeyboard-config/-/raw/{tag}/symbols/{file}"

# (setting name, label, XKB file, section)
LAYOUTS = [
    ("us", "US", "us", "basic"),
    ("gb", "UK", "gb", "basic"),
    ("de", "German", "de", "basic"),
    ("fr", "French (AZERTY)", "fr", "basic"),
    ("es", "Spanish", "es", "basic"),
    ("se", "Swedish / Finnish", "se", "basic"),
]

# XKB key names on the main block → HID usage. BKSL is the key above Enter
# (US) or left of it (ISO); Linux maps HID 0x31 and 0x32 to it alike.
KEYS = {
    "TLDE": 0x35,
    **{f"AE{n:02}": u for n, u in zip(range(1, 13), [0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2D, 0x2E])},
    **{f"AD{n:02}": u for n, u in zip(range(1, 13), [0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, 0x2F, 0x30])},
    **{f"AC{n:02}": u for n, u in zip(range(1, 12), [0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, 0x33, 0x34])},
    **{f"AB{n:02}": u for n, u in zip(range(1, 11), [0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10, 0x36, 0x37, 0x38])},
    "BKSL": 0x31,
    "LSGT": 0x64,
}

# Keyboard report modifier bits: Left Shift, Right Alt (AltGr).
SHIFT, ALTGR = 0x02, 0x40
LEVEL_MODS = [0, SHIFT, ALTGR, SHIFT | ALTGR]

KEYSYMS = {
    "space": " ", "exclam": "!", "quotedbl": '"', "numbersign": "#", "dollar": "$", "percent": "%",
    "ampersand": "&", "apostrophe": "'", "quoteright": "'", "parenleft": "(", "parenright": ")",
    "asterisk": "*", "plus": "+", "comma": ",", "minus": "-", "period": ".", "slash": "/",
    "colon": ":", "semicolon": ";", "less": "<", "equal": "=", "greater": ">", "question": "?",
    "at": "@", "bracketleft": "[", "backslash": "\\", "bracketright": "]", "asciicircum": "^",
    "underscore": "_", "grave": "`", "quoteleft": "`", "braceleft": "{", "bar": "|",
    "braceright": "}", "asciitilde": "~",
    "nobreakspace": " ", "exclamdown": "¡", "cent": "¢", "sterling": "£", "currency": "¤",
    "yen": "¥", "brokenbar": "¦", "section": "§", "diaeresis": "¨", "copyright": "©",
    "ordfeminine": "ª", "guillemotleft": "«", "notsign": "¬", "registered": "®", "macron": "¯",
    "degree": "°", "plusminus": "±", "twosuperior": "²", "threesuperior": "³", "acute": "´",
    "mu": "µ", "paragraph": "¶", "periodcentered": "·", "cedilla": "¸", "onesuperior": "¹",
    "masculine": "º", "guillemotright": "»", "onequarter": "¼", "onehalf": "½",
    "threequarters": "¾", "questiondown": "¿", "multiply": "×", "division": "÷",
    "ssharp": "ß", "EuroSign": "€",
}
# Latin-1 letters: XKB names them like Unicode (adiaeresis, Aring, ...).
for name, ch in [
    ("agrave", "à"), ("aacute", "á"), ("acircumflex", "â"), ("atilde", "ã"), ("adiaeresis", "ä"),
    ("aring", "å"), ("ae", "æ"), ("ccedilla", "ç"), ("egrave", "è"), ("eacute", "é"),
    ("ecircumflex", "ê"), ("ediaeresis", "ë"), ("igrave", "ì"), ("iacute", "í"),
    ("icircumflex", "î"), ("idiaeresis", "ï"), ("eth", "ð"), ("ntilde", "ñ"), ("ograve", "ò"),
    ("oacute", "ó"), ("ocircumflex", "ô"), ("otilde", "õ"), ("odiaeresis", "ö"), ("oslash", "ø"),
    ("ugrave", "ù"), ("uacute", "ú"), ("ucircumflex", "û"), ("udiaeresis", "ü"), ("yacute", "ý"),
    ("thorn", "þ"), ("ydiaeresis", "ÿ"), ("oe", "œ"),
]:
    KEYSYMS[name] = ch
    KEYSYMS[name[0].upper() + name[1:]] = ch.upper()
KEYSYMS["AE"] = "Æ"
KEYSYMS["OE"] = "Œ"
KEYSYMS["Ooblique"] = "Ø"
KEYSYMS["THORN"] = "Þ"
KEYSYMS["ETH"] = "Ð"


def keysym_char(name):
    if len(name) == 1 and (name.isalnum() or name in "_"):
        return name
    if re.fullmatch(r"U[0-9A-Fa-f]{4,6}", name):
        return chr(int(name[1:], 16))
    if re.fullmatch(r"0x[0-9A-Fa-f]{8}", name) and int(name, 16) & 0xFF000000 == 0x01000000:
        return chr(int(name, 16) & 0xFFFFFF)
    return KEYSYMS.get(name)


def fetch(file, cache):
    path = os.path.join(cache, XKB_TAG, file)
    if not os.path.exists(path):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with urllib.request.urlopen(URL.format(tag=XKB_TAG, file=file)) as r, open(path, "wb") as f:
            f.write(r.read())
    with open(path, encoding="utf-8") as f:
        return f.read()


def sections(text):
    """{section name: (body, is_default)} of an XKB symbols file."""
    text = re.sub(r"//[^\n]*", "", text)
    out = {}
    for m in re.finditer(r'((?:\w+\s+)*)xkb_symbols\s+"([^"]+)"\s*\{', text):
        depth, i = 1, m.end()
        while depth:
            depth += {"{": 1, "}": -1}.get(text[i], 0)
            i += 1
        out[m.group(2)] = (text[m.end() : i - 1], "default" in m.group(1).split())
    return out


def resolve(file, section, cache, keys, files):
    secs = files.setdefault(file, sections(fetch(file, cache)))
    if section is None:
        section = next(name for name, (_, default) in secs.items() if default)
    body, _ = secs[section]
    for m in re.finditer(r'include\s+"([^"]+)"|key\s+<(\w+)>\s*\{(.*?)\}\s*;', body, re.S):
        if m.group(1):
            for inc in m.group(1).split("+"):
                im = re.fullmatch(r"(\w+)(?:\((\w+)\))?", inc.strip())
                # Only the layout files themselves; level3, keypad etc. add
                # no main-block characters.
                if im and im.group(1) in {"latin", "us", "gb", "de", "fr", "es", "se"}:
                    resolve(im.group(1), im.group(2), cache, keys, files)
        elif m.group(2) in KEYS:
            lists = re.findall(r"\[([^\]]*)\]", m.group(3))
            if lists:
                keys[m.group(2)] = [s.strip() for s in lists[0].split(",")]


def layout_table(file, section, cache):
    keys, files = {}, {}
    resolve(file, section, cache, keys, files)
    table = {}
    # Prefer fewer modifiers, then the key order of KEYS.
    for level, mods in enumerate(LEVEL_MODS):
        for name, usage in KEYS.items():
            syms = keys.get(name, [])
            if level < len(syms):
                ch = keysym_char(syms[level])
                if ch and ch not in table and ch.isprintable() and ch != " ":
                    table[ch] = (usage, mods)
    return table


def rust_char(ch):
    if ch in "\\'":
        return "'\\" + ch + "'"
    if ord(ch) < 0x80:
        return "'" + ch + "'"
    return "'\\u{%04x}'" % ord(ch)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--cache", default=os.path.join(os.path.dirname(__file__), ".xkb-cache"))
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..", "deskflow-proto", "src", "layouts.rs"))
    args = ap.parse_args()

    out = [
        "// Generated by tools/gen_layouts.py from " + XKB_TAG + ". Do not edit.",
        "//",
        "// Per layout: (character, HID usage, modifier bits), sorted by character.",
        "",
        "use crate::typing::Layout;",
        "",
        "pub(crate) const SHIFT: u8 = 0x%02X;" % SHIFT,
        "pub(crate) const ALTGR: u8 = 0x%02X;" % ALTGR,
        "",
        "pub(crate) fn table(layout: Layout) -> &'static [(char, u8, u8)] {",
        "    match layout {",
    ]
    names = {"us": "Us", "gb": "Uk", "de": "German", "fr": "French", "es": "Spanish", "se": "Swedish"}
    for key, _, _, _ in LAYOUTS:
        out.append(f"        Layout::{names[key]} => {key.upper()},")
    out += ["    }", "}", ""]
    for key, label, file, section in LAYOUTS:
        table = layout_table(file, section, args.cache)
        out.append(f"// {label}: {len(table)} characters.")
        out.append(f"static {key.upper()}: &[(char, u8, u8)] = &[")
        for ch in sorted(table):
            usage, mods = table[ch]
            m = " | ".join(n for bit, n in [(SHIFT, "SHIFT"), (ALTGR, "ALTGR")] if mods & bit) or "0"
            out.append(f"    ({rust_char(ch)}, 0x{usage:02X}, {m}),")
        out += ["];", ""]
        print(f"{label}: {len(table)} characters", file=sys.stderr)
    with open(args.out, "w", encoding="utf-8") as f:
        f.write("\n".join(out))
    print(f"wrote {os.path.normpath(args.out)}", file=sys.stderr)


if __name__ == "__main__":
    main()
