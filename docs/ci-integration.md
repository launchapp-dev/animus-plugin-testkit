# CI Integration

## In a plugin repo

The shortest path is to add `animus-plugin-testkit` as a git submodule (or
clone it in a CI step) and call the harness:

```yaml
# .github/workflows/conformance.yml
name: conformance
on: [push, pull_request]
jobs:
  conformance:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          path: plugin
      - uses: actions/checkout@v4
        with:
          repository: launchapp-dev/animus-plugin-testkit
          path: testkit
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --release
        working-directory: plugin
      - run: cargo build --release --workspace
        working-directory: testkit
      - run: ./target/release/animus-plugin-harness conformance --plugin ../plugin/target/release/${{ github.event.repository.name }}
        working-directory: testkit
```

## Embedding scenarios in your own integration tests

Depend on `provider-conformance` and call `baseline_scenarios()`:

```rust
#[test]
fn baseline_loads() {
    let scenarios = provider_conformance::baseline_scenarios().unwrap();
    assert_eq!(scenarios.len(), 8);
}
```

You can then write your own harness equivalent if you want a single-binary
test that doesn't shell out — the wire surface lives entirely in
`animus-plugin-protocol` + `animus-provider-protocol`.

## Matrix workflow

`.github/workflows/matrix.yml` (in this repo) runs the harness against every
published `launchapp-dev/animus-provider-*` repo on a weekly schedule. Each
plugin's `MatrixReport` is uploaded as a CI artifact for trend analysis.
