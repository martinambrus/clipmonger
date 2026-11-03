# Generating Rust notice evidence

Use the approved cargo-about executable (SHA-256 `e902609296601f873c08ca912acea0c37b375128ea3940eb307a76080afb8211`) and the checked-in `packaging/about.toml`. The scan must run under the project's source/resource guard from the checkout root. The guard receipt must record the command shown below and successful cleanup. Existing ordinary `cargo about generate` receipts cannot be used with this procedure.

Copy the wrapper into a private directory at the front of PATH, using its source digest in its filename. Use absolute paths for the approved executable, configuration, output, and wrapper. Ensure no Cargo alias shadows this subcommand, and that the private directory and files remain under your control during the scan. For example, in a POSIX shell:

```sh
checkout=$(pwd -P)
private=$(mktemp -d)
wrapper_sha=$(sha256sum "$checkout/scripts/notice-scan-wrapper.py" | cut -d ' ' -f 1)
subcommand="clipmonger-notice-scan-$wrapper_sha"
wrapper="$private/cargo-$subcommand"
cp "$checkout/scripts/notice-scan-wrapper.py" "$wrapper"
chmod 700 "$wrapper"
export PATH="$private:$PATH"
tool=/absolute/path/to/approved/cargo-about
config="$checkout/packaging/about.toml"
data=/absolute/path/to/new/scan.json
receipt=/absolute/path/to/guard-receipt.json
sha256sum "$checkout/scripts/notice-scan-wrapper.py" "$wrapper" "$tool" "$config"
```

Check the two wrapper hashes equal `wrapper_sha`, the tool hash equals the approved hash above, and the config hash is `f1f8a05bfa523d1233ad9661db8d97b086a27754283d5dc580cb36dddb38078d`. Immediately before and after the guarded command, repeat those hashes and confirm they are unchanged. The guard snapshots Rust and Cargo inputs, not these Python, configuration and executable files. Run the following **as the guarded command**:

```sh
cargo "$subcommand" --tool "$tool" --config "$config" --output-file "$data"
```

The wrapper checks its own hash-derived name and the executable and config hashes, then executes the absolute tool with the locked workspace, all features, both release targets and JSON output. After checking the guard result and the unchanged hashes, record the evidence:

```sh
python3 scripts/record-notice-scan.py --scan-data "$data" --scan-receipt "$receipt" --scan-tool "$tool" --scan-wrapper "$wrapper"
```

The resulting manifest is an integrity record, not a signature. The receipt and data may be copied to another checkout with identical source and an equivalent populated Cargo cache. Supply the approved local tool to the notice renderer or package checks there. A new scan is needed if the wrapper source changes.
