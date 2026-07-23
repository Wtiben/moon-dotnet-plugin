use moon_pdk_api::*;
use moon_pdk_test_utils::create_empty_moon_sandbox;
use serde_json::json;

mod dotnet_toolchain_tier1 {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn registers_and_reports_docker_image() {
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
    }
}
