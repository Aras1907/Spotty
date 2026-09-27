# Spotty translations

Every user-facing string in Spotty is marked in the source with
`gettext(...)` and extracted into `spotty.pot`. Each language below has a
`.po` file built from that catalogue.

## Languages (30)

Spanish (es), French (fr), German (de), Italian (it), Portuguese (pt),
Russian (ru), Ukrainian (uk), Polish (pl), Czech (cs), Dutch (nl),
Swedish (sv), Danish (da), Norwegian Bokmål (nb), Finnish (fi),
Turkish (tr), Greek (el), Romanian (ro), Hungarian (hu), Bulgarian (bg),
Chinese Simplified (zh_CN), Chinese Traditional (zh_TW), Japanese (ja),
Korean (ko), Vietnamese (vi), Thai (th), Indonesian (id), Hindi (hi),
Arabic (ar), Hebrew (he), Persian (fa).

> **Quality note**: these translations were produced by an AI assistant, not
> professional translators. They are deliberately kept as editable `.po`
> files so native speakers can review and fix wording — corrections are
> welcome via pull requests.

## Workflow

```sh
scripts/extract_i18n.py        # scan src/**.rs for gettext("…") → po/spotty.pot
scripts/make_po.py <lang>      # rebuild po/<lang>.po from po/i18n/<lang>.json
scripts/make_po.py --all       # rebuild every language
scripts/build_locales.sh       # msgfmt → po/locale/<lang>/LC_MESSAGES/spotty.mo
scripts/build_locales.sh --install   # also copy into ~/.local/share/locale
```

`po/i18n/<lang>.json` maps msgid → msgstr; `make_po.py` validates that
every `{placeholder}` in the msgid survives in the translation and exits
non-zero if one is dropped. Untranslated messages fall back to English
automatically at runtime.

Placeholders must be preserved exactly, e.g. `"No triggers match \"{q}\"."`
→ `"Ningún trigger coincide con \"{q}\"."`.
