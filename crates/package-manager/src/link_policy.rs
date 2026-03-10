use pacquet_npmrc::{NodeLinker, Npmrc};

/// Whether dependencies should be materialized at project root `node_modules`.
///
/// pnpm-like behavior:
/// - `node-linker=hoisted`: yes
/// - `node-linker=isolated`: only when symlinks are enabled
/// - `node-linker=pnp`: no root node_modules links
pub(crate) fn should_materialize_root_links(config: &Npmrc) -> bool {
    match config.node_linker {
        NodeLinker::Hoisted => true,
        NodeLinker::Isolated => config.symlink,
        NodeLinker::Pnp => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_materialize_root_links_for_isolated_depends_on_symlink() {
        let mut config = Npmrc::new();
        config.node_linker = NodeLinker::Isolated;
        config.symlink = true;
        assert!(should_materialize_root_links(&config));

        config.symlink = false;
        assert!(!should_materialize_root_links(&config));
    }

    #[test]
    fn should_materialize_root_links_for_hoisted_is_true() {
        let mut config = Npmrc::new();
        config.node_linker = NodeLinker::Hoisted;
        config.symlink = false;
        assert!(should_materialize_root_links(&config));
    }

    #[test]
    fn should_materialize_root_links_for_pnp_is_false() {
        let mut config = Npmrc::new();
        config.node_linker = NodeLinker::Pnp;
        config.symlink = true;
        assert!(!should_materialize_root_links(&config));
    }
}
