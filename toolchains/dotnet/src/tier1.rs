use crate::config::DotnetToolchainConfig;
use extism_pdk::*;
use moon_config::LanguageType;
use moon_pdk_api::*;
use schematic::SchemaBuilder;

#[plugin_fn]
pub fn register_toolchain(
    Json(_): Json<RegisterToolchainInput>,
) -> FnResult<Json<RegisterToolchainOutput>> {
    Ok(Json(RegisterToolchainOutput {
        name: ".NET".into(),
        description: Some(
            "Provides .NET SDK project-graph extraction, dependency install (dotnet restore), and Docker support for SDK-style C# projects.".into(),
        ),
        plugin_version: env!("CARGO_PKG_VERSION").into(),
        language: Some(LanguageType::CSharp),
        exe_names: vec!["dotnet".into()],
        config_file_globs: vec![
            "*.{csproj,fsproj,vbproj}".into(),
            "*.{sln,slnx}".into(),
            "global.json".into(),
            "Directory.Build.props".into(),
            "Directory.Packages.props".into(),
            "nuget.config".into(),
        ],
        // Project files (*.csproj) have variable names, which exact-name
        // manifest matching cannot express; detection is covered by
        // config_file_globs instead.
        manifest_file_names: vec![],
        lock_file_names: vec!["packages.lock.json".into()],
        // NuGet uses a global package cache, not an in-repo vendor dir.
        vendor_dir_name: None,
    }))
}

#[plugin_fn]
pub fn define_toolchain_config() -> FnResult<Json<DefineToolchainConfigOutput>> {
    Ok(Json(DefineToolchainConfigOutput {
        schema: SchemaBuilder::build_root::<DotnetToolchainConfig>(),
    }))
}

#[plugin_fn]
pub fn initialize_toolchain(
    Json(_): Json<InitializeToolchainInput>,
) -> FnResult<Json<InitializeToolchainOutput>> {
    Ok(Json(InitializeToolchainOutput {
        docs_url: Some("https://github.com/moon-dotnet-plugin/moon-dotnet-plugin#readme".into()),
        ..Default::default()
    }))
}

#[plugin_fn]
pub fn define_docker_metadata(
    Json(_): Json<DefineDockerMetadataInput>,
) -> FnResult<Json<DefineDockerMetadataOutput>> {
    Ok(Json(DefineDockerMetadataOutput {
        default_image: Some("mcr.microsoft.com/dotnet/sdk:latest".into()),
        scaffold_globs: vec![
            "**/*.{csproj,fsproj,vbproj}".into(),
            "**/*.{sln,slnx}".into(),
            "**/*.props".into(),
            "**/nuget.config".into(),
            "**/packages.lock.json".into(),
            "global.json".into(),
            // bin/obj contain generated *.props (obj/*.nuget.g.props) and
            // must never end up in the restore layer.
            "!**/bin/**".into(),
            "!**/obj/**".into(),
        ],
    }))
}
