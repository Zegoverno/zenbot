<!-- What changed and why (one concern per pull request). -->

## Checks

- [ ] `scripts/check.sh` passes (CI runs the same)
- [ ] MAP.md, INSTALL.md or AGENTS.md updated where this makes them wrong

## Marketplace listing (only if this changes `marketplace/plugins.json`)

- [ ] The entry is sorted by name and matches `marketplace/schema.json`
- [ ] The repository is public with an open-source licence, and the release is tagged `v<version>`
- [ ] The README explains what the plugin does and why it needs each permission
- [ ] `kernel` (permissions) and `contributes` (slots) equal the ones in the archive's `plugin.json`
- [ ] No network calls or telemetry beyond what the README declares
- [ ] I tried the archive locally: `zen plugins add ./<name>-<version>.tgz`
- [ ] Maintainer: I read the code at the release tag, not only the diff here
