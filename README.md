# pacquet

A fast, pnpm-compatible package manager for Node.js, written in Rust.

pacquet is a drop-in replacement for [pnpm](https://pnpm.io). It reads the same `pnpm-lock.yaml`, writes the same `node_modules` layout, and supports every pnpm CLI command -- so you can switch between the two without touching your project.

## Highlights

- **Full pnpm command parity** -- every command, alias, and flag that pnpm offers is implemented.
- **Native Rust performance** -- dependency resolution, tarball extraction, and file linking are all in-process; no shelling out to npm for core operations.
- **Content-addressable store** -- identical files are stored once and hard-linked into projects, just like pnpm.
- **Lockfile v9** -- reads and writes `pnpm-lock.yaml` v9 with full fidelity, including peer-dependency suffixes, patched dependencies, and catalogs.
- **Workspace support** -- `pnpm-workspace.yaml`, `--filter`, `--recursive`, injected dependencies, and workspace protocol (`workspace:*`, `workspace:^`, `workspace:~`) are all supported.
- **Cross-platform** -- Linux, macOS, and Windows (x64 & ARM64). Windows uses junctions where pnpm does; `virtualStoreDirMaxLength` is 60 on Windows and 120 elsewhere, matching pnpm exactly.

## Installation

```sh
cargo install --path crates/cli
```

Pre-built binaries for all platforms are attached to each [GitHub Release](../../releases).

## Commands

Every command listed below is natively implemented. Aliases (e.g. `i` for `install`, `rm` for `remove`) work the same as in pnpm.

### Package management

| Command | Description |
|---|---|
| `install` | Install all dependencies from `pnpm-lock.yaml` |
| `add` | Add one or more packages (npm, git, workspace, local, tarball) |
| `remove` | Remove packages |
| `update` | Update packages to their latest allowed version |
| `ci` | Clean install with a frozen lockfile |
| `import` | Generate `pnpm-lock.yaml` from an npm or yarn lockfile |
| `dedupe` | Re-resolve to remove duplicate lockfile entries |
| `fetch` | Download packages into the store without linking |
| `prune` | Remove extraneous packages from `node_modules` |
| `link` / `unlink` | Link or unlink local packages |
| `rebuild` | Re-run build scripts for installed packages |

### Running scripts

| Command | Description |
|---|---|
| `run` | Run a package.json script |
| `exec` | Run a command with `node_modules/.bin` in PATH |
| `dlx` | Run a package in a temporary environment |
| `test` | Shortcut for `run test` |
| `start` | Shortcut for `run start` |
| `restart` | Run stop, restart, and start scripts in sequence |
| `create` | Scaffold a project from a `create-*` starter kit |

### Inspection

| Command | Description |
|---|---|
| `list` | List installed packages |
| `why` | Show why a package is installed |
| `outdated` | Check for newer versions |
| `licenses` | List licenses of installed packages |
| `audit` | Check for known vulnerabilities (native lockfile analysis) |

### Publishing

| Command | Description |
|---|---|
| `publish` | Publish to the registry (workspace protocol is rewritten automatically) |
| `pack` | Create a tarball from a package |
| `version` | Bump version with git tag and commit |

### Patching

| Command | Description |
|---|---|
| `patch` | Extract a package for editing |
| `patch-commit` | Generate a `.patch` file from your edits |
| `patch-remove` | Remove an applied patch |

### Registry & account

| Command | Description |
|---|---|
| `login` / `logout` | Authenticate (web login with browser flow, or legacy) |
| `whoami` | Show current username |
| `token` | List, create, or revoke auth tokens |
| `profile` | View or update your npm profile |
| `owner` | Manage package maintainers |
| `access` | Set package access level (public / restricted) |
| `dist-tag` | Manage distribution tags |
| `star` / `unstar` / `stars` | Star or unstar packages |
| `deprecate` | Deprecate a package version |
| `unpublish` | Remove a version from the registry |
| `search` | Search the registry |
| `info` | Display package metadata |
| `bugs` / `docs` / `repo` | Open the bug tracker, docs, or repo in a browser |
| `ping` | Ping the registry |
| `team` | Manage organization teams |

### Configuration & environment

| Command | Description |
|---|---|
| `config` / `get` / `set` | Read and write `.npmrc` settings |
| `store` | Manage the content-addressable store (status, add, prune, path) |
| `cache` | Inspect and manage the metadata cache |
| `env` | Manage Node.js versions |
| `setup` | Set up shell helpers for `pnpm` / `pnpx` aliases |
| `self-update` | Update the `packageManager` field in `package.json` |
| `doctor` | Check for common configuration issues |
| `completion` | Print shell completions (bash, zsh, fish, powershell, elvish) |

### Other

| Command | Description |
|---|---|
| `init` | Create a `package.json` |
| `bin` / `root` / `prefix` | Print directory paths |
| `pkg` | Read or write fields in `package.json` |
| `set-script` | Add a script to `package.json` |
| `edit` | Open an installed package in `$EDITOR` |
| `deploy` | Deploy a workspace package into a target directory |
| `approve-builds` / `ignored-builds` | Manage build script approval |
| `cat-file` / `cat-index` / `find-hash` | Inspect the store |
| `server` | Manage the store server |
| `recursive` | Prefix for recursive workspace execution |
| `install-test` | Run install followed by test |
| `xmas` | :christmas_tree: |

## Compatibility with pnpm

pacquet aims for **behavioral parity** with pnpm. Specifically:

- **Lockfile** -- reads and writes `pnpm-lock.yaml` v9. A project installed by pacquet can be used by pnpm and vice versa without re-resolving.
- **`node_modules` layout** -- isolated (default), hoisted, and PnP layouts match pnpm's structure, including `.pnpm` virtual store paths and peer-dependency hash suffixes.
- **`.modules.yaml`** -- the metadata file in `node_modules` is written in the same format as pnpm so both tools recognize each other's installs.
- **`.pnpm-workspace-state-v1.json`** -- workspace state file is written on every install, matching pnpm's schema and field selection.
- **`.npmrc`** -- all settings from pnpm's `.npmrc` reference are supported: hoisting, `node-linker`, auth, TLS, proxy, peer dependencies, workspace injection, etc.
- **`.pnpmfile.cjs`** -- `readPackage` and `afterAllResolved` hooks are executed.
- **Content-addressable store** -- shares the same `~/.pnpm-store` as pnpm. Packages installed by one are reused by the other.

### Known differences

- `publish` delegates the final registry upload to `npm publish` (pnpm does the same).
- `audit --fix` parses the npm audit API response; pnpm does its own lockfile-to-audit-tree conversion.
- Some reporter/output formatting details differ from pnpm's exact output.

## Architecture

pacquet is organized as a Cargo workspace with focused crates:

| Crate | Purpose |
|---|---|
| `pacquet-cli` | CLI entry point, command routing, argument parsing |
| `pacquet-package-manager` | Core resolution, installation, virtual store layout |
| `pacquet-lockfile` | `pnpm-lock.yaml` v9 parsing, serialization, dependency paths |
| `pacquet-registry` | npm registry metadata fetching |
| `pacquet-tarball` | Tarball download, integrity verification, extraction |
| `pacquet-store-dir` | Content-addressable file store |
| `pacquet-npmrc` | `.npmrc` configuration parsing |
| `pacquet-network` | HTTP client with throttling, proxy, and TLS support |
| `pacquet-executor` | Lifecycle script execution, shell emulation |
| `pacquet-package-manifest` | `package.json` reading and writing |
| `pacquet-env` | Node.js version management |
| `pacquet-list` | Dependency tree rendering for `list` and `why` |
| `pacquet-fs` | Cross-platform filesystem utilities (symlinks, junctions) |
| `pacquet-diagnostics` | Error reporting via miette |

## Development

```sh
# Install dependencies
just install

# Run tests
cargo nextest run

# Run a specific test
cargo nextest run -p pacquet-lockfile dep_path_to_filename

# Lint
cargo clippy --workspace --all-targets -- -D warnings

# Format
cargo fmt --all
```

## Benchmarking

```sh
# Start a local registry (e.g. verdaccio)
verdaccio

# Compare branches
just integrated-benchmark --scenario=frozen-lockfile my-branch main

# Compare against pnpm
just integrated-benchmark --scenario=frozen-lockfile --with-pnpm HEAD

# See all options
just integrated-benchmark --help
```

## License

MIT
