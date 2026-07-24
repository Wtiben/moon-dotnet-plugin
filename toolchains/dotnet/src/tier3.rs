use crate::config::DotnetToolchainConfig;
use crate::dotnet_install::{
    exact_version, install_script_file_name, install_script_url, install_version_args,
};
use extism_pdk::*;
use moon_pdk::{
    exec, fetch_text, get_host_environment, into_virtual_path, parse_toolchain_config, plugin_err,
};
use moon_pdk_api::*;
use starbase_utils::fs;

#[plugin_fn]
pub fn setup_toolchain(
    Json(input): Json<SetupToolchainInput>,
) -> FnResult<Json<SetupToolchainOutput>> {
    let mut output = SetupToolchainOutput::default();

    // Without a `version:` setting moon skips the setup action entirely
    // ("use globals on PATH"); stay a no-op if called anyway.
    let Some(spec) = &input.configured_version else {
        return Ok(Json(output));
    };

    let config = parse_toolchain_config::<DotnetToolchainConfig>(input.toolchain_config)?;
    let env = get_host_environment()?;
    let windows = env.os.is_windows();

    // Install root: explicit `dotnetRoot` config wins, else `~/.dotnet` —
    // the same order resolve_dotnet_root uses when injecting DOTNET_ROOT
    // into task environments, so installed SDKs are picked up without any
    // further configuration. SDK versions install side-by-side.
    let install_root: std::path::PathBuf = match &config.dotnet_root {
        Some(root) => root.into(),
        None => {
            let Some(home) = env.home_dir.real_path() else {
                return Err(plugin_err!(
                    "Unable to resolve the host home directory for the default `~/.dotnet` install root."
                ));
            };

            home.join(".dotnet")
        }
    };

    let version_args = match install_version_args(spec, windows) {
        Ok(args) => args,
        Err(message) => return Err(plugin_err!("{}", message)),
    };

    // Fully-qualified versions can skip the network entirely when that SDK
    // is already laid out. Channels/aliases resolve server-side, so the
    // install script decides for those (it skips re-installs itself).
    if let Some(version) = exact_version(spec) {
        if into_virtual_path(install_root.join("sdk").join(&version))?.exists() {
            return Ok(Json(output));
        }
    }

    // Stage the official install script under moon's cache dir. Fetched
    // fresh on every run — the setup action itself is fingerprint-cached
    // by moon, so this executes rarely.
    let script_file = input
        .context
        .workspace_root
        .join(".moon/cache/dotnet-toolchain")
        .join(install_script_file_name(windows));

    if let Some(parent) = script_file.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write_file(&script_file, fetch_text(install_script_url(windows))?)?;

    let Some(script_path) = script_file.real_path() else {
        return Err(plugin_err!(
            "Unable to resolve a host path for the staged install script."
        ));
    };

    // `--no-path`: task environments get DOTNET_ROOT/PATH injected by
    // extend_task_command; the user's shell profile is left alone.
    let mut args: Vec<String> = if windows {
        vec![
            "-NoProfile".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-File".into(),
            script_path.to_string_lossy().to_string(),
            "-InstallDir".into(),
            install_root.to_string_lossy().to_string(),
            "-NoPath".into(),
        ]
    } else {
        vec![
            script_path.to_string_lossy().to_string(),
            "--install-dir".into(),
            install_root.to_string_lossy().to_string(),
            "--no-path".into(),
        ]
    };

    args.extend(version_args);

    let command = if windows { "powershell.exe" } else { "bash" };

    let mut operation = Operation::new("install-sdk")?;
    let result = exec(ExecCommandInput::pipe(command, args))?;

    if result.exit_code != 0 {
        operation.finish(OperationStatus::Failed);
        output.operations.push(operation);

        return Err(plugin_err!(
            "dotnet-install failed with exit code {}:\n{}\n{}",
            result.exit_code,
            result.stdout,
            result.stderr,
        ));
    }

    operation.finish(OperationStatus::Passed);
    output.operations.push(operation);
    // Informational only: for WASM-only toolchains the host currently
    // derives the action status itself and merges just operations/files.
    output.installed = true;

    Ok(Json(output))
}
