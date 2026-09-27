#!/usr/bin/env python3
"""Build po/<lang>.po from po/spotty.pot + po/i18n/<lang>.json.

po/i18n/<lang>.json maps msgid → msgstr; untranslated messages keep an
empty msgstr (gettext falls back to English at runtime). Placeholders
({name}, {query}, …) must be preserved — the generator validates them.

    scripts/make_po.py es fr de     # specific languages
    scripts/make_po.py --all        # everything in po/i18n/
"""
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
POT = ROOT / "po" / "spotty.pot"
I18N = ROOT / "po" / "i18n"

HEADER = """# {lang} translation for Spotty.
# Generated from po/spotty.pot + po/i18n/{lang}.json by scripts/make_po.py.
msgid ""
msgstr ""
"Project-Id-Version: spotty\\n"
"Content-Type: text/plain; charset=UTF-8\\n"
"Content-Transfer-Encoding: 8bit\\n"
"Language: {lang}\\n"
"MIME-Version: 1.0\\n"
"PO-Revision-Date: 2026-09-26 00:00+0000\\n"
"Last-Translator: Spotty contributors\\n"
"Language-Team: {lang}\\n"
"""


def placeholders(s: str) -> set[str]:
    return set(re.findall(r"\{[a-z_]+\}", s))


def lookup(data: dict, msgid: str) -> str:
    if msgid in data:
        return data[msgid]
    # Tolerate authoring without C-style \" escapes in the key.
    flat = msgid.replace('\\"', '"')
    for k, v in data.items():
        if k.replace('\\"', '"') == flat:
            return v
    return ""


def build(lang: str) -> None:
    data = json.loads((I18N / f"{lang}.json").read_text())
    out = [HEADER.format(lang=lang)]
    translated = missing = bad = 0
    for block in POT.read_text().split("\n\n"):
        m = re.search(r'^msgid "(.*)"$', block, re.M)
        if not m or m.group(1) == "":
            continue
        msgid = m.group(1)
        refs = "\n".join(l for l in block.splitlines() if l.startswith("#:"))
        msgstr = lookup(data, msgid)
        if not msgstr:
            missing += 1
        else:
            translated += 1
            have, want = placeholders(msgstr), placeholders(msgid)
            if have != want:
                bad += 1
                print(
                    f"  !! {lang}: placeholders differ for {msgid!r}: "
                    f"msgid={sorted(want)} msgstr={sorted(have)}"
                )
        body = json.dumps(msgstr, ensure_ascii=False)[1:-1]
        out.append(f"{refs}\nmsgid \"{msgid}\"\nmsgstr \"{body}\"")
    (ROOT / "po" / f"{lang}.po").write_text("\n\n".join(out) + "\n")
    flag = f", {bad} PLACEHOLDER ERRORS" if bad else ""
    print(f"po/{lang}.po: {translated} translated, {missing} fallback{flag}")
    if bad:
        sys.exit(1)


if __name__ == "__main__":
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    langs = [p.stem for p in sorted(I18N.glob("*.json"))] if args == ["--all"] else args
    for lang in langs:
        build(lang)
