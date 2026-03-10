# Options

[pnpm documentation](https://pnpm.io/pnpm-cli#options)

| Done | Command                 | Notes |
| ---- | ----------------------- | ----- |
| ✅   | -C <path>, --dir <path> |       |
| ✅   | -w, --workspace-root    | top-level flag |

# Manage dependencies

## `pacquet add <pkg...>`

[pnpm documentation](https://pnpm.io/cli/add)

- [x] Install from npm registry, including explicit versions, semver ranges, tags, npm alias specs, and multiple package specs
- [x] Install from the workspace (via `workspace:` protocol and `--workspace`)
- [x] Install from local file system
- [x] Install from remote tarball
- [~] Install from Git repository (GitHub-style specs are supported)

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
| ✅   | --reporter=<name>           | accepted for compatibility |
| ✅   | --use-store-server          | accepted for compatibility |
| ✅   | --shamefully-hoist          | enables hoisted links in `.pnpm/node_modules` |
| ✅   | --ignore-scripts            | install already skips lifecycle scripts |
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
|      | enable-pre-post-scripts      |       |
| ✅   | --resume-from <package_name> | supported for direct `run` and embedded `pnpm run` |
| ✅   | --report-summary             | supported for direct `run` and embedded `pnpm run` |
| ✅   | --filter <package_selector>  | direct and embedded workspace selection |
| ✅   | --filter-prod <selector>     | direct and embedded workspace selection with prod-only traversal |
| ✅   | --fail-if-no-match           | direct and embedded filter behavior |

## `pacquet test`

[pnpm documentation](https://pnpm.io/cli/test)

## `pacquet start`

[pnpm documentation](https://pnpm.io/cli/start)

# Misc.

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
