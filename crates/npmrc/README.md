For more information, read [pnpm docs about .npmrc](https://pnpm.io/npmrc)

# Dependency Hoisting Settings

| Done | Field                | Notes |
|------|----------------------|-------|
| ~    | hoist                | virtual-store hoist wired; derived config clears hoist patterns when disabled |
| ~    | hoist_pattern        | supports single-string `.npmrc` values plus include/exclude wildcard filtering for virtual-store hoists |
| ~    | public_hoist_pattern | supports single-string `.npmrc` values plus include/exclude wildcard filtering for root node_modules hoists |
| ~    | shamefully_hoist     | hoists all discovered virtual-store packages to root node_modules and normalizes to `public-hoist-pattern=*` |

# Node-Modules Settings

| Done | Field                 | Notes                               |
|------|-----------------------|-------------------------------------|
| ✅    | store_dir             |                                     |
| ✅    | modules_dir           |                                     |
| ~    | node_linker           | `hoisted` root hoist is wired; `pnp` now suppresses root node_modules hoists |
| ~    | symlink               | `false` avoids root links for isolated linker, clears hoist/public-hoist patterns, and still copies with hoisted linker |
| ✅    | virtual_store_dir     |                                     |
| ~    | package_import_method | `auto`, `copy`, `hardlink`, `clone`, and `clone-or-copy` wired (`clone` depends on reflink support) |
| ~    | modules_cache_max_age | stale orphan virtual-store entries are pruned based on age |

# Lockfile Settings

| Done | Attribute                    | Notes |
|------|------------------------------|-------|
| ✅    | lockfile                     |       |
| ✅    | prefer_frozen_lockfile       |       |
| ✅    | lockfile_include_tarball_url |       |
| ✅    | exclude_links_from_lockfile  | excludes external `link:` specs from importer snapshot while keeping `workspace:` links |
| ~    | inject_workspace_packages    | workspace protocol dependencies are materialized instead of symlinked during install; `dependenciesMeta.<dep>.injected=true` is honored for workspace deps |

# Registry & Authentication Settings

| Done | Field              | Notes |
|------|--------------------|-------|
| ✅    | registry           | default and scoped (`@scope:registry`) registries are normalized and used for metadata fetch/cache |
| ✅    | <URL>:_authToken   |       |
| ✅    | <URL>:tokenHelper  | token helper must be an absolute existing path; read from user/project config |

# Request Settings

| Done | Field               | Notes |
|------|---------------------|-------|
| ✅    | network_concurrency | limits concurrent HTTP requests |
| ✅    | fetch_timeout       | request timeout (milliseconds) |
| ✅    | strict_ssl          | controls TLS certificate validation |

# Peer Dependency Settings

| Done | Field                             | Notes |
|------|-----------------------------------|-------|
| ✅    | auto_install_peers                |       |
| ~    | dedupe_peer_dependents            | controls peer-suffix preference for hoisted package selection plus importer remapping in frozen and mutable lockfile installs |
| ✅    | strict_peer_dependencies          | fails install on missing/incompatible required peers |
| ✅    | resolve_peers_from_workspace_root | used by strict-peer validation plus lockfile and lockfile-less peer auto-install resolution fallback to workspace root |
