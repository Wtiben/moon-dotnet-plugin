use moon_pdk_api::*;
use moon_pdk_test_utils::create_empty_moon_sandbox;
use serde_json::json;

mod dotnet_toolchain_tier2 {
    use super::*;

    mod extend_task_command {
        use super::*;

        #[tokio::test(flavor = "multi_thread")]
        async fn injects_explicit_dotnet_root() {
            let sandbox = create_empty_moon_sandbox();
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .extend_task_command(ExtendTaskCommandInput {
                    toolchain_config: json!({ "dotnetRoot": "/custom/dotnet" }),
                    ..Default::default()
                })
                .await;

            assert_eq!(output.env.get("DOTNET_ROOT").unwrap(), "/custom/dotnet");
            assert_eq!(
                output.env.get("DOTNET_CLI_TELEMETRY_OPTOUT").unwrap(),
                "1"
            );
            assert_eq!(output.paths, vec![std::path::PathBuf::from("/custom/dotnet")]);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn falls_back_to_home_dotnet_when_sdk_layout_present() {
            let sandbox = create_empty_moon_sandbox();

            // A real SDK layout has the dotnet host executable at the root.
            let exe = if cfg!(windows) { "dotnet.exe" } else { "dotnet" };
            sandbox.create_file(format!(".home/.dotnet/{exe}").as_str(), "");

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .extend_task_command(ExtendTaskCommandInput {
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let root = output.env.get("DOTNET_ROOT").expect("DOTNET_ROOT not set");
            assert!(root.contains(".dotnet"));
            assert_eq!(output.paths.len(), 1);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn no_injection_without_any_dotnet_root() {
            let sandbox = create_empty_moon_sandbox();

            // `~/.dotnet` existing as a mere cache dir (no dotnet executable)
            // must NOT be treated as a DOTNET_ROOT.
            sandbox.create_file(".home/.dotnet/sdk/8.0.404/marker", "");

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .extend_task_command(ExtendTaskCommandInput {
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            // The host DOTNET_ROOT env var may leak in from the dev machine;
            // only assert the cache-dir case when it is not set there.
            if std::env::var("DOTNET_ROOT").is_err() {
                assert!(output.env.get("DOTNET_ROOT").is_none());
                assert!(output.paths.is_empty());
            }
        }
    }
}
