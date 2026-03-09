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
- [ ] Add CLI report

## Priority Roadmap vs local `pnpm`

- [x] `pacquet add` registry-spec parity: support multiple packages and preserve explicit version, range, and tag specs.
- [x] Top-level `-w, --workspace-root` so `add` and `install` can target the workspace root from a subproject.
- [~] Install-mode parity: `--ignore-scripts`, `--lockfile-only`, `--fix-lockfile`, `--offline`, `--prefer-offline`, `--resolution-only`, `--force`, `--reporter`, `--use-store-server`, and `--shamefully-hoist` are wired up; deeper reporter/store-server semantics are still missing.
- [~] Workspace command parity: workspace-root safety plus `add/install/remove --filter`, `add --workspace`, `add -r/--recursive`, `install -r/--recursive`, and `remove -r/--recursive` are in place; broader recursive command coverage is still missing.
- [~] Additional `add` sources: `workspace:`, local file system, remote tarball, npm alias specs, and GitHub-style Git specs (`github:` / `git+https://github.com/...`) are in place; broader Git host/protocol coverage is still missing.
- [ ] `.npmrc` parity: hoisting, `node-linker`, auth token helpers, peer-dependency settings, and remaining request config.
- [x] Store parity: `store status`, `store add`, and non-destructive `store prune`.
- [ ] Lifecycle parity: install script handling and `ignore-scripts` behavior consistent with pnpm.
- [ ] Command-surface parity: `exec`, `dlx`, `fetch`, `dedupe`, `cache`, and related advanced workflows.
- [ ] Advanced compatibility: patching, hooks/pnpmfile support, shell completion, reporter polish, and error-code parity.

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

# Run test
just test
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
