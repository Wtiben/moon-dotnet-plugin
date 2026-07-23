use extism_pdk::*;
use moon_pdk_api::*;

#[plugin_fn]
pub fn register_toolchain(
    Json(_): Json<RegisterToolchainInput>,
) -> FnResult<Json<RegisterToolchainOutput>> {
    Ok(Json(RegisterToolchainOutput {
        name: ".NET".into(),
        plugin_version: env!("CARGO_PKG_VERSION").into(),
        ..Default::default()
    }))
}

#[plugin_fn]
pub fn define_docker_metadata(
    Json(_): Json<DefineDockerMetadataInput>,
) -> FnResult<Json<DefineDockerMetadataOutput>> {
    Ok(Json(DefineDockerMetadataOutput {
        default_image: Some("mcr.microsoft.com/dotnet/sdk:latest".into()),
        scaffold_globs: vec![],
    }))
}
