use moon_config::DependencyScope;
use moon_pdk_api::*;
use moon_pdk_test_utils::{create_empty_moon_sandbox, create_moon_sandbox};
use serde_json::json;
use std::path::PathBuf;

mod dotnet_toolchain_tier2 {
    use super::*;

    mod locate_dependencies_root {
        use super::*;

        #[tokio::test(flavor = "multi_thread")]
        async fn finds_solution_root_from_nested_dir() {
            let sandbox = create_moon_sandbox("locate");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .locate_dependencies_root(LocateDependenciesRootInput {
                    starting_dir: VirtualPath::Real(sandbox.path().join("nested/proj")),
                    ..Default::default()
                })
                .await;

            assert_eq!(output.root.unwrap(), PathBuf::from("/workspace"));
            assert!(output.members.is_none());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn falls_back_to_project_file_dir_without_solution() {
            let sandbox = create_moon_sandbox("locate-no-sln");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .locate_dependencies_root(LocateDependenciesRootInput {
                    starting_dir: VirtualPath::Real(sandbox.path().join("proj")),
                    ..Default::default()
                })
                .await;

            assert_eq!(output.root.unwrap(), PathBuf::from("/workspace/proj"));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn returns_none_when_nothing_found() {
            let sandbox = create_empty_moon_sandbox();
            sandbox.create_file("empty/dir/marker.txt", "");

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .locate_dependencies_root(LocateDependenciesRootInput {
                    starting_dir: VirtualPath::Real(sandbox.path().join("empty/dir")),
                    ..Default::default()
                })
                .await;

            assert!(output.root.is_none());
        }
    }

    mod extend_project_graph {
        use super::*;

        fn projects_input() -> ExtendProjectGraphInput {
            let mut input = ExtendProjectGraphInput::default();
            input.project_sources.insert(Id::raw("app"), "app".into());
            input.project_sources.insert(Id::raw("lib"), "lib".into());
            input.project_sources.insert(Id::raw("core"), "core".into());
            input
                .project_sources
                .insert(Id::raw("app-tests"), "app-tests".into());
            input
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn maps_project_references_to_moon_deps() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = projects_input();
            input.toolchain_config = json!({ "inferDependencies": true });

            let output = plugin.extend_project_graph(input).await;

            let app = &output.extended_projects[&Id::raw("app")];
            assert_eq!(app.dependencies.len(), 1);
            assert_eq!(app.dependencies[0].id, Id::raw("lib"));
            assert_eq!(app.dependencies[0].scope, DependencyScope::Production);

            let lib = &output.extended_projects[&Id::raw("lib")];
            assert_eq!(lib.dependencies[0].id, Id::raw("core"));

            // core has no references at all, so it contributes nothing.
            assert!(!output.extended_projects.contains_key(&Id::raw("core")));

            let tests = &output.extended_projects[&Id::raw("app-tests")];
            assert_eq!(tests.dependencies[0].id, Id::raw("app"));

            // One csproj per project, virtual-path form.
            assert_eq!(output.input_files.len(), 4);
            assert!(
                output
                    .input_files
                    .contains(&PathBuf::from("/workspace/app/App.csproj"))
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn respects_infer_dependencies_off() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = projects_input();
            input.toolchain_config = json!({ "inferDependencies": false });

            let output = plugin.extend_project_graph(input).await;

            assert!(output.extended_projects.is_empty());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn infers_tasks_when_enabled() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = projects_input();
            input.toolchain_config =
                json!({ "inferDependencies": true, "inferTasks": true });

            let output = plugin.extend_project_graph(input).await;

            // app is an Exe -> run task.
            let app = &output.extended_projects[&Id::raw("app")];
            assert!(app.tasks.contains_key(&Id::raw("run")));

            // app-tests references Microsoft.NET.Test.Sdk -> IsTestProject -> test task.
            let tests = &output.extended_projects[&Id::raw("app-tests")];
            assert!(tests.tasks.contains_key(&Id::raw("test")));

            // core is a plain classlib -> no tasks, no deps.
            assert!(!output.extended_projects.contains_key(&Id::raw("core")));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn broken_project_does_not_abort_graph() {
            let sandbox = create_moon_sandbox("projects");
            sandbox.create_file("core/Core.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><broken");

            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = projects_input();
            input.toolchain_config = json!({ "inferDependencies": true });

            let output = plugin.extend_project_graph(input).await;

            // The other projects still resolve their dependencies.
            let app = &output.extended_projects[&Id::raw("app")];
            assert_eq!(app.dependencies[0].id, Id::raw("lib"));
        }
    }

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
