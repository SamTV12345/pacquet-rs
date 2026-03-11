For more information, read [pnpm docs about .npmrc](https://pnpm.io/npmrc)

# Dependency Hoisting Settings

| Done | Field                | Notes |
|------|----------------------|-------|
| ✅    | hoist                | virtual-store hoist wired; derived config clears hoist patterns when disabled |
| ✅    | hoist_pattern        | supports single-string `.npmrc` values plus include/exclude wildcard filtering for virtual-store hoists |
| ✅    | public_hoist_pattern | supports single-string `.npmrc` values plus include/exclude wildcard filtering for root node_modules hoists |
| ✅    | shamefully_hoist     | hoists all discovered virtual-store packages to root node_modules and normalizes to `public-hoist-pattern=*` |

# Node-Modules Settings

| Done | Field                 | Notes                               |
|------|-----------------------|-------------------------------------|
| ✅    | store_dir             |                                     |
| ✅    | modules_dir           |                                     |
| ✅    | node_linker           | `isolated`, `hoisted`, and pnpm-style `pnp` install shapes are wired; `pnp` suppresses root dependency links while keeping `.bin`, `.modules.yaml`, `.pnpm`, and writing `.pnp.cjs` |
| ✅    | symlink               | `false` avoids root links for isolated linker, clears hoist/public-hoist patterns, and still copies with hoisted linker |
| ✅    | prefer_symlinked_executables | defaults to true for `node-linker=hoisted`; POSIX `.bin` entries symlink non-Node executables instead of wrapping them, and `run` exports pnpm-style `NODE_PATH`/verify-deps env overrides |
| ✅    | virtual_store_dir     |                                     |
| ✅    | package_import_method | `auto`, `copy`, `hardlink`, `clone`, and `clone-or-copy` wired for store imports plus local directory materialization/relink; local source `node_modules` is ignored during import (`clone` depends on reflink support) |
| ✅    | disable_relink_local_dir_deps | skips refreshing/relinking already-installed local directory dependencies on reinstall, including injected workspace deps, hardlinked local dirs, and frozen installs |
| ✅    | modules_cache_max_age | stale orphan virtual-store entries are pruned using pnpm-like `node_modules/.modules.yaml` `prunedAt` age gating |

# Lockfile Settings

| Done | Attribute                    | Notes |
|------|------------------------------|-------|
| ✅    | lockfile                     |       |
| ✅    | prefer_frozen_lockfile       |       |
| ✅    | lockfile_include_tarball_url |       |
| ✅    | exclude_links_from_lockfile  | excludes external `link:` specs from importer snapshot while keeping `workspace:` links |
| ✅    | inject_workspace_packages    | workspace protocol dependencies are materialized instead of symlinked during install; `dependenciesMeta.<dep>.injected=true` is honored for workspace deps, including nested workspace children, transitive peer-context snapshots, and peer workspace deps that stay linked inside injected snapshots when pnpm does |
| ✅    | dedupe_injected_deps         | injected workspace deps can be rewritten back to `link:` when an already-installed workspace importer provides the same direct dependency set; distinct transitive peer-context local snapshot variants are preserved and only matching variants are deduped |

# Registry & Authentication Settings

| Done | Field              | Notes |
|------|--------------------|-------|
| ✅    | registry           | default and scoped (`@scope:registry`) registries are normalized and used for metadata fetch/cache, lockfile tarball inference, and frozen installs |
| ✅    | <URL>:_authToken   |       |
| ✅    | <URL>:tokenHelper  | token helper must be an absolute existing path; read from user/project config |

# Request Settings

| Done | Field               | Notes |
|------|---------------------|-------|
| ✅    | network_concurrency | limits concurrent HTTP requests |
| ✅    | fetch_timeout       | request timeout (milliseconds) |
| ✅    | strict_ssl          | controls TLS certificate validation |
| ✅    | proxy / https_proxy | pnpm-style proxy derivation is parsed from `.npmrc` and wired into install/store/env HTTP clients |
| ✅    | no_proxy / noproxy  | pnpm-style `noproxy` normalization and bypass list wiring are in place across install/store/env HTTP clients |
| ✅    | cafile / ca         | PEM CA bundle is read from `.npmrc` and wired into install/store/env HTTP clients |
| ✅    | `<URL>:ca/cert/key` | inline per-registry TLS config plus pnpm-style `:cafile/:certfile/:keyfile` are parsed and selected by request URL across install/store/env HTTP clients |

# Peer Dependency Settings

| Done | Field                             | Notes |
|------|-----------------------------------|-------|
| ✅    | auto_install_peers                |       |
| ✅    | dedupe_peer_dependents            | controls peer-suffix preference for hoisted package selection plus importer remapping in frozen and mutable lockfile installs |
| ✅    | strict_peer_dependencies          | fails install on missing/incompatible required peers |
| ✅    | resolve_peers_from_workspace_root | used by strict-peer validation plus lockfile and lockfile-less peer auto-install resolution fallback to workspace root |
