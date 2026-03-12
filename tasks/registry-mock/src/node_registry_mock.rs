use crate::{registry_mock, workspace_root};
use pipe_trait::Pipe;
use std::{env, iter, path::PathBuf, process::Command, sync::OnceLock};
use which::which_in;

#[derive(Debug, Clone)]
enum RegistryMockCommand {
    Binary(PathBuf),
    CargoRun,
}

static NODE_REGISTRY_MOCK: OnceLock<RegistryMockCommand> = OnceLock::new();

fn init() -> RegistryMockCommand {
    let bin = registry_mock().join("node_modules").join(".bin");
    let paths = env::var_os("PATH")
        .unwrap_or_default()
        .pipe_ref(env::split_paths)
        .chain(iter::once(bin))
        .pipe(env::join_paths)
        .expect("append node_modules/.bin to PATH");

    which_in("registry-mock", Some(paths), ".")
        .map(RegistryMockCommand::Binary)
        .unwrap_or(RegistryMockCommand::CargoRun)
}

pub fn node_registry_mock_command() -> Command {
    match NODE_REGISTRY_MOCK.get_or_init(init) {
        RegistryMockCommand::Binary(path) => Command::new(path),
        RegistryMockCommand::CargoRun => {
            let mut command = Command::new(env!("CARGO"));
            command
                .current_dir(workspace_root())
                .arg("run")
                .arg("--bin=pacquet-registry-mock")
                .arg("--");
            command
        }
    }
}
