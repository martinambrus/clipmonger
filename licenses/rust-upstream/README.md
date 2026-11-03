# Rust license source clarifications

`sources.json` records exact upstream license texts used where a published
crate omits them. Each normal entry identifies the version, the commit in its
packaged `.cargo_vcs_info.json`, the upstream URL and the SHA-256 of the text.
The notice renderer checks those values before using the clarification.

`winapi-x86_64-pc-windows-gnu` 0.4.0 predates the packaged VCS metadata. Its
locked crates.io archive SHA-256 is recorded in `sources.json`; its 1,418
published payload files match the upstream tree at the recorded revision. The
packaged original manifest differs only in the package version. The renderer
checks the locked archive and its extracted contents before using that notice.
This does not assert that the version bump itself was committed upstream.

For the five listed egui 0.37.0 packages, the published manifest declares
`MIT OR Apache-2.0`. The renderer may select Apache-2.0 and uses the canonical
text from the Apache Software Foundation, pinned by SHA-256 in
`apache-source.json`; it checks the locked archive and packaged contents, and
stops if a NOTICE or COPYRIGHT file needs additional review. See the [Cargo
license-field reference](https://doc.rust-lang.org/cargo/reference/manifest.html#the-license-and-license-file-fields)
and [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0.txt).
