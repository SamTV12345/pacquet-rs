use crate::resolve_package_version_from_tarball_spec;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_registry::PackageVersion;

pub(crate) fn is_git_spec(spec: &str) -> bool {
    spec.starts_with("github:")
        || spec.starts_with("git+https://github.com/")
        || spec.starts_with("https://github.com/")
        || spec.starts_with("git+ssh://git@github.com/")
        || spec.starts_with("git+ssh://git@github.com:")
        || is_github_shorthand_spec(spec)
}

fn split_ref(spec: &str) -> (&str, &str) {
    match spec.split_once('#') {
        Some((repo, reference)) if !reference.is_empty() => (repo, reference),
        _ => (spec, "HEAD"),
    }
}

fn normalize_repo(repo: &str) -> String {
    repo.trim_end_matches(".git").trim_end_matches('/').to_string()
}

fn is_github_shorthand_spec(spec: &str) -> bool {
    if spec.starts_with('@')
        || spec.contains("://")
        || spec.starts_with('.')
        || spec.starts_with('/')
    {
        return false;
    }

    let repo = spec.split_once('#').map_or(spec, |(repo, _)| repo);
    let mut segments = repo.split('/').filter(|segment| !segment.is_empty());
    let Some(owner) = segments.next() else {
        return false;
    };
    let Some(repo_name) = segments.next() else {
        return false;
    };
    !owner.is_empty() && !repo_name.is_empty() && segments.next().is_none()
}

fn github_repo_from_spec(spec: &str) -> Option<(String, String)> {
    if let Some(repo_and_ref) = spec.strip_prefix("github:") {
        let (repo, reference) = split_ref(repo_and_ref);
        let repo = normalize_repo(repo);
        if repo.split('/').count() >= 2 {
            return Some((repo, reference.to_string()));
        }
        return None;
    }

    if is_github_shorthand_spec(spec) {
        let (repo, reference) = split_ref(spec);
        return Some((normalize_repo(repo), reference.to_string()));
    }

    let normalized = if let Some(value) = spec.strip_prefix("git+https://github.com/") {
        value
    } else if let Some(value) = spec.strip_prefix("https://github.com/") {
        value
    } else if let Some(value) = spec.strip_prefix("git+ssh://git@github.com/") {
        value
    } else if let Some(value) = spec.strip_prefix("git+ssh://git@github.com:") {
        value
    } else {
        return None;
    };

    let (repo_path, reference) = split_ref(normalized);
    let mut segments = repo_path.split('/').filter(|segment| !segment.is_empty());
    let owner = segments.next()?;
    let repo = segments.next()?;
    let repo = normalize_repo(&format!("{owner}/{repo}"));
    Some((repo, reference.to_string()))
}

pub(crate) fn git_spec_to_tarball_url(spec: &str) -> Option<String> {
    let (repo, reference) = github_repo_from_spec(spec)?;
    Some(format!("https://codeload.github.com/{repo}/tar.gz/{reference}"))
}

pub(crate) fn normalize_git_spec(spec: &str) -> Option<String> {
    let (repo, reference) = github_repo_from_spec(spec)?;
    Some(if reference == "HEAD" {
        format!("github:{repo}")
    } else {
        format!("github:{repo}#{reference}")
    })
}

pub(crate) async fn resolve_package_version_from_git_spec(
    config: &Npmrc,
    http_client: &ThrottledClient,
    git_spec: &str,
) -> Result<PackageVersion, String> {
    let tarball_url = git_spec_to_tarball_url(git_spec).ok_or_else(|| {
        format!(
            "unsupported git spec: {git_spec}. currently supported: github:, owner/repo shorthand, and github.com URLs"
        )
    })?;
    resolve_package_version_from_tarball_spec(config, http_client, &tarball_url).await
}

#[cfg(test)]
mod tests {
    use super::{git_spec_to_tarball_url, is_git_spec, normalize_git_spec};
    use pretty_assertions::assert_eq;

    #[test]
    fn detects_supported_git_specs() {
        assert!(is_git_spec("pnpm/pnpm"));
        assert!(is_git_spec("pnpm/pnpm#main"));
        assert!(is_git_spec("github:pnpm/pnpm"));
        assert!(is_git_spec("github:pnpm/pnpm#main"));
        assert!(is_git_spec("git+https://github.com/pnpm/pnpm.git#main"));
        assert!(is_git_spec("https://github.com/pnpm/pnpm.git#main"));
        assert!(is_git_spec("git+ssh://git@github.com:pnpm/pnpm.git#main"));
        assert!(!is_git_spec("fastify@^4.0.0"));
    }

    #[test]
    fn converts_github_specs_to_codeload_tarball() {
        assert_eq!(
            git_spec_to_tarball_url("pnpm/pnpm"),
            Some("https://codeload.github.com/pnpm/pnpm/tar.gz/HEAD".to_string())
        );
        assert_eq!(
            git_spec_to_tarball_url("github:pnpm/pnpm"),
            Some("https://codeload.github.com/pnpm/pnpm/tar.gz/HEAD".to_string())
        );
        assert_eq!(
            git_spec_to_tarball_url("github:pnpm/pnpm#main"),
            Some("https://codeload.github.com/pnpm/pnpm/tar.gz/main".to_string())
        );
        assert_eq!(
            git_spec_to_tarball_url("git+https://github.com/pnpm/pnpm.git#v10.0.0"),
            Some("https://codeload.github.com/pnpm/pnpm/tar.gz/v10.0.0".to_string())
        );
        assert_eq!(
            git_spec_to_tarball_url("git+ssh://git@github.com:pnpm/pnpm.git#main"),
            Some("https://codeload.github.com/pnpm/pnpm/tar.gz/main".to_string())
        );
    }

    #[test]
    fn normalizes_git_specs_to_github_protocol() {
        assert_eq!(normalize_git_spec("pnpm/pnpm"), Some("github:pnpm/pnpm".to_string()));
        assert_eq!(
            normalize_git_spec("https://github.com/pnpm/pnpm.git#main"),
            Some("github:pnpm/pnpm#main".to_string())
        );
        assert_eq!(
            normalize_git_spec("git+ssh://git@github.com:pnpm/pnpm.git#main"),
            Some("github:pnpm/pnpm#main".to_string())
        );
    }
}
