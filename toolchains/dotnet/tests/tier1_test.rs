use moon_pdk_api::*;
use moon_pdk_test_utils::create_empty_moon_sandbox;
use serde_json::json;

mod dotnet_toolchain_tier1 {
    use super::*;

    mod register_toolchain {
        use super::*;

        #[tokio::test(flavor = "multi_thread")]
        async fn registers_metadata() {
            let sandbox = create_empty_moon_sandbox();
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .register_toolchain(RegisterToolchainInput {
                    id: Id::raw("dotnet"),
                })
                .await;

            assert_eq!(output.name, ".NET");
            assert_eq!(output.exe_names, vec!["dotnet".to_string()]);
            assert_eq!(output.lock_file_names, vec!["packages.lock.json".to_string()]);
            assert!(output.manifest_file_names.is_empty());
            assert!(output.vendor_dir_name.is_none());
            assert!(
                output
                    .config_file_globs
                    .contains(&"*.{csproj,fsproj,vbproj}".to_string())
            );
            assert!(
                output
                    .config_file_globs
                    .contains(&"Directory.Build.targets".to_string())
            );
            assert!(
                output
                    .config_file_globs
                    .contains(&"{nuget,NuGet}.{config,Config}".to_string())
            );
            assert!(
                output
                    .config_file_globs
                    .contains(&"packages.*.lock.json".to_string())
            );
        }
    }

    mod define_docker_metadata {
        use super::*;

        #[tokio::test(flavor = "multi_thread")]
        async fn docker_metadata_defaults() {
            let sandbox = create_empty_moon_sandbox();
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .define_docker_metadata(DefineDockerMetadataInput {
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            assert_eq!(
                output.default_image.unwrap(),
                "mcr.microsoft.com/dotnet/sdk:latest"
            );
            assert!(
                output
                    .scaffold_globs
                    .contains(&"**/*.{csproj,fsproj,vbproj}".to_string())
            );
            assert!(
                output
                    .scaffold_globs
                    .contains(&"**/packages.lock.json".to_string())
            );
            assert!(output.scaffold_globs.contains(&"**/*.targets".to_string()));
            assert!(
                output
                    .scaffold_globs
                    .contains(&"**/packages.*.lock.json".to_string())
            );
        }
    }
}
