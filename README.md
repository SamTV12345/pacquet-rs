# pacquet

Experimental package manager for node.js written in rust.

### TODO

- [x] `.npmrc` support (for supported features [readme.md](./crates/npmrc/README.md))
- [x] CLI commands (for supported features [readme.md](./crates/cli/README.md))
- [x] Content addressable file store support
- [~] Shrink-file support in sync with `pnpm-lock.yml`
  - [x] Frozen lockfile
  - [x] Update outdated lockfile
  - [x] Creating lockfile
- [~] Workspace support
- [ ] Full sync with [pnpm error codes](https://pnpm.io/errors)
- [x] Generate a `node_modules/.bin` folder
- [x] Add CLI report

## Priority Roadmap vs local `pnpm`

- [x] `pacquet add` registry-spec parity: support multiple packages and preserve explicit version, range, and tag specs.
- [x] Top-level `-w, --workspace-root` so `add` and `install` can target the workspace root from a subproject.
- [~] Install-mode parity: `--ignore-scripts`, `--lockfile-only`, `--fix-lockfile`, `--offline`, `--prefer-offline`, `--resolution-only`, `--force`, `--reporter` (`default`, `append-only`, `silent`), `--use-store-server`, and `--shamefully-hoist` are wired up; recursive summaries, progress lines, shared-store reuse, current-lockfile/modules state, and `list`/`why` text+JSON goldens are covered by parity tests, but deeper pnpm reporter/store-server semantics are still missing.
- [~] Workspace command parity: workspace-root safety plus `add/install/remove --filter`, `add --workspace`, `add -r/--recursive`, `install -r/--recursive`, and `remove -r/--recursive` are in place; broader recursive command coverage is still missing.
- [~] Additional `add` sources: `workspace:`, local file system, remote tarball, npm alias specs, and GitHub-style Git specs (`owner/repo`, `github:`, `https://github.com/...`, `git+ssh://git@github.com/...`) are in place; broader Git host/protocol coverage is still missing.
- [x] `.npmrc` parity: hoisting, `node-linker`, auth token helpers, request/TLS settings, local-dir/injected-workspace lockfile behaviors, and peer-dependency settings are wired.
- [x] Store parity: `store status`, `store add`, and non-destructive `store prune`.
- [x] Lifecycle parity: install script handling, `pnpm:devPreinstall`, and `ignore-scripts`/`lockfile-only` behavior are consistent with pnpm for current install flows.
- [~] Command-surface parity: `exec` (including recursive summary/prefix controls), lockfile-based `fetch`, metadata `cache` inspection, `dedupe`, temporary-package `dlx` execution with cache reuse/expiry, and `list`/`why` pnpm goldens are in place; pnpm-exact reporter/output polish is still missing.
- [~] Advanced compatibility: `.pnpmfile.cjs` `readPackage` and `afterAllResolved`, `--ignore-pnpmfile`, custom `--pnpmfile`, hook logging, and lockfile checksum behavior are in place; patching, broader hook coverage, shell completion, reporter polish, and error-code parity are still missing.

## Command Audit vs local `pnpm`

Audited against local pnpm command registration in `/Users/samuelschwanzer/WebstormProjects/pnpm/pnpm/src/cmd/index.ts`.

Implemented in pacquet:
- `add`
- `bin`
- `cache`
- `ci`
- `config`
- `dedupe`
- `dlx`
- `env`
- `exec`
- `fetch`
- `get`
- `init`
- `install`
- `link`
- `list` / `ls` / `ll`
- `outdated`
- `prune`
- `remove`
- `run`
- `set`
- `start`
- `store`
- `test`
- `unlink`
- `why`

Present but only partial pnpm parity:
- `env`
  Current pacquet surface is `add`, `use`, `remove`, and `list`, not the full pnpm env/version-management surface.
- `outdated`
  Table/list/json output, recursive mode, and compatibility filtering are implemented, but pnpm's exit-code behavior and some metadata/details edge cases are still not identical.
- `link`
  Local directory links, global register/link round-trips, workspace override writing, and peer-dependency warnings are in place, but pnpm's exact manifest-preservation and broader install-option parity are still incomplete.
- `unlink`
  Removes pacquet-created `link:` dependencies and link overrides, supports recursive workspace unlinking, and reinstalls afterward, but it cannot fully restore pnpm's pre-link manifest state because pacquet `link` currently rewrites the saved spec.
- `list` / `why`
  Goldens are in place against local pnpm, but pnpm output format still varies between observed environments and the tests normalize equivalent variants.

Missing compared to local pnpm:
- `approve-builds`
- `audit`
- `create`
- `deploy`
- `doctor`
- `ignored-builds`
- `import`
- `licenses`
- `pack`
- `patch`
- `patch-commit`
- `patch-remove`
- `publish`
- `rebuild`
- `restart`
- `self-update`
- `server`
- `setup`
- `update`

Internal pnpm command wiring not counted as user-facing parity:
- `completion-server`
- recursive command dispatcher (`recursive`)
- store inspection helpers (`cat-file`, `cat-index`, `find-hash`)
- install-test helper (`installTest`)

## Debugging

```shell
TRACE=pacquet_tarball just cli add fastify
```

## Testing

```sh
# Install necessary dependencies
just install

# Start a mocked registry server (optional)
just registry-mock launch

# Run tests
just test

# Or run the compiled suite via nextest
cargo nextest run

# Lint strictly
cargo clippy --workspace --all-targets -- -D warnings
```

## Benchmarking

### Install between multiple revisions

First, you to start a local registry server, such as [verdaccio](https://verdaccio.org/):

```sh
verdaccio
```

Then, you can use the script named `integrated-benchmark` to run the various benchmark, For example:

```sh
# Comparing the branch you're working on against main
just integrated-benchmark --scenario=frozen-lockfile my-branch main
```

```sh
# Comparing current commit against the previous commit
just integrated-benchmark --scenario=frozen-lockfile HEAD HEAD~
```

```sh
# Comparing pacquet of current commit against pnpm
just integrated-benchmark --scenario=frozen-lockfile --with-pnpm HEAD
```

```sh
# Comparing pacquet of current commit, pacquet of main, and pnpm against each other
just integrated-benchmark --scenario=frozen-lockfile --with-pnpm HEAD main
```

```sh
# See more options
just integrated-benchmark --help
```
