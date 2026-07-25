use crate::config::{INFERABLE_TASKS, InferTasksSetting};
use crate::msbuild::MsbuildEvaluation;
use moon_common::Id;
use moon_config::{
    Input, Output, PartialTaskArgs, PartialTaskConfig, PartialTaskDependency,
    PartialTaskDependencyConfig, PartialTaskOptionsConfig, TaskOptionCache, TaskOptionRunInCI,
};
use moon_pdk_api::{AnyResult, anyhow};
use moon_target::Target;
use std::collections::{BTreeMap, BTreeSet};

/// Everything task inference needs to know about one MSBuild project.
pub struct InferInputs<'a> {
    pub evaluation: &'a MsbuildEvaluation,

    /// Project file name to pass explicitly in commands when the project
    /// directory holds more than one MSBuild project file (bare `dotnet
    /// build` would otherwise error on ambiguity).
    pub explicit_project_file: Option<&'a str>,

    /// Host-real absolute path of the project directory (for making
    /// evaluated output paths project-relative). Forward or back slashes.
    pub project_dir: &'a str,

    /// Host-real absolute path of the workspace root.
    pub workspace_dir: &'a str,

    /// Whether the `global.json` governing this project selects
    /// Microsoft.Testing.Platform for `dotnet test`. A project can also opt
    /// in on its own via `TestingPlatformDotnetTestSupport`, which is read
    /// from the evaluation.
    pub test_platform_runner: bool,
}

/// Strip `base` (plus one separator) from the start of `value`,
/// case-insensitively — Windows paths are case-insensitive and MSBuild
/// output casing is not guaranteed to match what moon reports. Both inputs
/// must already use forward slashes.
fn strip_prefix_ci<'a>(value: &'a str, base: &str) -> Option<&'a str> {
    if base.is_empty() {
        return None;
    }

    let mut value_iter = value.char_indices();
    let mut base_iter = base.chars();

    loop {
        let Some(base_ch) = base_iter.next() else {
            // Base fully consumed: the next value char must be the separator.
            return match value_iter.next() {
                Some((index, '/')) => Some(&value[index + 1..]),
                _ => None,
            };
        };

        let (_, value_ch) = value_iter.next()?;

        if value_ch != base_ch && !value_ch.to_lowercase().eq(base_ch.to_lowercase()) {
            return None;
        }
    }
}

/// Turn an evaluated MSBuild output path into a moon task output: relative
/// paths pass through, absolute paths under the project dir become
/// project-relative, absolute paths under the workspace root become
/// workspace-relative (leading `/`). Anything else (redirected outside the
/// workspace) is `None` — the task must then disable caching rather than
/// cache the wrong directory.
pub fn resolve_output_path(raw: &str, project_dir: &str, workspace_dir: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }

    let value = raw.replace('\\', "/");
    let value = value.trim_end_matches('/');

    if value.is_empty() {
        return None;
    }

    let is_absolute = value.starts_with('/') || value.as_bytes().get(1) == Some(&b':');

    if !is_absolute {
        return Some(value.to_string());
    }

    let project_dir = project_dir.replace('\\', "/");

    if let Some(relative) = strip_prefix_ci(value, project_dir.trim_end_matches('/')) {
        return Some(relative.to_string());
    }

    let workspace_dir = workspace_dir.replace('\\', "/");

    if let Some(relative) = strip_prefix_ci(value, workspace_dir.trim_end_matches('/')) {
        return Some(format!("/{relative}"));
    }

    None
}

fn command(verb: &str, project_file: Option<&str>, extra_args: &[&str]) -> PartialTaskArgs {
    let mut list = vec!["dotnet".to_string(), verb.to_string()];

    list.extend(project_file.map(str::to_string));
    list.extend(extra_args.iter().map(|arg| arg.to_string()));

    PartialTaskArgs::List(list)
}

/// Pin the evaluated `Configuration` on cacheable commands. `dotnet build`
/// defaults to Debug but `dotnet publish` defaults to Release (.NET 8+), so
/// without an explicit `-c` a `publish --no-build` would look for outputs a
/// `build` never produced. Passing the configuration the evaluation itself
/// saw keeps every command consistent with the evaluated output paths —
/// including repos that set `Configuration` in `Directory.Build.props`.
fn pin_configuration(command: &mut PartialTaskArgs, configuration: &str) {
    if configuration.is_empty() {
        return;
    }

    if let PartialTaskArgs::List(list) = command {
        list.push("-c".into());
        list.push(configuration.into());
    }
}

fn parse_target(target: &str) -> AnyResult<PartialTaskDependency> {
    Ok(PartialTaskDependency::Target(
        Target::parse(target).map_err(|error| anyhow!("{error}"))?,
    ))
}

/// Same, but tolerated when the target does not exist. Required for `~:` deps:
/// moon defaults `optional` to `false` for the `OwnSelf` scope, so a project
/// that infers `test` or `publish` without `build` — `inferTasks: ['test']`, or
/// a `build` id claimed by an inherited task file — would fail project-graph
/// construction outright with `UnknownDepTarget` rather than simply losing the
/// ordering edge.
fn parse_optional_target(target: &str) -> AnyResult<PartialTaskDependency> {
    Ok(PartialTaskDependency::Object(PartialTaskDependencyConfig {
        target: Some(Target::parse(target).map_err(|error| anyhow!("{error}"))?),
        optional: Some(true),
        ..Default::default()
    }))
}

/// Inputs for cacheable tasks: everything in the project EXCEPT the
/// evaluated output and intermediate directories. moon's default `**/*`
/// would otherwise hash `obj/` (which MSBuild mutates on every build), so
/// task hashes would never stabilize and nothing would ever be a cache hit.
fn stable_inputs(inputs: &InferInputs) -> AnyResult<Vec<Input>> {
    let mut list = vec![Input::parse("**/*").map_err(|error| anyhow!("{error}"))?];

    for property in ["BaseOutputPath", "BaseIntermediateOutputPath"] {
        if let Some(dir) = resolve_output_path(
            inputs.evaluation.property(property),
            inputs.project_dir,
            inputs.workspace_dir,
        ) {
            list.push(Input::parse(format!("!{dir}/**")).map_err(|error| anyhow!("{error}"))?);
        }
    }

    Ok(list)
}

/// Give a task its evaluated outputs, or disable caching when they could
/// not be determined (never cache the wrong directory).
fn apply_outputs(task: &mut PartialTaskConfig, outputs: Option<String>) -> AnyResult<()> {
    match outputs {
        Some(path) => {
            task.outputs = Some(vec![
                Output::parse(&path).map_err(|error| anyhow!("{error}"))?,
            ]);
        }
        None => {
            task.options.get_or_insert_default().cache = Some(TaskOptionCache::Enabled(false));
        }
    }

    Ok(())
}

/// Task ids that inference would have contributed but had to yield to an
/// inherited task file, paired with the file that claimed each one.
///
/// Yielding is silent otherwise, which turns "why does no project have a
/// build task?" into a dead end — worth one report per workspace.
pub fn reportable_conflicts<'a>(
    reserved: &'a BTreeMap<String, String>,
    setting: &InferTasksSetting,
) -> Vec<(&'a str, &'a str)> {
    INFERABLE_TASKS
        .iter()
        .filter(|task| setting.includes(task))
        .filter_map(|task| {
            reserved
                .get_key_value(*task)
                .map(|(id, file)| (id.as_str(), file.as_str()))
        })
        .collect()
}

/// Infer `build` / `test` / `run` / `publish` tasks from one project's
/// MSBuild evaluation.
///
/// - `build` — every project; `--no-dependencies` so moon's task graph
///   (`deps: ^:build`) orchestrates upstream builds and caches each project
///   independently (verified: MSBuild resolves `ProjectReference`s from the
///   upstream `bin` output without rebuilding them).
/// - `test` — projects with `IsTestProject=true` or a `Microsoft.NET.Test.Sdk`
///   reference; `--no-build` on top of a `build` dep.
/// - `run` — `Exe`/`WinExe` non-test projects; never cached, never in CI.
/// - `publish` — `Exe`/`WinExe` non-test single-TFM projects (multi-TFM
///   `dotnet publish` requires an explicit `-f`); `--no-build` on top of a
///   `build` dep.
///
/// `restore` is deliberately NOT a task: moon models it as the
/// install-dependencies action (with `--locked-mode`), which runs before
/// tasks — hence `--no-restore` everywhere.
///
/// `reserved_ids` (task ids from applicable inherited task files) are
/// skipped entirely: moon merges plugin tasks over inherited tasks with
/// args-append semantics, which produces garbage commands — yielding is the
/// only safe move. Project-level `moon.yml` tasks need no such handling;
/// moon itself guarantees they win over plugin tasks.
pub fn infer_tasks(
    setting: &InferTasksSetting,
    reserved_ids: &BTreeSet<String>,
    inputs: &InferInputs,
) -> AnyResult<BTreeMap<Id, PartialTaskConfig>> {
    let mut tasks = BTreeMap::new();
    let evaluation = inputs.evaluation;

    // `IsTestProject` is set by Microsoft.NET.Test.Sdk's build props, which
    // are only imported after a restore. Fall back to the package reference
    // itself so unrestored projects are detected too.
    let is_test = evaluation
        .property("IsTestProject")
        .eq_ignore_ascii_case("true")
        || evaluation
            .package_references()
            .keys()
            .any(|name| name.eq_ignore_ascii_case("Microsoft.NET.Test.Sdk"));

    let output_type = evaluation.property("OutputType");
    let is_exe = !is_test
        && (output_type.eq_ignore_ascii_case("Exe") || output_type.eq_ignore_ascii_case("WinExe"));
    let is_single_tfm = evaluation.property("TargetFrameworks").is_empty();

    let wants = |task: &str| setting.includes(task) && !reserved_ids.contains(task);
    let file = inputs.explicit_project_file;

    let hash_inputs = stable_inputs(inputs)?;
    let configuration = evaluation.property("Configuration");

    if wants("build") {
        let mut build_command = command("build", file, &["--no-restore", "--no-dependencies"]);
        pin_configuration(&mut build_command, configuration);

        let mut task = PartialTaskConfig {
            command: Some(build_command),
            deps: Some(vec![parse_target("^:build")?]),
            description: Some(
                "Builds the project. Upstream projects build through moon task deps. (inferred)"
                    .into(),
            ),
            inputs: Some(hash_inputs.clone()),
            ..Default::default()
        };

        apply_outputs(
            &mut task,
            resolve_output_path(
                evaluation.property("BaseOutputPath"),
                inputs.project_dir,
                inputs.workspace_dir,
            ),
        )?;

        tasks.insert(Id::raw("build"), task);
    }

    if is_test && wants("test") {
        // Microsoft.Testing.Platform's `dotnet test` takes the project
        // through `--project` and rejects a positional path; classic VSTest
        // mode is the exact opposite and rejects `--project`. Both verified
        // against SDK 10.0.201, so the flavour has to match the runner.
        let uses_test_platform = inputs.test_platform_runner
            || evaluation
                .property("TestingPlatformDotnetTestSupport")
                .eq_ignore_ascii_case("true");

        let mut test_command = match file {
            Some(file) if uses_test_platform => command(
                "test",
                None,
                &["--project", file, "--no-build", "--no-restore"],
            ),
            _ => command("test", file, &["--no-build", "--no-restore"]),
        };

        pin_configuration(&mut test_command, configuration);

        tasks.insert(
            Id::raw("test"),
            PartialTaskConfig {
                command: Some(test_command),
                deps: Some(vec![parse_optional_target("~:build")?]),
                description: Some("Runs tests against the built assemblies. (inferred)".into()),
                inputs: Some(hash_inputs.clone()),
                ..Default::default()
            },
        );
    }

    if is_exe && wants("run") {
        tasks.insert(
            Id::raw("run"),
            PartialTaskConfig {
                command: Some(if let Some(file) = file {
                    command("run", None, &["--project", file])
                } else {
                    command("run", None, &[])
                }),
                description: Some("Runs the application locally. (inferred)".into()),
                options: Some(PartialTaskOptionsConfig {
                    cache: Some(TaskOptionCache::Enabled(false)),
                    run_in_ci: Some(TaskOptionRunInCI::Enabled(false)),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
    }

    if is_exe && is_single_tfm && wants("publish") {
        let mut publish_command = command("publish", file, &["--no-build", "--no-restore"]);
        pin_configuration(&mut publish_command, configuration);

        let mut task = PartialTaskConfig {
            command: Some(publish_command),
            deps: Some(vec![parse_optional_target("~:build")?]),
            description: Some("Publishes the built application. (inferred)".into()),
            inputs: Some(hash_inputs),
            ..Default::default()
        };

        apply_outputs(
            &mut task,
            resolve_output_path(
                evaluation.property("PublishDir"),
                inputs.project_dir,
                inputs.workspace_dir,
            ),
        )?;

        tasks.insert(Id::raw("publish"), task);
    }

    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluation(properties: &[(&str, &str)]) -> MsbuildEvaluation {
        let mut evaluation = MsbuildEvaluation::default();

        for (name, value) in properties {
            evaluation
                .properties
                .insert(name.to_string(), value.to_string());
        }

        evaluation
    }

    fn infer(
        evaluation: &MsbuildEvaluation,
        setting: &InferTasksSetting,
        reserved: &[&str],
    ) -> BTreeMap<Id, PartialTaskConfig> {
        infer_tasks(
            setting,
            &reserved.iter().map(|id| id.to_string()).collect(),
            &InferInputs {
                evaluation,
                explicit_project_file: None,
                project_dir: "C:\\work\\repo\\app",
                workspace_dir: "C:\\work\\repo",
                test_platform_runner: false,
            },
        )
        .unwrap()
    }

    fn test_project_evaluation() -> MsbuildEvaluation {
        let mut eval = evaluation(&[("OutputType", "Exe"), ("TargetFramework", "net10.0")]);
        eval.items.insert(
            "PackageReference".into(),
            vec![serde_json::json!({ "Identity": "Microsoft.NET.Test.Sdk" })],
        );
        eval
    }

    fn command_line(task: &PartialTaskConfig) -> String {
        match task.command.as_ref().unwrap() {
            PartialTaskArgs::List(list) => list.join(" "),
            PartialTaskArgs::String(value) => value.clone(),
            other => panic!("unexpected command shape: {other:?}"),
        }
    }

    #[test]
    fn classlib_gets_build_only() {
        let eval = evaluation(&[
            ("OutputType", "Library"),
            ("BaseOutputPath", "bin\\"),
            ("BaseIntermediateOutputPath", "obj\\"),
        ]);
        let tasks = infer(&eval, &InferTasksSetting::default(), &[]);

        assert_eq!(
            tasks.keys().map(|id| id.as_str()).collect::<Vec<_>>(),
            vec!["build"]
        );

        let build = &tasks[&Id::raw("build")];
        assert_eq!(
            command_line(build),
            "dotnet build --no-restore --no-dependencies"
        );
        assert_eq!(
            build.outputs.as_ref().unwrap(),
            &vec![Output::parse("bin").unwrap()]
        );
        assert!(build.options.is_none(), "outputs known => cache untouched");
        assert!(build.deps.is_some());
        // Inputs exclude the evaluated output/intermediate dirs so hashes
        // stabilize (obj is mutated by every build).
        assert_eq!(
            build.inputs.as_ref().unwrap(),
            &vec![
                Input::parse("**/*").unwrap(),
                Input::parse("!bin/**").unwrap(),
                Input::parse("!obj/**").unwrap(),
            ]
        );
    }

    #[test]
    fn exe_gets_build_run_publish() {
        let eval = evaluation(&[
            ("OutputType", "Exe"),
            ("BaseOutputPath", "bin\\"),
            ("TargetFramework", "net8.0"),
            ("PublishDir", "bin\\Debug\\net8.0\\publish\\"),
        ]);
        let tasks = infer(&eval, &InferTasksSetting::default(), &[]);

        assert_eq!(
            tasks.keys().map(|id| id.as_str()).collect::<Vec<_>>(),
            vec!["build", "publish", "run"]
        );

        let run = &tasks[&Id::raw("run")];
        assert_eq!(command_line(run), "dotnet run");
        let run_options = run.options.as_ref().unwrap();
        assert_eq!(run_options.cache, Some(TaskOptionCache::Enabled(false)));
        assert_eq!(
            run_options.run_in_ci,
            Some(TaskOptionRunInCI::Enabled(false))
        );

        let publish = &tasks[&Id::raw("publish")];
        assert_eq!(
            command_line(publish),
            "dotnet publish --no-build --no-restore"
        );
        assert_eq!(
            publish.outputs.as_ref().unwrap(),
            &vec![Output::parse("bin/Debug/net8.0/publish").unwrap()]
        );
    }

    #[test]
    fn test_project_gets_build_test_never_run() {
        // Modern test SDKs can flip OutputType to Exe — test wins over run.
        let mut eval = evaluation(&[("OutputType", "Exe"), ("TargetFramework", "net8.0")]);
        eval.items.insert(
            "PackageReference".into(),
            vec![serde_json::json!({ "Identity": "Microsoft.NET.Test.Sdk", "Version": "17.10.0" })],
        );

        let tasks = infer(&eval, &InferTasksSetting::default(), &[]);

        assert!(tasks.contains_key(&Id::raw("build")));
        assert!(tasks.contains_key(&Id::raw("test")));
        assert!(!tasks.contains_key(&Id::raw("run")));
        assert!(!tasks.contains_key(&Id::raw("publish")));

        let test = &tasks[&Id::raw("test")];
        assert_eq!(command_line(test), "dotnet test --no-build --no-restore");
    }

    #[test]
    fn pins_evaluated_configuration_on_cacheable_commands() {
        // `dotnet publish` defaults to Release (.NET 8+) while `dotnet build`
        // defaults to Debug — the explicit `-c` keeps `--no-build` coherent.
        let mut eval = evaluation(&[
            ("OutputType", "Exe"),
            ("TargetFramework", "net8.0"),
            ("Configuration", "Debug"),
            ("PublishDir", "bin\\Debug\\net8.0\\publish\\"),
        ]);
        eval.items.insert(
            "PackageReference".into(),
            vec![serde_json::json!({ "Identity": "Microsoft.NET.Test.Sdk" })],
        );

        let tasks = infer(&eval, &InferTasksSetting::default(), &[]);

        assert_eq!(
            command_line(&tasks[&Id::raw("build")]),
            "dotnet build --no-restore --no-dependencies -c Debug"
        );
        assert_eq!(
            command_line(&tasks[&Id::raw("test")]),
            "dotnet test --no-build --no-restore -c Debug"
        );
    }

    #[test]
    fn multi_tfm_exe_skips_publish() {
        let eval = evaluation(&[("OutputType", "Exe"), ("TargetFrameworks", "net8.0;net9.0")]);
        let tasks = infer(&eval, &InferTasksSetting::default(), &[]);

        assert!(tasks.contains_key(&Id::raw("run")));
        assert!(!tasks.contains_key(&Id::raw("publish")));
    }

    #[test]
    fn unknown_outputs_disable_caching_instead_of_guessing() {
        // BaseOutputPath redirected outside the workspace entirely.
        let eval = evaluation(&[
            ("OutputType", "Library"),
            ("BaseOutputPath", "D:\\global-outputs\\app\\"),
        ]);
        let tasks = infer(&eval, &InferTasksSetting::default(), &[]);

        let build = &tasks[&Id::raw("build")];
        assert!(build.outputs.is_none());
        assert_eq!(
            build.options.as_ref().unwrap().cache,
            Some(TaskOptionCache::Enabled(false))
        );
    }

    #[test]
    fn granular_selection_and_reserved_ids_are_respected() {
        let eval = evaluation(&[
            ("OutputType", "Exe"),
            ("BaseOutputPath", "bin\\"),
            ("TargetFramework", "net8.0"),
            ("PublishDir", "bin\\Debug\\net8.0\\publish\\"),
        ]);

        let only = InferTasksSetting::Only(vec!["run".into(), "publish".into()]);
        let tasks = infer(&eval, &only, &[]);
        assert_eq!(
            tasks.keys().map(|id| id.as_str()).collect::<Vec<_>>(),
            vec!["publish", "run"],
            "granular selection"
        );

        let tasks = infer(&eval, &InferTasksSetting::default(), &["run", "build"]);
        assert_eq!(
            tasks.keys().map(|id| id.as_str()).collect::<Vec<_>>(),
            vec!["publish"],
            "reserved (inherited) ids skipped"
        );

        let tasks = infer(&eval, &InferTasksSetting::Enabled(false), &[]);
        assert!(tasks.is_empty());
    }

    #[test]
    fn the_self_build_dep_is_optional() {
        // moon defaults `~:` deps to mandatory, so selecting only `test` or
        // only `publish` — no `build` task to depend on — would fail
        // project-graph construction with `UnknownDepTarget` if these were
        // plain targets.
        // A project is either a test project or an executable — `is_exe`
        // excludes `is_test` — so each dep needs its own evaluation.
        let cases = [
            ("test", evaluation(&[("IsTestProject", "true")])),
            (
                "publish",
                evaluation(&[
                    ("OutputType", "Exe"),
                    ("TargetFramework", "net8.0"),
                    ("PublishDir", "bin\\Debug\\net8.0\\publish\\"),
                ]),
            ),
        ];

        for (id, eval) in cases {
            let tasks = infer(&eval, &InferTasksSetting::Only(vec![id.into()]), &[]);

            assert_eq!(
                tasks[&Id::raw(id)].deps.as_deref(),
                Some(&[parse_optional_target("~:build").unwrap()][..]),
                "`{id}` must depend on an optional `~:build`"
            );
        }
    }

    #[test]
    fn multiple_project_files_get_explicit_targets() {
        let eval = evaluation(&[
            ("OutputType", "Exe"),
            ("BaseOutputPath", "bin\\"),
            ("TargetFramework", "net8.0"),
        ]);

        let tasks = infer_tasks(
            &InferTasksSetting::default(),
            &BTreeSet::new(),
            &InferInputs {
                evaluation: &eval,
                explicit_project_file: Some("App.csproj"),
                project_dir: "/repo/app",
                workspace_dir: "/repo",
                test_platform_runner: false,
            },
        )
        .unwrap();

        assert_eq!(
            command_line(&tasks[&Id::raw("build")]),
            "dotnet build App.csproj --no-restore --no-dependencies"
        );
        assert_eq!(
            command_line(&tasks[&Id::raw("run")]),
            "dotnet run --project App.csproj"
        );
    }

    #[test]
    fn test_platform_takes_the_project_through_a_flag() {
        let eval = test_project_evaluation();

        let infer_with = |runner: bool, file: Option<&str>| {
            let tasks = infer_tasks(
                &InferTasksSetting::default(),
                &BTreeSet::new(),
                &InferInputs {
                    evaluation: &eval,
                    explicit_project_file: file,
                    project_dir: "/repo/app-tests",
                    workspace_dir: "/repo",
                    test_platform_runner: runner,
                },
            )
            .unwrap();

            command_line(&tasks[&Id::raw("test")])
        };

        // MTP rejects a positional project path...
        assert_eq!(
            infer_with(true, Some("App.Tests.csproj")),
            "dotnet test --project App.Tests.csproj --no-build --no-restore"
        );
        // ...while classic VSTest mode rejects `--project`.
        assert_eq!(
            infer_with(false, Some("App.Tests.csproj")),
            "dotnet test App.Tests.csproj --no-build --no-restore"
        );
        // With one project file in the directory neither flavour applies:
        // the command runs in the project directory with no path at all.
        assert_eq!(
            infer_with(true, None),
            "dotnet test --no-build --no-restore"
        );
        assert_eq!(
            infer_with(false, None),
            "dotnet test --no-build --no-restore"
        );
    }

    #[test]
    fn project_level_test_platform_opt_in_is_honored() {
        // A project can select MTP on its own, without a global.json.
        let mut eval = test_project_evaluation();
        eval.properties
            .insert("TestingPlatformDotnetTestSupport".into(), "true".into());

        let tasks = infer_tasks(
            &InferTasksSetting::default(),
            &BTreeSet::new(),
            &InferInputs {
                evaluation: &eval,
                explicit_project_file: Some("App.Tests.csproj"),
                project_dir: "/repo/app-tests",
                workspace_dir: "/repo",
                test_platform_runner: false,
            },
        )
        .unwrap();

        assert_eq!(
            command_line(&tasks[&Id::raw("test")]),
            "dotnet test --project App.Tests.csproj --no-build --no-restore"
        );
    }

    #[test]
    fn reports_only_conflicts_that_actually_suppress_inference() {
        let reserved: BTreeMap<String, String> = [
            ("build", "/workspace/.moon/tasks/dotnet.yml"),
            ("publish", "/workspace/.moon/tasks.yml"),
            // Not inferable, so its presence is unremarkable.
            ("lint", "/workspace/.moon/tasks/all.yml"),
        ]
        .iter()
        .map(|(id, file)| (id.to_string(), file.to_string()))
        .collect();

        assert_eq!(
            reportable_conflicts(&reserved, &InferTasksSetting::default()),
            vec![
                ("build", "/workspace/.moon/tasks/dotnet.yml"),
                ("publish", "/workspace/.moon/tasks.yml"),
            ]
        );

        // A task the user did not ask us to infer is not a conflict.
        assert_eq!(
            reportable_conflicts(&reserved, &InferTasksSetting::Only(vec!["publish".into()])),
            vec![("publish", "/workspace/.moon/tasks.yml")]
        );

        assert!(reportable_conflicts(&reserved, &InferTasksSetting::Enabled(false)).is_empty());
        assert!(reportable_conflicts(&BTreeMap::new(), &InferTasksSetting::default()).is_empty());
    }

    #[test]
    fn resolves_output_paths_in_every_form() {
        // Relative stays relative.
        assert_eq!(
            resolve_output_path("bin\\", "C:\\repo\\app", "C:\\repo"),
            Some("bin".into())
        );
        // Absolute under the project dir, case-insensitive.
        assert_eq!(
            resolve_output_path("C:\\Repo\\App\\bin\\Debug\\", "c:\\repo\\app", "c:\\repo"),
            Some("bin/Debug".into())
        );
        // Absolute under the workspace (artifacts layout) => workspace-relative.
        assert_eq!(
            resolve_output_path(
                "C:\\repo\\artifacts\\bin\\app\\",
                "C:\\repo\\app",
                "C:\\repo"
            ),
            Some("/artifacts/bin/app".into())
        );
        // Unix forms.
        assert_eq!(
            resolve_output_path("/repo/app/bin", "/repo/app", "/repo"),
            Some("bin".into())
        );
        // Outside the workspace => not resolvable.
        assert_eq!(
            resolve_output_path("D:\\elsewhere\\bin", "C:\\repo\\app", "C:\\repo"),
            None
        );
        // Empty => not resolvable.
        assert_eq!(resolve_output_path("", "C:\\repo\\app", "C:\\repo"), None);
        // Prefix must respect component boundaries.
        assert_eq!(
            resolve_output_path("/repo/app-other/bin", "/repo/app", "/repo"),
            Some("/app-other/bin".into())
        );
    }
}
