For more information, read [pnpm docs about .npmrc](https://pnpm.io/npmrc)

# Dependency Hoisting Settings

| Done | Field                | Notes |
|------|----------------------|-------|
| ~    | hoist                | virtual-store hoist + root public-hoist behavior wired |
| ~    | hoist_pattern        | supports include/exclude pattern filtering for virtual-store hoists |
| ~    | public_hoist_pattern | supports include/exclude pattern filtering for root node_modules hoists |
| ~    | shamefully_hoist     | hoists all discovered virtual-store packages to root node_modules |

# Node-Modules Settings

| Done | Field                 | Notes                               |
|------|-----------------------|-------------------------------------|
| ✅    | store_dir             |                                     |
| ✅    | modules_dir           |                                     |
| ~    | node_linker           | `hoisted` root hoist is wired; `pnp` now suppresses root node_modules hoists |
| ~    | symlink               | `false` avoids root links for isolated linker; with hoisted linker packages are copied into root |
| ✅    | virtual_store_dir     |                                     |
| ~    | package_import_method | `auto`, `copy`, `hardlink`, `clone`, and `clone-or-copy` wired (`clone` depends on reflink support) |
| ~    | modules_cache_max_age | stale orphan virtual-store entries are pruned based on age |

# Lockfile Settings

| Done | Attribute                    | Notes |
|------|------------------------------|-------|
| ✅    | lockfile                     |       |
| ✅    | prefer_frozen_lockfile       |       |
| ✅    | lockfile_include_tarball_url |       |

# Registry & Authentication Settings

| Done | Field              | Notes |
|------|--------------------|-------|
| ✅    | registry           |       |
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
| ~    | dedupe_peer_dependents            | controls peer-suffix preference for hoisted package selection |
| ✅    | strict_peer_dependencies          | fails install on missing/incompatible required peers |
| ✅    | resolve_peers_from_workspace_root | used by strict-peer validation plus lockfile and lockfile-less peer auto-install resolution fallback to workspace root |
