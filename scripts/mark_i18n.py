#!/usr/bin/env python3
"""One-shot marker pass: wrap static, user-visible string literals in
gettext(...) so they land in po/spotty.pot.

Conservative patterns only: literal arguments of text-bearing builder
calls. format!/dynamic strings and anything machine-readable (icon names,
CSS classes, response ids, action words) are deliberately untouched and
get marked by hand where needed.
"""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent

# (regex, replacement) — first arg of add_response stays the raw id.
RULES: list[tuple[re.Pattern, str]] = [
    (re.compile(r'\.(title|subtitle|label|heading|body|description|placeholder_text|tooltip_text|button_text)\("((?:[^"\\]|\\.)*)"\)'),
     r'.\1(gettext("\2"))'),
    (re.compile(r'adw::Toast::new\("((?:[^"\\]|\\.)*)"\)'),
     r'adw::Toast::new(&gettext("\1"))'),
    (re.compile(r'\.add_response\(("[a-z_]+"), "((?:[^"\\]|\\.)*)"\)'),
     r'.add_response(\1, &gettext("\2"))'),
    (re.compile(r'\.set_description\(Some\("((?:[^"\\]|\\.)*)"\)\)'),
     r'.set_description(Some(&gettext("\1")))'),
    (re.compile(r'\.set_title\("((?:[^"\\]|\\.)*)"\)'),
     r'.set_title(&gettext("\1"))'),
    # Static struct fields: title: "…".into() etc. (display data only —
    # never machine-read keys, which live in action/id fields).
    (re.compile(r'\b(title|subtitle|description|label|body|detail): Some\("((?:[^"\\]|\\.)*)"\.into\(\)\)'),
     r'\1: Some(gettext("\2").into())'),
    (re.compile(r'\b(title|subtitle|description|label|body|detail): "((?:[^"\\]|\\.)*)"\.into\(\)'),
     r'\1: gettext("\2").into()'),
    # format! with no placeholders at all → plain gettext.
    (re.compile(r'format!\("([^"{}]*)"\)'),
     r'gettext("\1")'),
]

FILES = [
    "src/ui/settings_window.rs",
    "src/ui/search_window.rs",
    "src/ui/result_row.rs",
    "trigger-backends/src/search/cmd.rs",
    "trigger-backends/src/search/mod.rs",
    "trigger-backends/src/search/files.rs",
    "trigger-backends/src/search/dictionary.rs",
    "trigger-backends/src/search/translate.rs",
    "trigger-backends/src/search/system.rs",
    "trigger-backends/src/search/emoji.rs",
    "trigger-backends/src/search/run.rs",
    "trigger-backends/src/features/operations.rs",
    "src/triggers.rs",
    "trigger-backends/src/features/preview.rs",
    "src/app.rs",
    "trigger-backends/src/features/clipboard.rs",
]


def process(path: pathlib.Path) -> int:
    text = original = path.read_text()
    for pattern, repl in RULES:
        text = pattern.sub(repl, text)
    if text == original:
        return 0
    # Add the import once, after the crate:: imports block if present.
    if "use crate::i18n::gettext;" not in text:
        m = re.search(r'^(use crate::[^\n]*\n)+', text, re.M)
        if m:
            text = text[: m.end()] + "use crate::i18n::gettext;\n" + text[m.end():]
        else:
            m = re.search(r'^(?://![^\n]*\n|\s*\n)*', text)
            text = text[: m.end()] + "use crate::i18n::gettext;\n\n" + text[m.end():]
    path.write_text(text)
    return len(re.findall(r'gettext\(', text)) - len(re.findall(r'gettext\(', original))


def main() -> None:
    total = 0
    for rel in FILES:
        p = ROOT / rel
        if p.exists():
            n = process(p)
            total += n
            if n:
                print(f"{rel}: +{n} gettext calls")
    print(f"total new markers: {total}")


if __name__ == "__main__":
    main()
