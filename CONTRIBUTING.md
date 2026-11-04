# Contributing to VNMF

Thank you for your interest! VNMF is developed first of all for the author's own manga library,
but contributions of any size are welcome: bug reports, fixes, translations, documentation,
packaging, new features.

Issues and pull requests can be written in **English or Ukrainian**.

## Reporting a problem

Please include:

- what you did, what you expected and what happened instead;
- the version (`vnmfinfo --version`) and your system (distribution, GTK / libadwaita version);
- for a file that doesn't open or looks wrong — the output of `vnmfinfo file.vnmf`
  (and `vnmfinfo --json file.vnmf` if it's long). Please don't attach files with copyrighted
  content; if a file is needed to reproduce the problem, try to reproduce it with your own or
  freely licensed images.

For a damaged file, `vnmfrecovery` usually tells what exactly is broken; include its output too.

## Building and testing

Requirements are listed in the [README](README.md#building). In short:

```sh
cargo build --workspace
cargo test --workspace        # must pass
cargo fmt --all               # formatting (settings in rustfmt.toml)
cargo clippy --workspace      # please don't add new warnings
```

Running the applications from the source tree:

```sh
cargo run --bin vnmf-reader -- some.vnmf
cargo run --bin vnmfinfo -- some.vnmf
```

Most of the format logic has tests in `vnmf-core/tests/roundtrip.rs` and next to the code
(`#[cfg(test)]` modules). A fix for a bug should ideally come with a test that fails without it.
Tests must not depend on files outside the repository — generate the data in the test
(see the helpers at the top of `roundtrip.rs`).

The Android reader is built from `android/` (see the README). If you change `vnmf-ffi`, please
make sure the Android project still builds, or say in the pull request that you couldn't check it.

## Changing the file format

The format is specified in [FORMAT.md](FORMAT.md), and the specification is the reference: a
reader or writer must be implementable from it alone.

- Any change to the layout, the meaning of a field or a flag **must update FORMAT.md** in the same
  pull request.
- Version 2.0 is stable. Compatible additions (new flags, using reserved bytes with
  "0 = old behaviour") are a minor version; anything that breaks existing readers needs a new
  major version — please open an issue to discuss it first.
- Files written by older versions must keep opening.

Changes to the metadata schema (`vnmf-metadata/1`) follow the same idea: new optional fields are
fine, renaming or changing the meaning of existing ones is not.

## Code style

- Code, comments, documentation and commit messages are in **English**.
- Formatting: `cargo fmt` with the repository's `rustfmt.toml`.
- Comments explain *why*, not *what*. Doc comments (`///`) on public items.
- No `unwrap()` on data that comes from a file: a damaged or malicious file must produce an
  error, never a panic or wrong output. Use checked arithmetic for offsets and sizes.
- New dependencies: please explain in the pull request why they are needed. The core crates
  (`vnmf-core`, `vnmf-parity`, `vnmf-i18n`) deliberately have very few.

## User interface text and translations

The GTK applications are translated with gettext; see [po/README.md](po/README.md).

- Wrap every user-visible string in `tr("…")`, `trf!("… {0} …", value)` or, for counts,
  `ntrf!("{0} page", "{0} pages", n, n)` (from `vnmf_i18n`).
- Don't build sentences from pieces: translators need whole phrases.
- After changing strings, run `po/update.sh`. You don't have to translate the new strings
  yourself.
- Console tools stay in English on purpose.

**Adding a translation** is one of the easiest ways to help: create `po/<lang>.po` as described
in [po/README.md](po/README.md) and open a pull request. No programming is needed.

## Pull requests

- Keep a pull request focused on one thing; separate refactoring from behaviour changes.
- Describe what changed and why, and how you tested it. Screenshots help for UI changes.
- Commit messages: a short summary line (≈ 70 characters, imperative mood: "Fix…", "Add…"),
  then details if needed.

### AI-assisted contributions

A large part of VNMF itself was written with the help of an AI assistant, so AI-assisted
contributions are welcome on the same terms as any other: you are responsible for the code you
submit, you have read and tested it, and you can explain it. Please mention in the pull request
if a substantial part was generated.

## License

VNMF is licensed under the GNU General Public License, version 3 or later. By submitting a
contribution you agree that it is licensed under the same terms (`GPL-3.0-or-later`).
