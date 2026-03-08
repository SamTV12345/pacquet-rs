use crate::State;
use clap::Args;
use pacquet_list::{IncludedDependencies, WhyOptions, WhyReportAs, render_why};

#[derive(Debug, Args)]
pub struct WhyArgs {
    /// Package name(s) to inspect.
    #[arg()]
    package_names: Vec<String>,

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

    /// Max display depth of the reverse dependency tree.
    #[arg(long)]
    depth: Option<usize>,
}

impl WhyArgs {
    pub fn run(self, state: State) -> miette::Result<()> {
        if self.package_names.is_empty() {
            miette::bail!("`pacquet why` requires the package name");
        }

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
        // pnpm currently treats `--no-production` and `--no-dev` as no-op for `why`.
        let _ = self.no_production;
        let _ = self.no_dev;
        if self.no_optional {
            include.optional_dependencies = false;
        }

        let report_as = if self.parseable {
            WhyReportAs::Parseable
        } else if self.json {
            WhyReportAs::Json
        } else {
            WhyReportAs::Tree
        };

        let output = render_why(
            WhyOptions {
                lockfile: state.lockfile.as_ref(),
                lockfile_dir: &state.lockfile_dir,
                root_importer_id: &state.lockfile_importer_id,
                modules_dir: &state.config.modules_dir,
                include,
                package_queries: &self.package_names,
                depth: self.depth,
                long: self.long,
            },
            report_as,
        )?;
        println!("{output}");
        Ok(())
    }
}
