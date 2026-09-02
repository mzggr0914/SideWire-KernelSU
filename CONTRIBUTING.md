# Contributing

Bug reports and focused pull requests are welcome.

Before opening a PR, please run:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If you changed the WebUI, also run `pnpm build` from `webui/`. If you changed `sidewired`, make sure the Android arm64 target still builds.

A few preferences:

- keep changes scoped; avoid unrelated cleanup in the same PR
- preserve secure-by-default behavior
- do not add automatic secure → insecure fallback
- note any protocol or capability change explicitly
- include real-device details when reporting Android-specific behavior

For security issues, use the process in [SECURITY.md](SECURITY.md) instead of a public issue.
