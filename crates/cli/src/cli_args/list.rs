use crate::State;
use clap::Args;
use pacquet_list::{
    IncludedDependencies, ListJsonOptions, render_json, render_parseable, render_tree,
};
use std::path::Path;

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Display only the dependency graph for packages in dependencies and optionalDependencies.
    #[arg(short = 'P', long = "production", alias = "prod")]
    production: bool,
    /// Don't display packages from dependencies.
    #[arg(long = "no-production")]
    no_production: bool,

    /// Display only the dependency graph for packages in devDependencies.
    #[arg(short = 'D', long = "dev")]
    dev: bool,
    /// Don't display packages from devDependencies.
    #[arg(long = "no-dev")]
    no_dev: bool,

    /// Include optionalDependencies.
    #[arg(long = "optional")]
    optional: bool,
    /// Don't display packages from optionalDependencies.
    #[arg(long = "no-optional")]
    no_optional: bool,

    /// Show information in JSON format.
    #[arg(long)]
    json: bool,
    /// Show parseable output instead of tree view.
    #[arg(long)]
    parseable: bool,

    /// Show extended information.
    #[arg(long)]
    long: bool,

    /// Max display depth of the dependency tree.
    #[arg(long, default_value_t = 0)]
    depth: i32,
}

impl ListArgs {
    pub fn run(self, state: State) -> miette::Result<()> {
        let mut include = IncludedDependencies {
            dependencies: true,
            dev_dependencies: true,
            optional_dependencies: true,
        };
        if self.production && !self.dev {
            include.dev_dependencies = false;
        } else if self.dev && !self.production {
            include.dependencies = false;
            include.optional_dependencies = false;
        }
        if self.optional {
            include.optional_dependencies = true;
        }
        if self.no_production {
            include.dependencies = false;
        }
        if self.no_dev {
            include.dev_dependencies = false;
        }
        if self.no_optional {
            include.optional_dependencies = false;
        }

        let State { config, manifest, lockfile, lockfile_importer_id, .. } = state;
        let project_dir = manifest.path().parent().unwrap_or_else(|| Path::new("."));
        let render_options = ListJsonOptions {
            manifest: &manifest,
            lockfile: lockfile.as_ref(),
            lockfile_importer_id: &lockfile_importer_id,
            project_dir,
            modules_dir: &config.modules_dir,
            registry: &config.registry,
            include,
            depth: self.depth,
            long: self.long,
        };
        let output = if self.parseable {
            render_parseable(render_options)?
        } else if self.json {
            render_json(render_options)?
        } else {
            render_tree(render_options)?
        };
        println!("{output}");
        Ok(())
    }
}
