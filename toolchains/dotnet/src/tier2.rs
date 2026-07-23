use crate::config::DotnetToolchainConfig;
use extism_pdk::*;
use moon_pdk::{get_host_env_var, get_host_environment, parse_toolchain_config};
use moon_pdk_api::*;

/// Resolve the DOTNET_ROOT to inject into task environments.
/// Order: explicit config > existing host env var > `~/.dotnet` when it
/// contains an actual SDK layout (the proto dotnet plugin installs there).
fn resolve_dotnet_root(config: &DotnetToolchainConfig) -> AnyResult<Option<String>> {
    if let Some(root) = &config.dotnet_root {
        return Ok(Some(root.clone()));
    }

    if let Some(existing) = get_host_env_var("DOTNET_ROOT")? {
        if !existing.is_empty() {
            return Ok(Some(existing));
        }
    }

    let env = get_host_environment()?;
    let candidate = env.home_dir.join(".dotnet");

    // `~/.dotnet` doubles as the dotnet CLI's user-level cache directory, so
    // mere existence is not enough — require the `dotnet` host executable,
    // which a real SDK install (e.g. via the proto dotnet plugin) provides.
    let exe = if env.os.is_windows() {
        "dotnet.exe"
    } else {
        "dotnet"
    };

    if candidate.join(exe).exists() {
        if let Some(real) = candidate.real_path() {
            return Ok(Some(real.to_string_lossy().to_string()));
        }
    }

    Ok(None)
}

#[plugin_fn]
pub fn extend_task_command(
    Json(input): Json<ExtendTaskCommandInput>,
) -> FnResult<Json<ExtendTaskCommandOutput>> {
    let config = parse_toolchain_config::<DotnetToolchainConfig>(input.toolchain_config)?;
    let mut output = ExtendTaskCommandOutput::default();

    if let Some(root) = resolve_dotnet_root(&config)? {
        output.env.insert("DOTNET_ROOT".into(), root.clone());
        output.paths.push(root.into());
        // Opt out of telemetry noise in CI task runs.
        output
            .env
            .insert("DOTNET_CLI_TELEMETRY_OPTOUT".into(), "1".into());
    }

    Ok(Json(output))
}
