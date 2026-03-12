# Options

[pnpm documentation](https://pnpm.io/pnpm-cli#options)

| Done | Command                 | Notes |
| ---- | ----------------------- | ----- |
| ✅   | -C <path>, --dir <path> |       |
| ✅   | -w, --workspace-root    | top-level flag |

# Command Audit

Audited against local pnpm command registration in `/Users/samuelschwanzer/WebstormProjects/pnpm/pnpm/src/cmd/index.ts`.

Implemented commands:
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
- `list` / `ls` / `ll`
- `outdated`
- `prune`
- `remove`
- `run`
- `set`
- `start`
- `store`
- `test`
- `why`

Missing commands from local pnpm:
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

# Manage dependencies

## `pacquet add <pkg...>`

[pnpm documentation](https://pnpm.io/cli/add)

- [x] Install from npm registry, including explicit versions, semver ranges, tags, npm alias specs, and multiple package specs
- [x] Install from the workspace (via `workspace:` protocol and `--workspace`)
- [x] Install from local file system
- [x] Install from remote tarball
- [~] Install from Git repository (GitHub shorthand/`github:`/GitHub URL specs are supported, including named installs; broader Git host coverage is still missing)

| Done | Command                       | Notes |
| ---- | ----------------------------- | ----- |
| ✅   | --save-prod                   |       |
| ✅   | --save-dev                    |       |
| ✅   | --save-optional               |       |
| ✅   | --save-exact                  |       |
| ✅   | --save-peer                   |       |
| ✅   | --ignore-workspace-root-check | blocks accidental workspace-root add unless this or `-w` is used |
|      | --global                      |       |
| ✅   | --workspace                   | adds only from workspace and writes `workspace:` protocol |
| ✅   | --filter <package_selector>   | targets selected workspace projects |
| ✅   | -r, --recursive               | adds to all workspace projects except workspace root |

## `pacquet install`

[pnpm documentation](https://pnpm.io/cli/install)

| Done | Command                     | Notes |
| ---- | --------------------------- | ----- |
| ✅   | --force                     | bypasses warm-install reuse and reimports packages from store/registry |
| ✅   | --offline                   | strict offline mode using only local metadata/store data |
| ✅   | --prefer-offline            | uses local metadata cache first and falls back online when needed |
| ✅   | --prod                      |       |
| ✅   | --dev                       |       |
| ✅   | --no-optional               |       |
| ✅   | --lockfile-only             | writes pnpm-lock.yaml without creating node_modules |
| ✅   | --fix-lockfile              | overrides frozen lockfile strictness for repair/update flow |
| ✅   | --frozen-lockfile           |       |
| ✅   | --prefer-frozen-lockfile    | overrides `.npmrc` preference for current command |
| ✅   | --no-prefer-frozen-lockfile | overrides `.npmrc` preference for current command |
| ✅   | --reporter=<name>           | supports `default`, `append-only`, and `silent` |
| ✅   | --use-store-server          | accepted for compatibility |
| ✅   | --shamefully-hoist          | enables hoisted links in `.pnpm/node_modules` |
| ✅   | --ignore-scripts            | skips project and dependency lifecycle scripts during install |
| ✅   | --filter <package_selector> | workspace installs can target selected projects |
| ✅   | -r, --recursive             | installs all workspace projects from any workspace cwd |
| ✅   | --resolution-only           | resolves and writes lockfile without installing |

## `pacquet remove <pkg...>`

[pnpm documentation](https://pnpm.io/cli/remove)

| Done | Command                     | Notes |
| ---- | --------------------------- | ----- |
| ✅   | --save-prod                 | remove only from `dependencies` |
| ✅   | --save-dev                  | remove only from `devDependencies` |
| ✅   | --save-optional             | remove only from `optionalDependencies` |
| ✅   | --filter <package_selector> | removes only in selected workspace projects; no-op when nothing matches |
| ✅   | -r, --recursive             | removes across all workspace projects including workspace root |

# Run scripts

## `pacquet run`

[pnpm documentation](https://pnpm.io/cli/run)

| Done | Command                      | Notes |
| ---- | ---------------------------- | ----- |
| ✅   | script-shell                 |       |
| ✅   | shell-emulator               | basic shell-emulation (env-prefix + `&&`) |
| ✅   | --recursive                  | runs script in all workspace packages (excluding root by default) |
| ✅   | --if-present                 |       |
| ✅   | --parallel                   | supported for direct `run` and embedded `pnpm run` |
| ✅   | --stream                     | accepted for compatibility in direct and embedded workspace runs |
| ✅   | --aggregate-output           | supported for direct `run` and embedded `pnpm run` |
| ✅   | --workspace-concurrency      | supported for direct `run` and embedded `pnpm run` |
| ✅   | --sequential                 | supported for direct `run` and embedded `pnpm run` |
| ✅   | --reverse                    | supported for direct `run` and embedded `pnpm run` |
| ✅   | --no-bail / --bail           | supported for direct `run` and embedded `pnpm run` |
| ✅   | enable-pre-post-scripts      | runs `pre<name>`/`post<name>` around `pacquet run <name>` when enabled in `.npmrc` |
| ✅   | --resume-from <package_name> | supported for direct `run` and embedded `pnpm run` |
| ✅   | --report-summary             | supported for direct `run` and embedded `pnpm run` |
| ✅   | --filter <package_selector>  | direct and embedded workspace selection |
| ✅   | --filter-prod <selector>     | direct and embedded workspace selection with prod-only traversal |
| ✅   | --fail-if-no-match           | direct and embedded filter behavior |

## `pacquet exec`

[pnpm documentation](https://pnpm.io/cli/exec)

| Done | Command       | Notes |
| ---- | ------------- | ----- |
| ✅   | `exec <cmd>`  | runs an arbitrary command with `node_modules/.bin` prepended to `PATH` |
| ✅   | `exec -r`     | runs the command in all workspace packages |
| ✅   | `--report-summary` | writes `pnpm-exec-summary.json` for recursive exec |
| ✅   | `--reporter-hide-prefix` / `--no-reporter-hide-prefix` | recursive exec output prefix control |

## `pacquet dlx`

[pnpm documentation](https://pnpm.io/cli/dlx)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `dlx <pkg>` | installs a temporary package environment under `cache-dir/dlx` and runs its default bin |
| ✅   | `--package <pkg>` | installs explicit package(s) before running the requested command |
| ✅   | `-c, --shell-mode` | runs the requested command through the system shell in the original cwd |
| ✅   | cache reuse / expiry | the temp environment is reused via `cache-dir/dlx/<key>/pkg`, and `dlx-cache-max-age` controls expiry |
| ✅   | `--reporter=<name>` | passes through `default`, `append-only`, and `silent` to the temporary install phase |

## `pacquet dedupe`

[pnpm documentation](https://pnpm.io/cli/dedupe)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `dedupe` | re-resolves dependencies and updates the lockfile/install result to newer compatible versions |
| ✅   | `dedupe --check` | checks whether dedupe would change the lockfile without mutating the current workspace |
| ✅   | `--ignore-scripts` | passes through to the underlying install flow |
| ✅   | `--offline` / `--prefer-offline` | passes through to the underlying resolution flow |
| ✅   | `--reporter=<name>` | passes through `default`, `append-only`, and `silent` to the underlying install flow |

## `pacquet fetch`

[pnpm documentation](https://pnpm.io/cli/fetch)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `fetch` | warms the store from `pnpm-lock.yaml` without mutating workspace `node_modules` |
| ✅   | `-P, --prod` | fetches only production and optional packages from the lockfile root importer |
| ✅   | `-D, --dev` | fetches only development packages from the lockfile root importer |
| ✅   | `--reporter=<name>` | supports `default`, `append-only`, and `silent` progress output |

## `pacquet list` / `pacquet ls`

[pnpm documentation](https://pnpm.io/cli/list)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `ls --json --depth=0/1` | JSON output is covered against local pnpm goldens for direct and depth-1 dependency views |
| ✅   | `ls --parseable` | parseable output is covered against local pnpm goldens |
| ✅   | `ls --depth=0` | text output matches current local pnpm in the compatibility suite; the golden normalizes pnpm's flat/tree variant formatting |
| ✅   | `--long` | long JSON and parseable output are covered |

## `pacquet why`

[pnpm documentation](https://pnpm.io/cli/why)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `why <pkg>` | tree output is covered by the current `why` suite |
| ✅   | `why --json` | compatibility suite covers both currently observed pnpm JSON shapes |
| ✅   | `why --parseable` | parseable output is covered against local pnpm goldens |
| ✅   | `--depth=0` | JSON compatibility covered in the golden suite |

## `pacquet cache`

[pnpm documentation](https://pnpm.io/cli/cache)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `cache list` | lists locally cached package metadata files |
| ✅   | `cache list-registries` | lists registries present in the local metadata cache |
| ✅   | `cache view <pkg>` | shows cached metadata grouped by registry |
| ✅   | `cache delete <pattern...>` | deletes matching metadata cache files |

## `pacquet config` / `pacquet get` / `pacquet set`

[pnpm documentation](https://pnpm.io/cli/config)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `config list` | prints merged raw `.npmrc` settings sorted by key |
| ✅   | `config list --json` | prints merged raw `.npmrc` settings as JSON |
| ✅   | `config get <key>` | supports kebab-case and camelCase keys |
| ✅   | `config set <key> <value>` | writes to `.npmrc`; `key=value` syntax is also supported |
| ✅   | `config delete <key>` | removes the key from `.npmrc` |
| ✅   | `get` / `set` | top-level aliases delegating to `config get` / `config set` |
| ~    | `--location project|global` | project and global `.npmrc` targets are supported; pnpm-workspace.yaml fallback is still missing |

## `pacquet prune`

[pnpm documentation](https://pnpm.io/cli/prune)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `prune` | forces virtual-store orphan pruning and re-syncs installed dependencies to the manifest |
| ✅   | `prune --prod` | removes installed `devDependencies` while keeping production dependencies |
| ✅   | `prune --no-optional` | excludes optional dependencies from the pruned install result |
| ✅   | `--ignore-scripts` | passes through to the underlying install flow |

## `pacquet outdated`

[pnpm documentation](https://pnpm.io/cli/outdated)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `outdated` | checks registry dependencies against the latest registry version with pnpm-like table output |
| ✅   | `outdated --json` | emits pnpm-shaped JSON objects keyed by package name |
| ✅   | `--prod` / `--dev` / `--no-optional` | filters dependency groups like install |
| ✅   | package filters | exact names and glob patterns are supported |
| ✅   | `--compatible` | uses the latest version that still satisfies the declared range |
| ✅   | `-r` / `--recursive` | aggregates outdated dependencies across workspace packages |
| ✅   | `--long` | includes detail strings when registry metadata provides them |
| ✅   | `--format table|list|json` / `--no-table` | supports pnpm-compatible format selection |
| ~    | exit-code parity | pacquet still does not mirror pnpm's non-zero exit code for outdated packages |

## `pacquet link`

[pnpm documentation](https://pnpm.io/cli/link)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `link <dir>` | links a local package into the current project using a `link:` spec and reinstalls |
| ✅   | `link <pkg>` | resolves globally linked packages from `PNPM_HOME/global/node_modules` |
| ✅   | `link` | registers the current package in the global link area and links its bins into `PNPM_HOME` |
| ✅   | workspace overrides | writes `overrides` to `pnpm-workspace.yaml` when linking inside a workspace |
| ✅   | peer dependency warning | warns when the linked package declares peer dependencies |
| ~    | manifest preservation | pacquet currently rewrites the dependency spec to `link:` instead of always preserving the previous declared range like pnpm |
| ~    | install-option parity | pnpm's broader `link` install/config option surface is not fully mirrored yet |

## `pacquet unlink`

[pnpm documentation](https://pnpm.io/cli/unlink)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `unlink` | removes pacquet-created `link:` dependencies and reinstalls the project |
| ✅   | `unlink <pkg...>` | removes only matching linked packages |
| ✅   | `unlink -r` / `unlink --recursive` | unlinks matching packages across workspace projects |
| ✅   | workspace overrides | removes `link:` overrides from `pnpm-workspace.yaml` during unlink |
| ~    | exact pnpm manifest restoration | pnpm can reinstall from the pre-link saved range; pacquet currently removes the `link:` spec because `link` does not preserve the original range yet |

## `pacquet test`

[pnpm documentation](https://pnpm.io/cli/test)

## `pacquet start`

[pnpm documentation](https://pnpm.io/cli/start)

# Misc.

## `pacquet bin`

[pnpm documentation](https://pnpm.io/cli/bin)

| Done | Command | Notes |
| ---- | ------- | ----- |
| ✅   | `bin` | prints `<cwd>/node_modules/.bin` |
| ✅   | `bin -g` / `bin --global` | prefers `PNPM_HOME`, matching pnpm's common global-bin flow |

## `pacquet store`

[pnpm documentation](https://pnpm.io/cli/store)

| Done | Command | Notes                                                     |
| ---- | ------- | --------------------------------------------------------- |
| ✅   | status  | reports modified/missing store entries                    |
| ✅   | add     | fetches package specs into the global store without mutating the current workspace |
| ✅   | prune   | removes unreferenced store packages while keeping referenced ones |
| ✅   | path    |                                                           |

## `pacquet init`

[pnpm documentation](https://pnpm.io/cli/init)
