use crate::InstallPackageBySnapshot;
use futures_util::future;
use pacquet_lockfile::{DependencyPath, PackageSnapshot};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pipe_trait::Pipe;
use std::collections::HashMap;

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
}

impl<'a> CreateVirtualStore<'a> {
    /// Execute the subroutine.
    pub async fn run(self) {
        let CreateVirtualStore { http_client, config, packages } = self;

        let packages = if let Some(packages) = packages {
            packages
        } else {
            return;
        };

        packages
            .iter()
            .map(|(dependency_path, package_snapshot)| async move {
                InstallPackageBySnapshot { http_client, config, dependency_path, package_snapshot }
                    .run()
                    .await
                    .unwrap(); // TODO: properly propagate this error
            })
            .pipe(future::join_all)
            .await;
    }
}
