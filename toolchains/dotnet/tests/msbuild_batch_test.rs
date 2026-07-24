use dotnet_toolchain::msbuild::{
    MsbuildEvaluation, detect_failed_projects, moon_eval_targets_xml, normalize_path_key,
    parse_batch_output, traversal_project_xml,
};
use std::path::PathBuf;
use std::process::Command;

/// End-to-end validation of the batched evaluation mechanism against a real
/// MSBuild, independent of the wasm plugin — whose per-project fallback
/// would silently mask a broken batch in the sandbox tests.
#[test]
fn batched_traversal_evaluates_fixture_projects() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/__fixtures__/projects");

    let projects = [
        ["app", "App.csproj"],
        ["lib", "Lib.csproj"],
        ["core", "Core.csproj"],
        ["app-tests", "App.Tests.csproj"],
    ]
    .iter()
    .map(|[dir, file]| fixtures.join(dir).join(file).to_string_lossy().to_string())
    .collect::<Vec<_>>();

    let scratch = std::env::temp_dir().join(format!(
        "moon-dotnet-batch-test-{}",
        std::process::id()
    ));

    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("moon-eval.targets"), moon_eval_targets_xml()).unwrap();
    std::fs::write(
        scratch.join("traversal.proj"),
        traversal_project_xml(&projects),
    )
    .unwrap();

    let output = Command::new("dotnet")
        .args([
            "msbuild",
            scratch.join("traversal.proj").to_str().unwrap(),
            "-nologo",
            "-maxCpuCount",
            "-nodeReuse:false",
            "-t:MoonCollect",
            "-getItem:MoonEval",
        ])
        .output()
        .expect("these tests require a .NET SDK on PATH");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "batch invocation failed ({:?}):\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let results = parse_batch_output(&stdout).unwrap();

    std::fs::remove_dir_all(&scratch).ok();

    let get = |index: usize| -> &MsbuildEvaluation {
        let key = normalize_path_key(&projects[index]);

        results
            .get(&key)
            .unwrap_or_else(|| panic!("missing {key} in batch output: {:?}", results.keys()))
    };

    // app -> lib, and its Exe OutputType survives the round-trip.
    let app = get(0);
    let app_refs = app.project_reference_paths();
    assert_eq!(app_refs.len(), 1);
    assert!(normalize_path_key(&app_refs[0]).ends_with("lib/lib.csproj"));
    assert_eq!(app.property("OutputType"), "Exe");

    // lib -> core.
    let lib_refs = get(1).project_reference_paths();
    assert_eq!(lib_refs.len(), 1);
    assert!(normalize_path_key(&lib_refs[0]).ends_with("core/core.csproj"));

    // core has no references at all.
    let core = get(2);
    assert!(core.project_reference_paths().is_empty());
    assert!(core.package_references().is_empty());

    // app-tests -> app, with its evaluated package set intact.
    let tests = get(3);
    let tests_refs = tests.project_reference_paths();
    assert!(normalize_path_key(&tests_refs[0]).ends_with("app/app.csproj"));

    let packages = tests.package_references();
    assert_eq!(packages.get("Microsoft.NET.Test.Sdk").unwrap(), "17.10.0");
    assert_eq!(packages.get("xunit").unwrap(), "2.8.0");
}

/// Documents the MSBuild behavior the retry logic in
/// `evaluate_projects_batch` exists for: one unloadable project makes the
/// whole batch return exit != 0 with ZERO target outputs (`ContinueOnError`
/// does not rescue load errors) — and validates that
/// `detect_failed_projects` identifies exactly the offender from the real
/// error output, so the retry can exclude it.
#[test]
fn broken_project_aborts_batch_and_is_detectable() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/__fixtures__/projects");

    let scratch = std::env::temp_dir().join(format!(
        "moon-dotnet-batch-broken-test-{}",
        std::process::id()
    ));

    std::fs::create_dir_all(scratch.join("broken")).unwrap();
    std::fs::write(
        scratch.join("broken/Broken.csproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\"><broken",
    )
    .unwrap();

    let projects = vec![
        fixtures.join("core").join("Core.csproj").to_string_lossy().to_string(),
        scratch.join("broken/Broken.csproj").to_string_lossy().to_string(),
    ];

    std::fs::write(scratch.join("moon-eval.targets"), moon_eval_targets_xml()).unwrap();
    std::fs::write(
        scratch.join("traversal.proj"),
        traversal_project_xml(&projects),
    )
    .unwrap();

    let output = Command::new("dotnet")
        .args([
            "msbuild",
            scratch.join("traversal.proj").to_str().unwrap(),
            "-nologo",
            "-maxCpuCount",
            "-nodeReuse:false",
            "-t:MoonCollect",
            "-getItem:MoonEval",
        ])
        .output()
        .expect("these tests require a .NET SDK on PATH");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    std::fs::remove_dir_all(&scratch).ok();

    // The whole batch fails — not just the broken project.
    assert!(!output.status.success());
    assert!(parse_batch_output(&stdout).unwrap().is_empty());

    // But the offender is identifiable from the error lines, and the healthy
    // project is not falsely implicated.
    let failed = detect_failed_projects(&format!("{stdout}{stderr}"), &projects);
    assert_eq!(failed, vec![projects[1].clone()]);
}
