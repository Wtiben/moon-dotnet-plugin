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
        async fn finds_slnx_root_from_nested_dir() {
            let sandbox = create_empty_moon_sandbox();
            // .slnx is a marker only — content is never parsed.
            sandbox.create_file("App.slnx", "<Solution>\n</Solution>\n");
            sandbox.create_file(
                "nested/proj/Proj.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\" />",
            );

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
        async fn falls_back_to_alternate_lock_file_name() {
            let sandbox = create_empty_moon_sandbox();
            sandbox.create_file(
                "proj/packages.Proj.lock.json",
                r#"{"version": 1, "dependencies": {}}"#,
            );

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
            input.toolchain_config = json!({ "inferDependencies": true, "inferTasks": false });

            let output = plugin.extend_project_graph(input).await;

            let app = &output.extended_projects[&Id::raw("app")];
            assert_eq!(app.dependencies.len(), 1);
            assert_eq!(app.dependencies[0].id, Id::raw("lib"));
            assert_eq!(app.dependencies[0].scope, DependencyScope::Production);

            let lib = &output.extended_projects[&Id::raw("lib")];
            assert_eq!(lib.dependencies[0].id, Id::raw("core"));

            // core has no references, but still contributes its
            // AssemblyName-derived alias.
            let core = &output.extended_projects[&Id::raw("core")];
            assert!(core.dependencies.is_empty());
            assert_eq!(core.alias.as_deref(), Some("Core"));

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
            input.toolchain_config = json!({ "inferDependencies": false, "inferTasks": false });

            let output = plugin.extend_project_graph(input).await;

            assert!(output.extended_projects.is_empty());
        }

        fn task_ids(project: &ExtendProjectOutput) -> Vec<&str> {
            project.tasks.keys().map(|id| id.as_str()).collect()
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn infers_tasks_by_default() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = projects_input();
            input.toolchain_config = json!({});

            let output = plugin.extend_project_graph(input).await;

            // app is an Exe -> build + publish + run.
            let app = &output.extended_projects[&Id::raw("app")];
            assert_eq!(task_ids(app), vec!["build", "publish", "run"]);

            let build = &app.tasks[&Id::raw("build")];
            assert_eq!(
                build.command,
                Some(moon_config::PartialTaskArgs::List(vec![
                    "dotnet".into(),
                    "build".into(),
                    "--no-restore".into(),
                    "--no-dependencies".into(),
                    "-c".into(),
                    "Debug".into(),
                ]))
            );
            // Outputs came from the real evaluated BaseOutputPath.
            assert_eq!(
                build.outputs,
                Some(vec![moon_config::Output::parse("bin").unwrap()])
            );
            assert!(build.deps.is_some(), "build depends on ^:build");

            // run is never cached and never runs in CI.
            let run_options = app.tasks[&Id::raw("run")].options.as_ref().unwrap();
            assert_eq!(
                run_options.cache,
                Some(moon_config::TaskOptionCache::Enabled(false))
            );

            // app-tests references Microsoft.NET.Test.Sdk -> build + test.
            let tests = &output.extended_projects[&Id::raw("app-tests")];
            assert_eq!(task_ids(tests), vec!["build", "test"]);

            // Plain classlibs still get a build task.
            let core = &output.extended_projects[&Id::raw("core")];
            assert_eq!(task_ids(core), vec!["build"]);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn infers_only_listed_tasks() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = projects_input();
            input.toolchain_config =
                json!({ "inferDependencies": false, "inferTasks": ["test"] });

            let output = plugin.extend_project_graph(input).await;

            // Only app-tests qualifies for a test task; nothing else
            // contributes anything.
            let tests = &output.extended_projects[&Id::raw("app-tests")];
            assert_eq!(task_ids(tests), vec!["test"]);

            // The others still appear, but only to contribute their
            // AssemblyName alias — no tasks, and no deps with inference off.
            for id in ["app", "core", "lib"] {
                let project = &output.extended_projects[&Id::raw(id)];

                assert!(project.tasks.is_empty(), "{id} tasks");
                assert!(project.dependencies.is_empty(), "{id} deps");
                assert!(project.alias.is_some(), "{id} alias");
            }

        }

        #[tokio::test(flavor = "multi_thread")]
        async fn inference_yields_to_inherited_task_files() {
            let sandbox = create_moon_sandbox("projects");

            // Applies to dotnet projects: suppresses inferred `build`.
            sandbox.create_file(
                ".moon/tasks/dotnet.yml",
                "inheritedBy:\n  toolchains: ['dotnet']\ntasks:\n  build:\n    command: 'dotnet build'\n",
            );
            // Unscoped: assumed to apply -> suppresses inferred `publish`.
            sandbox.create_file(
                ".moon/tasks.yml",
                "tasks:\n  publish:\n    command: 'echo deploy'\n",
            );
            // Explicitly scoped to another toolchain: must NOT suppress `run`.
            sandbox.create_file(
                ".moon/tasks/node.yml",
                "inheritedBy:\n  toolchains: ['javascript']\ntasks:\n  run:\n    command: 'node server.js'\n",
            );

            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = projects_input();
            input.toolchain_config = json!({ "inferDependencies": false });

            let output = plugin.extend_project_graph(input).await;

            let app = &output.extended_projects[&Id::raw("app")];
            assert_eq!(task_ids(app), vec!["run"]);

            let tests = &output.extended_projects[&Id::raw("app-tests")];
            assert_eq!(task_ids(tests), vec!["test"]);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn infers_dependencies_across_languages() {
            let sandbox = create_moon_sandbox("mixed-lang");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = ExtendProjectGraphInput::default();
            input.project_sources.insert(Id::raw("app"), "app".into());
            input.project_sources.insert(Id::raw("lib"), "lib".into());
            input.project_sources.insert(Id::raw("core"), "core".into());
            input.toolchain_config = json!({ "inferDependencies": true });

            let output = plugin.extend_project_graph(input).await;

            // C# -> F# -> VB project references all resolve; MSBuild
            // evaluation is language-agnostic.
            let app = &output.extended_projects[&Id::raw("app")];
            assert_eq!(app.dependencies[0].id, Id::raw("lib"));

            let lib = &output.extended_projects[&Id::raw("lib")];
            assert_eq!(lib.dependencies[0].id, Id::raw("core"));

            assert!(
                output
                    .input_files
                    .contains(&PathBuf::from("/workspace/lib/Lib.fsproj"))
            );
            assert!(
                output
                    .input_files
                    .contains(&PathBuf::from("/workspace/core/Core.vbproj"))
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn emits_assembly_name_as_alias() {
            let sandbox = create_moon_sandbox("matrix");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = ExtendProjectGraphInput::default();
            input
                .project_sources
                .insert(Id::raw("deep"), "nested/deep".into());
            input.toolchain_config = json!({ "inferDependencies": true });

            let output = plugin.extend_project_graph(input).await;

            // Explicit <AssemblyName> beats the file-name default.
            let deep = &output.extended_projects[&Id::raw("deep")];
            assert_eq!(deep.alias.as_deref(), Some("MyCompany.Deep"));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn condition_gated_project_references_resolve() {
            let sandbox = create_moon_sandbox("matrix");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut input = ExtendProjectGraphInput::default();
            input
                .project_sources
                .insert(Id::raw("deep"), "nested/deep".into());
            input.project_sources.insert(Id::raw("cond"), "cond".into());
            input.toolchain_config = json!({ "inferDependencies": true });

            let output = plugin.extend_project_graph(input).await;

            // The ProjectReference is gated on '$(EnableDeepRef)' == '1',
            // set in the project itself — real evaluation resolves it.
            let cond = &output.extended_projects[&Id::raw("cond")];
            assert_eq!(cond.dependencies[0].id, Id::raw("deep"));
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

    mod install_dependencies {
        use super::*;

        #[tokio::test(flavor = "multi_thread")]
        async fn plain_restore_without_lockfile() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .install_dependencies(InstallDependenciesInput {
                    root: VirtualPath::Real(sandbox.path().into()),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let command = output.install_command.unwrap().command;
            assert_eq!(command.command, "dotnet");
            assert_eq!(command.args, vec!["restore".to_string()]);
            assert!(output.dedupe_command.is_none());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn locked_mode_when_lockfile_present() {
            let sandbox = create_moon_sandbox("locked");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .install_dependencies(InstallDependenciesInput {
                    root: VirtualPath::Real(sandbox.path().into()),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let command = output.install_command.unwrap().command;
            assert_eq!(
                command.args,
                vec!["restore".to_string(), "--locked-mode".to_string()]
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn locked_mode_with_alternate_lock_name() {
            let sandbox = create_empty_moon_sandbox();
            sandbox.create_file(
                "proj/packages.Proj.lock.json",
                r#"{"version": 1, "dependencies": {}}"#,
            );

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .install_dependencies(InstallDependenciesInput {
                    root: VirtualPath::Real(sandbox.path().into()),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let command = output.install_command.unwrap().command;
            assert_eq!(
                command.args,
                vec!["restore".to_string(), "--locked-mode".to_string()]
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn appends_restore_args() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .install_dependencies(InstallDependenciesInput {
                    root: VirtualPath::Real(sandbox.path().into()),
                    toolchain_config: json!({ "restoreArgs": ["--verbosity", "minimal"] }),
                    ..Default::default()
                })
                .await;

            let command = output.install_command.unwrap().command;
            assert_eq!(
                command.args,
                vec![
                    "restore".to_string(),
                    "--verbosity".to_string(),
                    "minimal".to_string()
                ]
            );
        }
    }

    mod parse_lock {
        use super::*;

        #[tokio::test(flavor = "multi_thread")]
        async fn parses_generated_lockfile() {
            let sandbox = create_moon_sandbox("locked");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .parse_lock(ParseLockInput {
                    path: VirtualPath::Real(
                        sandbox.path().join("proj/packages.lock.json"),
                    ),
                    root: VirtualPath::Real(sandbox.path().into()),
                    ..Default::default()
                })
                .await;

            let newtonsoft = &output.dependencies["Newtonsoft.Json"];
            assert_eq!(newtonsoft.len(), 1);
            assert_eq!(
                newtonsoft[0].version.as_ref().unwrap().to_string(),
                "13.0.3"
            );
            assert!(
                newtonsoft[0]
                    .hash
                    .as_deref()
                    .unwrap()
                    .starts_with("HrC5")
            );
        }
    }

    mod parse_manifest {
        use super::*;

        #[tokio::test(flavor = "multi_thread")]
        async fn parses_package_references_from_a_project_file() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .parse_manifest(ParseManifestInput {
                    path: VirtualPath::Real(sandbox.path().join("app-tests/App.Tests.csproj")),
                    root: VirtualPath::Real(sandbox.path().into()),
                    ..Default::default()
                })
                .await;

            assert_eq!(
                output.dependencies["xunit"]
                    .get_version()
                    .unwrap()
                    .to_string(),
                "2.8.0"
            );
            assert_eq!(
                output.dependencies["Microsoft.NET.Test.Sdk"]
                    .get_version()
                    .unwrap()
                    .to_string(),
                "17.10.0"
            );
            // Test projects set IsPackable=false via the test SDK props, but
            // those only apply after a restore; unrestored evaluation leaves
            // it empty, which we report as not publishable.
            assert!(!output.publishable);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn versionless_references_are_inherited() {
            let sandbox = create_moon_sandbox("cpm");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .parse_manifest(ParseManifestInput {
                    path: VirtualPath::Real(sandbox.path().join("proj/Cpm.csproj")),
                    root: VirtualPath::Real(sandbox.path().into()),
                    ..Default::default()
                })
                .await;

            // Central Package Management: the version lives in
            // Directory.Packages.props, so moon resolves it from the
            // workspace manifest.
            let dep = &output.dependencies["Newtonsoft.Json"];
            assert!(dep.is_inherited());
            assert!(dep.get_version().is_none());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn parses_package_versions_from_directory_packages_props() {
            let sandbox = create_moon_sandbox("cpm");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .parse_manifest(ParseManifestInput {
                    path: VirtualPath::Real(sandbox.path().join("Directory.Packages.props")),
                    root: VirtualPath::Real(sandbox.path().into()),
                    ..Default::default()
                })
                .await;

            // The workspace manifest resolves what versionless project
            // references inherit.
            assert_eq!(
                output.dependencies["Newtonsoft.Json"]
                    .get_version()
                    .unwrap()
                    .to_string(),
                "13.0.3"
            );
            assert!(!output.publishable);
        }
    }

    mod setup_environment {
        use super::*;

        #[tokio::test(flavor = "multi_thread")]
        async fn no_commands_without_a_tool_manifest() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .setup_environment(SetupEnvironmentInput {
                    root: VirtualPath::Real(sandbox.path().into()),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            assert!(output.commands.is_empty());
            assert!(output.changed_files.is_empty());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn restores_local_tools_when_manifest_exists() {
            let sandbox = create_moon_sandbox("projects");
            sandbox.create_file(
                ".config/dotnet-tools.json",
                r#"{"version": 1, "isRoot": true, "tools": {}}"#,
            );

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .setup_environment(SetupEnvironmentInput {
                    root: VirtualPath::Real(sandbox.path().into()),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            assert_eq!(output.commands.len(), 1);

            let command = &output.commands[0];
            assert_eq!(command.command.command, "dotnet");
            assert_eq!(
                command.command.args,
                vec!["tool".to_string(), "restore".to_string()]
            );
            // The cache key embeds a digest of the manifest content, so a
            // manifest edit changes the declaration moon fingerprints this
            // action on (otherwise the restore would never re-run).
            let cache_key = command.cache.as_deref().unwrap();
            assert!(cache_key.starts_with("dotnet-tool-restore-"));
            assert_eq!(command.inputs.len(), 1);

            // Same content -> same key; different content -> different key.
            let repeat = plugin
                .setup_environment(SetupEnvironmentInput {
                    root: VirtualPath::Real(sandbox.path().into()),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            assert_eq!(repeat.commands[0].cache.as_deref(), Some(cache_key));

            sandbox.create_file(
                ".config/dotnet-tools.json",
                r#"{"version": 1, "isRoot": true, "tools": {"dotnetsay": {"version": "2.1.7", "commands": ["dotnetsay"]}}}"#,
            );

            let edited = plugin
                .setup_environment(SetupEnvironmentInput {
                    root: VirtualPath::Real(sandbox.path().into()),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            assert_ne!(edited.commands[0].cache.as_deref(), Some(cache_key));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn finds_tool_manifest_above_the_dependencies_root() {
            let sandbox = create_moon_sandbox("projects");
            // Tool manifests conventionally live at the repository root, but
            // any project directory with a lock file is its own dependencies
            // root — so the lookup walks upward like the dotnet CLI does.
            sandbox.create_file(
                ".config/dotnet-tools.json",
                r#"{"version": 1, "isRoot": true, "tools": {}}"#,
            );

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .setup_environment(SetupEnvironmentInput {
                    root: VirtualPath::Real(sandbox.path().join("app")),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            assert_eq!(output.commands.len(), 1);
            assert_eq!(
                output.commands[0].command.args,
                vec!["tool".to_string(), "restore".to_string()]
            );
        }
    }

    mod hash_task_contents {
        use super::*;

        fn fragment(id: &str, source: &str) -> moon_pdk_api::ProjectFragment {
            moon_pdk_api::ProjectFragment {
                id: Id::raw(id),
                source: source.into(),
                toolchains: vec![Id::raw("dotnet")],
                ..Default::default()
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn lockfile_branch_includes_raw_lock_text() {
            let sandbox = create_moon_sandbox("locked");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project: fragment("proj", "proj"),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            assert_eq!(output.contents.len(), 1);
            let lockfiles = output.contents[0]["lockfiles"].as_object().unwrap();
            let lock_text = lockfiles["/workspace/proj/packages.lock.json"]
                .as_str()
                .unwrap();
            assert!(lock_text.contains("Newtonsoft.Json"));
            assert!(lock_text.contains("contentHash"));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn lockfile_branch_still_hashes_config_files() {
            let sandbox = create_moon_sandbox("locked");
            // Even with the package set pinned by the lock file, props/targets
            // change build behavior and must contribute to the hash.
            sandbox.create_file(
                "Directory.Build.props",
                "<Project><PropertyGroup><LangVersion>12</LangVersion></PropertyGroup></Project>",
            );

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project: fragment("proj", "proj"),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let contents = &output.contents[0];
            assert!(contents["lockfiles"].is_object());
            let configs = contents["configs"].as_object().unwrap();
            assert!(
                configs["/workspace/Directory.Build.props"]
                    .as_str()
                    .unwrap()
                    .contains("LangVersion")
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn alternate_lock_file_name_takes_lock_branch() {
            let sandbox = create_moon_sandbox("projects");
            // `packages.<project>.lock.json` via NuGetLockFilePath.
            sandbox.create_file(
                "app/packages.App.lock.json",
                r#"{"version": 1, "dependencies": {}}"#,
            );

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project: fragment("app", "app"),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let contents = &output.contents[0];
            let lockfiles = contents["lockfiles"].as_object().unwrap();
            assert!(lockfiles.contains_key("/workspace/app/packages.App.lock.json"));
            // Lock branch: no MSBuild evaluation happens.
            assert!(contents.get("packages").is_none());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn hashes_all_config_file_kinds() {
            let sandbox = create_moon_sandbox("projects");
            // Valid-but-harmless contents: MSBuild auto-imports
            // Directory.Build.targets and auto-applies Directory.Build.rsp,
            // so garbage would break evaluation of the fixture projects.
            sandbox.create_file("core/Directory.Build.targets", "<Project />");
            sandbox.create_file("Directory.Build.rsp", "");
            sandbox.create_file("NuGet.Config", "<configuration />");
            sandbox.create_file("global.json", "{}");

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project: fragment("core", "core"),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let configs = output.contents[0]["configs"].as_object().unwrap();
            assert!(configs.contains_key("/workspace/core/Directory.Build.targets"));
            assert!(configs.contains_key("/workspace/Directory.Build.rsp"));
            // Actual (non-lowercase) file name is preserved in the key.
            assert!(configs.contains_key("/workspace/NuGet.Config"));
            assert!(configs.contains_key("/workspace/global.json"));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn evaluated_packages_branch_without_lockfile() {
            let sandbox = create_moon_sandbox("projects");
            sandbox.create_file(
                "Directory.Build.props",
                "<Project><PropertyGroup><LangVersion>latest</LangVersion></PropertyGroup></Project>",
            );

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project: fragment("app-tests", "app-tests"),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            assert_eq!(output.contents.len(), 1);
            let contents = &output.contents[0];

            assert_eq!(contents["packages"]["xunit"].as_str().unwrap(), "2.8.0");
            assert_eq!(
                contents["packages"]["Microsoft.NET.Test.Sdk"]
                    .as_str()
                    .unwrap(),
                "17.10.0"
            );

            let configs = contents["configs"].as_object().unwrap();
            assert_eq!(configs.len(), 1);
            assert!(
                configs
                    .values()
                    .next()
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .contains("LangVersion")
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn props_inheritance_chain_hashes_every_level() {
            let sandbox = create_moon_sandbox("matrix");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project: fragment("deep", "nested/deep"),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let contents = &output.contents[0];
            let configs = contents["configs"].as_object().unwrap();

            // Every props file from the project dir up to the workspace root
            // is content-hashed, not just the nearest one.
            assert!(configs.contains_key("/workspace/nested/Directory.Build.props"));
            assert!(configs.contains_key("/workspace/Directory.Build.props"));

            // The nested props chains to the root props via
            // GetPathOfFileAbove, so packages from both levels evaluate in.
            let packages = contents["packages"].as_object().unwrap();
            assert_eq!(packages["NestedPkg"].as_str().unwrap(), "2.0.0");
            assert_eq!(packages["RootPkg"].as_str().unwrap(), "1.0.0");
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn multi_targeted_project_hashes_the_outer_build() {
            let sandbox = create_moon_sandbox("matrix");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project: fragment("multi", "multi"),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let packages = output.contents[0]["packages"].as_object().unwrap();

            // Documented scope cut: evaluation is the outer (cross-targeting)
            // build, where TargetFramework is empty — so per-TFM conditional
            // packages are invisible. The root props package still resolves,
            // proving the project itself evaluated.
            assert!(packages.contains_key("RootPkg"));
            assert!(!packages.contains_key("Net8OnlyPkg"));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn condition_gated_packages_resolve_by_evaluation() {
            let sandbox = create_moon_sandbox("matrix");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project: fragment("cond", "cond"),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let packages = output.contents[0]["packages"].as_object().unwrap();

            // Conditions are resolved by MSBuild, so a true condition
            // contributes and a false one does not.
            assert_eq!(packages["ExtraPkg"].as_str().unwrap(), "3.0.0");
            assert!(!packages.contains_key("NeverPkg"));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn central_package_management_hashes_via_props() {
            let sandbox = create_moon_sandbox("cpm");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project: fragment("proj", "proj"),
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let contents = &output.contents[0];

            // CPM applies versions during restore, not evaluation, so the
            // versionless PackageReference surfaces as "*" — the pinned
            // version reaches the hash through the Directory.Packages.props
            // content below, which is what keeps caching correct.
            assert_eq!(contents["packages"]["Newtonsoft.Json"].as_str().unwrap(), "*");

            let configs = contents["configs"].as_object().unwrap();
            assert!(
                configs["/workspace/Directory.Packages.props"]
                    .as_str()
                    .unwrap()
                    .contains("13.0.3")
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn skips_projects_without_dotnet_toolchain() {
            let sandbox = create_moon_sandbox("projects");
            let plugin = sandbox.create_toolchain("dotnet").await;

            let mut project = fragment("app", "app");
            project.toolchains = vec![];

            let output = plugin
                .hash_task_contents(HashTaskContentsInput {
                    project,
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            assert!(output.contents.is_empty());
        }
    }

    mod prune_docker {
        use super::*;

        #[tokio::test(flavor = "multi_thread")]
        async fn removes_bin_and_obj_dirs() {
            let sandbox = create_empty_moon_sandbox();
            sandbox.create_file("app/bin/Debug/x.dll", "");
            sandbox.create_file("app/obj/project.assets.json", "");
            sandbox.create_file("app/keep.cs", "");

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .prune_docker(PruneDockerInput {
                    projects: vec![moon_pdk_api::ProjectFragment {
                        id: Id::raw("app"),
                        source: "app".into(),
                        ..Default::default()
                    }],
                    root: VirtualPath::Real(sandbox.path().into()),
                    ..Default::default()
                })
                .await;

            assert!(!sandbox.path().join("app/bin").exists());
            assert!(!sandbox.path().join("app/obj").exists());
            assert!(sandbox.path().join("app/keep.cs").exists());

            assert_eq!(
                output.changed_files,
                vec![
                    PathBuf::from("/workspace/app/bin"),
                    PathBuf::from("/workspace/app/obj"),
                ]
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn does_nothing_without_bin_obj() {
            let sandbox = create_empty_moon_sandbox();
            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .prune_docker(PruneDockerInput {
                    root: VirtualPath::Real(sandbox.path().into()),
                    ..Default::default()
                })
                .await;

            assert!(output.changed_files.is_empty());
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

            // An ambient DOTNET_ROOT (e.g. set by actions/setup-dotnet on CI
            // runners) legitimately takes precedence over the home-dir
            // fallback; only assert the fallback value when none is set.
            match std::env::var("DOTNET_ROOT") {
                Ok(ambient) if !ambient.is_empty() => assert_eq!(root, &ambient),
                _ => assert!(root.contains(".dotnet")),
            }

            assert_eq!(output.paths.len(), 1);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn skips_home_fallback_that_cannot_satisfy_global_json() {
            let sandbox = create_moon_sandbox("projects");

            // A leftover ~/.dotnet holding only SDK 8 — the exact shape that
            // made every task fail against a 10.x pin in a real repo.
            let exe = if cfg!(windows) { "dotnet.exe" } else { "dotnet" };
            sandbox.create_file(format!(".home/.dotnet/{exe}").as_str(), "");
            sandbox.create_file(".home/.dotnet/sdk/8.0.423/marker", "");
            sandbox.create_file(
                "global.json",
                r#"{"sdk":{"version":"10.0.301","rollForward":"latestMajor"}}"#,
            );

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .extend_task_command(ExtendTaskCommandInput {
                    project: moon_pdk_api::ProjectFragment {
                        id: Id::raw("app"),
                        source: "app".into(),
                        toolchains: vec![Id::raw("dotnet")],
                        ..Default::default()
                    },
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            // An ambient DOTNET_ROOT (actions/setup-dotnet sets one on CI
            // runners) legitimately wins over the fallback and never reaches
            // the guard, so only assert when none is set. The satisfaction
            // rules themselves are unit-tested in `global_json`.
            match std::env::var("DOTNET_ROOT") {
                Ok(ambient) if !ambient.is_empty() => {
                    assert_eq!(output.env.get("DOTNET_ROOT").unwrap(), &ambient);
                }
                _ => {
                    assert!(
                        output.env.get("DOTNET_ROOT").is_none(),
                        "an SDK-8-only ~/.dotnet must not be injected for a 10.x pin"
                    );
                    assert!(output.paths.is_empty());
                }
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn uses_home_fallback_that_satisfies_global_json() {
            let sandbox = create_moon_sandbox("projects");

            let exe = if cfg!(windows) { "dotnet.exe" } else { "dotnet" };
            sandbox.create_file(format!(".home/.dotnet/{exe}").as_str(), "");
            sandbox.create_file(".home/.dotnet/sdk/10.0.301/marker", "");
            sandbox.create_file(
                "global.json",
                r#"{"sdk":{"version":"10.0.301","rollForward":"latestMajor"}}"#,
            );

            let plugin = sandbox.create_toolchain("dotnet").await;

            let output = plugin
                .extend_task_command(ExtendTaskCommandInput {
                    project: moon_pdk_api::ProjectFragment {
                        id: Id::raw("app"),
                        source: "app".into(),
                        toolchains: vec![Id::raw("dotnet")],
                        ..Default::default()
                    },
                    toolchain_config: json!({}),
                    ..Default::default()
                })
                .await;

            let root = output.env.get("DOTNET_ROOT").expect("DOTNET_ROOT not set");

            match std::env::var("DOTNET_ROOT") {
                Ok(ambient) if !ambient.is_empty() => assert_eq!(root, &ambient),
                _ => assert!(root.contains(".dotnet")),
            }
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
