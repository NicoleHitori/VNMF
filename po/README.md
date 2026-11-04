# Translations

The VNMF graphical applications (Reader, Info, Creator, Guard) use gettext. The strings in the
code are English and serve as message ids; each language is one file here, `po/<lang>.po`.
The console tools stay English on purpose: their output is meant for scripts and logs.

At runtime the applications load `<prefix>/share/locale/<lang>/LC_MESSAGES/vnmf.mo` for the
system language (`LANG` / `LC_MESSAGES`) or for the language chosen by the user — in Reader
under *Reading settings* → *Language*, in Info, Creator and Guard under main menu → *Language…*
(stored in `~/.config/vnmf/language` and shared by all of them; every installed catalog shows up
in the list automatically), so **adding or fixing a translation doesn't require
rebuilding the programs**: only the `.po` file is compiled into a `.mo` file.

| File | What it is |
|---|---|
| `vnmf.pot` | template: all translatable strings (generated, don't edit) |
| `<lang>.po` | a translation, e.g. `uk.po` — Ukrainian |
| `update.sh` | extracts strings from the code into `vnmf.pot` and merges them into every `.po` |
| `build.sh` | compiles every `.po` into `.mo` files |

Requires GNU gettext ≥ 0.24 (`xgettext` with Rust support; Fedora: `sudo dnf install gettext`).

## Adding a language

```sh
msginit --locale=de --input=po/vnmf.pot --output-file=po/de.po --no-translator
# translate po/de.po (Poedit, GNOME Translation Editor, Lokalize or any text editor)
po/build.sh "$HOME/.local/share/locale"     # or reinstall: ./install.sh --no-build
```

That's all: the next start of an application in that language picks the catalog up.

Things to keep in mind when translating:

- `{0}`, `{1}`… are values inserted by the program. Keep all of them; their order may change
  (`"Page {0} of {1}"` → `"{1} 中的第 {0} 页"`).
- Plural messages (`msgid_plural`) get as many `msgstr[N]` forms as the language has; the rule is
  in the `Plural-Forms` header (`msginit` fills it in).
- `\n` is a line break; `<tt>…</tt>` is markup and must stay as is.

Application names and descriptions in the menus come from the `.desktop` files and
`data/org.vnmf.Reader.metainfo.xml`; add `Name[de]=` / `Comment[de]=` / `xml:lang="de"`
entries there as well.

## Testing without installing

```sh
po/build.sh                                  # → target/locale
VNMF_LOCALEDIR=target/locale LANG=de_DE.UTF-8 cargo run --bin vnmf-reader
```

The locale itself (`de_DE.UTF-8`) must be available on the system (`locale -a`).

## After changing strings in the code

```sh
po/update.sh      # refreshes vnmf.pot and every .po; prints how much is translated
```

New strings appear untranslated, changed ones are marked `fuzzy` with the previous text shown.
In the code, wrap user-visible text in `tr("…")`, `trf!("… {0} …", value)` or, for counts,
`ntrf!("{0} page", "{0} pages", n, n)` (from `vnmf_i18n`); strings in `const` tables are marked
with `n_("…")` and translated with `tr(…)` where they are shown.
