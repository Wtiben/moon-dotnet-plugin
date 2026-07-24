use moon_pdk_api::AnyResult;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Result of an MSBuild evaluation via `dotnet msbuild -getProperty:... -getItem:...`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct MsbuildEvaluation {
    #[serde(rename = "Properties", default)]
    pub properties: BTreeMap<String, String>,

    #[serde(rename = "Items", default)]
    pub items: BTreeMap<String, Vec<serde_json::Value>>,
}

impl MsbuildEvaluation {
    pub fn property(&self, name: &str) -> &str {
        self.properties.get(name).map(String::as_str).unwrap_or("")
    }

    /// `FullPath` of every ProjectReference item (host-real absolute paths).
    pub fn project_reference_paths(&self) -> Vec<String> {
        self.items
            .get("ProjectReference")
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("FullPath"))
                    .filter_map(|value| value.as_str())
                    .map(|value| value.to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// PackageReference `Identity` -> `Version` (missing version becomes `*`).
    pub fn package_references(&self) -> BTreeMap<String, String> {
        self.items
            .get("PackageReference")
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let identity = item.get("Identity")?.as_str()?;
                        let version = item
                            .get("Version")
                            .and_then(|value| value.as_str())
                            .filter(|value| !value.is_empty())
                            .unwrap_or("*");

                        Some((identity.to_owned(), version.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The exact `-getProperty` list requested per evaluation.
pub const EVAL_PROPERTIES: &str =
    "TargetFramework,TargetFrameworks,OutputType,IsTestProject,IsPackable,RestorePackagesWithLockFile";

/// The exact `-getItem` list requested per evaluation.
pub const EVAL_ITEMS: &str = "ProjectReference,PackageReference";

/// Parse the stdout of an MSBuild `-get*` invocation. MSBuild may print stray
/// warnings before the JSON — start at the first `{`.
pub fn parse_msbuild_output(stdout: &str) -> AnyResult<MsbuildEvaluation> {
    let json_start = stdout
        .find('{')
        .ok_or_else(|| moon_pdk_api::anyhow!("no JSON found in MSBuild output"))?;

    Ok(serde_json::from_str(&stdout[json_start..])?)
}

/// Lexically normalize a host path for cross-referencing MSBuild output
/// against moon project paths: forward slashes, lowercased (paths on Windows
/// are case-insensitive; MSBuild output casing is not guaranteed to match
/// the on-disk casing moon reports).
pub fn normalize_path_key(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

/// Escape a literal path for use inside an MSBuild `Include` attribute:
/// MSBuild's own special characters (property/item expansion, list
/// separators, globs) via `%XX` escapes, then XML attribute characters.
pub fn escape_msbuild_include(path: &str) -> String {
    path.replace('%', "%25")
        .replace('$', "%24")
        .replace('@', "%40")
        .replace(';', "%3B")
        .replace('*', "%2A")
        .replace('?', "%3F")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Item metadata name carrying the `|`-joined ProjectReference full paths in
/// batched evaluation output.
const BATCH_PROJECT_REFS: &str = "MoonProjectRefs";

/// Item metadata name carrying the `|`-joined `Identity@Version`
/// PackageReference entries in batched evaluation output.
const BATCH_PACKAGE_REFS: &str = "MoonPackageRefs";

/// The `.targets` file injected into every project during batched evaluation
/// (via the `CustomAfterMicrosoftCommon(CrossTargeting)Targets` hooks): a
/// target that returns the project's evaluation state as one item with
/// metadata. It runs as the entry target with no dependencies, so item state
/// when it executes is identical to evaluation state — the same answers as a
/// per-project `-getItem` query.
pub fn moon_eval_targets_xml() -> String {
    let properties = EVAL_PROPERTIES
        .split(',')
        .map(|prop| format!("        <{prop}>$({prop})</{prop}>\n"))
        .collect::<String>();

    format!(
        r#"<Project>
  <Target Name="MoonEvalInner" Returns="@(_MoonEvalResult)">
    <ItemGroup>
      <_MoonEvalResult Include="$(MSBuildProjectFullPath)">
{properties}        <{BATCH_PROJECT_REFS}>@(ProjectReference->'%(FullPath)', '|')</{BATCH_PROJECT_REFS}>
        <{BATCH_PACKAGE_REFS}>@(PackageReference->'%(Identity)@%(Version)', '|')</{BATCH_PACKAGE_REFS}>
      </_MoonEvalResult>
    </ItemGroup>
  </Target>
</Project>
"#
    )
}

/// The traversal project for batched evaluation: fans out to every listed
/// project with `BuildInParallel` (in-process MSBuild worker nodes) and
/// collects the injected target's outputs. A raw `<Project>` with no `Sdk`
/// attribute imports nothing implicitly, so the workspace's own
/// `Directory.Build.props` cannot interfere with the traversal itself, while
/// the child projects still evaluate with their full normal import chains.
/// `ContinueOnError` keeps one broken project from aborting the batch — it
/// just goes missing from the output (and falls back to per-project
/// evaluation).
pub fn traversal_project_xml(project_paths: &[String]) -> String {
    let includes = project_paths
        .iter()
        .map(|path| {
            format!(
                "    <MoonProject Include=\"{}\" />\n",
                escape_msbuild_include(path)
            )
        })
        .collect::<String>();

    format!(
        r#"<Project DefaultTargets="MoonCollect">
  <ItemGroup>
{includes}  </ItemGroup>
  <Target Name="MoonCollect" Returns="@(MoonEval)">
    <MSBuild
      Projects="@(MoonProject)"
      Targets="MoonEvalInner"
      BuildInParallel="true"
      ContinueOnError="WarnAndContinue"
      Properties="CustomAfterMicrosoftCommonTargets=$(MSBuildThisFileDirectory)moon-eval.targets;CustomAfterMicrosoftCommonCrossTargetingTargets=$(MSBuildThisFileDirectory)moon-eval.targets">
      <Output TaskParameter="TargetOutputs" ItemName="MoonEval" />
    </MSBuild>
  </Target>
</Project>
"#
    )
}

/// Parse the `-getItem:MoonEval` JSON of a batched traversal invocation into
/// per-project evaluations. Each project is keyed (normalized) by every
/// identifying path on its item: the traversal `Include` we wrote
/// (`OriginalItemSpec`) and MSBuild's own expanded full path
/// (`MSBuildSourceProjectFile` / `Identity`) — these can differ lexically,
/// e.g. Windows 8.3 short names in temp directories.
pub fn parse_batch_output(stdout: &str) -> AnyResult<BTreeMap<String, MsbuildEvaluation>> {
    let raw = parse_msbuild_output(stdout)?;
    let mut results = BTreeMap::new();

    let Some(items) = raw.items.get("MoonEval") else {
        return Ok(results);
    };

    for item in items {
        let metadata = |name: &str| {
            item.get(name)
                .and_then(|value| value.as_str())
                .unwrap_or("")
        };

        let mut evaluation = MsbuildEvaluation::default();

        for prop in EVAL_PROPERTIES.split(',') {
            evaluation
                .properties
                .insert(prop.to_owned(), metadata(prop).to_owned());
        }

        let project_refs = metadata(BATCH_PROJECT_REFS)
            .split('|')
            .filter(|path| !path.is_empty())
            .map(|path| serde_json::json!({ "FullPath": path }))
            .collect::<Vec<_>>();

        if !project_refs.is_empty() {
            evaluation
                .items
                .insert("ProjectReference".to_owned(), project_refs);
        }

        let package_refs = metadata(BATCH_PACKAGE_REFS)
            .split('|')
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                // '@' cannot appear in NuGet package ids or versions; an
                // empty version (e.g. Central Package Management) becomes
                // `*` downstream.
                let (identity, version) = entry.rsplit_once('@').unwrap_or((entry, ""));
                serde_json::json!({ "Identity": identity, "Version": version })
            })
            .collect::<Vec<_>>();

        if !package_refs.is_empty() {
            evaluation
                .items
                .insert("PackageReference".to_owned(), package_refs);
        }

        for key_field in ["OriginalItemSpec", "MSBuildSourceProjectFile", "Identity"] {
            let key = metadata(key_field);

            if !key.is_empty() {
                results.insert(normalize_path_key(key), evaluation.clone());
            }
        }
    }

    Ok(results)
}

/// Given the output of a failed batch invocation, find which of the input
/// projects MSBuild reported errors for. Error lines carry the
/// locale-invariant `<full path>(line,col): error CODE:` prefix, so a
/// normalized substring match identifies the offenders without parsing
/// localized message text. Only the trailing `<parent>/<file>(` suffix is
/// matched, not the full path: MSBuild prints expanded long paths, which
/// can differ lexically from the paths we passed (e.g. Windows 8.3 short
/// names like `RUNNER~1` in a temp-dir prefix). A shared suffix across two
/// projects merely over-excludes — those projects fall back to per-project
/// evaluation, which stays correct.
pub fn detect_failed_projects(output: &str, project_paths: &[String]) -> Vec<String> {
    let haystack = normalize_path_key(output);

    project_paths
        .iter()
        .filter(|path| {
            let normalized = normalize_path_key(path);

            // From the second-to-last separator: "/<parent>/<file>" — the
            // leading slash anchors the match to a component boundary.
            let suffix = normalized
                .rmatch_indices('/')
                .nth(1)
                .map(|(index, _)| &normalized[index..])
                .unwrap_or(&normalized);

            haystack.contains(&format!("{suffix}("))
        })
        .cloned()
        .collect()
}

/// Evaluate many projects with a single MSBuild invocation, paying the
/// dotnet/MSBuild startup cost (which dominates per-project evaluation)
/// once instead of once per project, and evaluating in parallel. The
/// generated traversal files live under `.moon/cache/` in the workspace.
#[cfg(feature = "wasm")]
pub fn evaluate_projects_batch(
    workspace_root: &moon_pdk_api::VirtualPath,
    project_real_paths: &[std::path::PathBuf],
) -> AnyResult<BTreeMap<String, MsbuildEvaluation>> {
    use moon_pdk::exec;
    use moon_pdk_api::{ExecCommandInput, anyhow};
    use starbase_utils::fs;

    let dir = workspace_root
        .join(".moon")
        .join("cache")
        .join("dotnet-toolchain");

    fs::create_dir_all(&dir)?;
    fs::write_file(dir.join("moon-eval.targets"), moon_eval_targets_xml())?;

    let traversal = dir.join("traversal.proj");

    let traversal_arg = traversal
        .real_path()
        .ok_or_else(|| anyhow!("no host-real path for {traversal:?}"))?
        .to_string_lossy()
        .to_string();

    let run = |batch_paths: &[String]| {
        fs::write_file(&traversal, traversal_project_xml(batch_paths))?;

        exec(ExecCommandInput::pipe(
            "dotnet",
            [
                "msbuild",
                traversal_arg.as_str(),
                "-nologo",
                // Parallel in-process worker nodes, but never leave them
                // alive after the invocation (node reuse lingers ~15 min,
                // which is hostile to CI containers).
                "-maxCpuCount",
                "-nodeReuse:false",
                "-t:MoonCollect",
                "-getItem:MoonEval",
            ],
        ))
    };

    let paths = project_real_paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    let output = run(&paths)?;

    if output.exit_code == 0 {
        return parse_batch_output(&output.stdout);
    }

    // MSBuild returns NO target outputs at all when any project fails to
    // load (ContinueOnError does not rescue load errors). Identify the
    // offenders from the error lines and retry once without them — their
    // absence from the result triggers the caller's per-project fallback,
    // which surfaces the real error.
    let combined = format!("{}{}", output.stdout, output.stderr);
    let failed = detect_failed_projects(&combined, &paths);

    if !failed.is_empty() && failed.len() < paths.len() {
        let remaining = paths
            .iter()
            .filter(|path| !failed.contains(path))
            .cloned()
            .collect::<Vec<_>>();

        let retry = run(&remaining)?;

        if retry.exit_code == 0 {
            return parse_batch_output(&retry.stdout);
        }
    }

    Err(anyhow!(
        "Batched MSBuild evaluation failed (exit code {}): {}{}",
        output.exit_code,
        output.stdout,
        output.stderr,
    ))
}

/// Run a real MSBuild evaluation for a project file (host-real path).
#[cfg(feature = "wasm")]
pub fn evaluate_project(csproj_real_path: &std::path::Path) -> AnyResult<MsbuildEvaluation> {
    use moon_pdk::exec;
    use moon_pdk_api::{ExecCommandInput, anyhow};

    let path_arg = csproj_real_path.to_string_lossy().to_string();

    let output = exec(ExecCommandInput::pipe(
        "dotnet",
        [
            "msbuild",
            path_arg.as_str(),
            "-nologo",
            &format!("-getProperty:{EVAL_PROPERTIES}"),
            &format!("-getItem:{EVAL_ITEMS}"),
        ],
    ))?;

    if output.exit_code != 0 {
        return Err(anyhow!(
            "MSBuild evaluation failed for {} (exit code {}): {}{}",
            csproj_real_path.display(),
            output.exit_code,
            output.stdout,
            output.stderr,
        ));
    }

    parse_msbuild_output(&output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Abridged real output shape from `dotnet msbuild -getProperty:... -getItem:...`
    // on .NET SDK 8+.
    const SAMPLE: &str = r#"{
  "Properties": {
    "TargetFramework": "",
    "TargetFrameworks": "net8.0;net9.0",
    "OutputType": "Exe",
    "IsTestProject": "",
    "IsPackable": "true",
    "RestorePackagesWithLockFile": ""
  },
  "Items": {
    "ProjectReference": [
      {
        "Identity": "..\\LibA\\LibA.csproj",
        "FullPath": "C:\\abs\\path\\LibA\\LibA.csproj",
        "Filename": "LibA",
        "Extension": ".csproj",
        "DefiningProjectFullPath": "C:\\abs\\path\\App\\App.csproj"
      }
    ],
    "PackageReference": [
      { "Identity": "Newtonsoft.Json", "Version": "13.0.3" },
      { "Identity": "NoVersionPkg" }
    ]
  }
}"#;

    #[test]
    fn parses_real_output_shape() {
        let eval = parse_msbuild_output(SAMPLE).unwrap();

        assert_eq!(eval.property("OutputType"), "Exe");
        assert_eq!(eval.property("TargetFrameworks"), "net8.0;net9.0");
        assert_eq!(eval.property("TargetFramework"), "");
        assert_eq!(eval.property("IsPackable"), "true");
        assert_eq!(
            eval.project_reference_paths(),
            vec!["C:\\abs\\path\\LibA\\LibA.csproj".to_string()]
        );

        let packages = eval.package_references();
        assert_eq!(packages.get("Newtonsoft.Json").unwrap(), "13.0.3");
        assert_eq!(packages.get("NoVersionPkg").unwrap(), "*");
    }

    #[test]
    fn skips_leading_noise_before_json() {
        let noisy = format!("some warning: blah\nanother line\n{SAMPLE}");
        let eval = parse_msbuild_output(&noisy).unwrap();

        assert_eq!(eval.property("OutputType"), "Exe");
    }

    #[test]
    fn errors_when_no_json() {
        assert!(parse_msbuild_output("MSBUILD : error MSB1063: ...").is_err());
    }

    #[test]
    fn empty_items_and_properties() {
        let eval = parse_msbuild_output("{}").unwrap();

        assert_eq!(eval.property("OutputType"), "");
        assert!(eval.project_reference_paths().is_empty());
        assert!(eval.package_references().is_empty());
    }

    // Abridged real output shape from a batched traversal invocation
    // (`-t:MoonCollect -getItem:MoonEval`), including MSBuild's well-known
    // metadata noise and a Windows 8.3 short path in OriginalItemSpec.
    const BATCH_SAMPLE: &str = r#"{
  "Items": {
    "MoonEval": [
      {
        "Identity": "C:\\long\\path\\app\\App.csproj",
        "MSBuildSourceProjectFile": "C:\\long\\path\\app\\App.csproj",
        "MSBuildSourceTargetName": "MoonEvalInner",
        "OriginalItemSpec": "C:\\LONGPA~1\\app\\App.csproj",
        "OutputType": "Exe",
        "TargetFramework": "net8.0",
        "TargetFrameworks": "",
        "IsTestProject": "",
        "IsPackable": "",
        "RestorePackagesWithLockFile": "",
        "MoonProjectRefs": "C:\\long\\path\\lib\\Lib.csproj|C:\\long\\path\\core\\Core.csproj",
        "MoonPackageRefs": "Newtonsoft.Json@13.0.3|CpmPackage@",
        "Filename": "App",
        "Extension": ".csproj"
      },
      {
        "Identity": "C:\\long\\path\\multi\\Multi.csproj",
        "MSBuildSourceProjectFile": "C:\\long\\path\\multi\\Multi.csproj",
        "OriginalItemSpec": "C:\\long\\path\\multi\\Multi.csproj",
        "OutputType": "Library",
        "TargetFramework": "",
        "TargetFrameworks": "net8.0;netstandard2.0",
        "IsTestProject": "",
        "IsPackable": "",
        "RestorePackagesWithLockFile": "",
        "MoonProjectRefs": "",
        "MoonPackageRefs": ""
      }
    ]
  }
}"#;

    #[test]
    fn parses_batch_output_per_project() {
        let results = parse_batch_output(BATCH_SAMPLE).unwrap();

        // Keyed by both the expanded path and the 8.3 short form we passed.
        let app = &results["c:/long/path/app/app.csproj"];
        assert!(results.contains_key("c:/longpa~1/app/app.csproj"));

        assert_eq!(app.property("OutputType"), "Exe");
        assert_eq!(app.property("TargetFramework"), "net8.0");
        assert_eq!(
            app.project_reference_paths(),
            vec![
                "C:\\long\\path\\lib\\Lib.csproj".to_string(),
                "C:\\long\\path\\core\\Core.csproj".to_string(),
            ]
        );

        let packages = app.package_references();
        assert_eq!(packages.get("Newtonsoft.Json").unwrap(), "13.0.3");
        // Versionless (CPM-style) entries fall back to `*`.
        assert_eq!(packages.get("CpmPackage").unwrap(), "*");

        let multi = &results["c:/long/path/multi/multi.csproj"];
        assert_eq!(multi.property("TargetFrameworks"), "net8.0;netstandard2.0");
        assert!(multi.project_reference_paths().is_empty());
        assert!(multi.package_references().is_empty());
    }

    #[test]
    fn batch_output_without_items_is_empty() {
        assert!(parse_batch_output("{}").unwrap().is_empty());
    }

    #[test]
    fn escapes_msbuild_includes() {
        assert_eq!(
            escape_msbuild_include("C:\\repo\\A & B\\$(odd)@*?;<x>\"100%\".csproj"),
            "C:\\repo\\A &amp; B\\%24(odd)%40%2A%3F%3B&lt;x&gt;&quot;100%25&quot;.csproj"
        );
    }

    #[test]
    fn targets_xml_covers_all_eval_properties() {
        let xml = moon_eval_targets_xml();

        for prop in EVAL_PROPERTIES.split(',') {
            assert!(xml.contains(&format!("<{prop}>$({prop})</{prop}>")), "{prop}");
        }

        assert!(xml.contains("MoonProjectRefs"));
        assert!(xml.contains("MoonPackageRefs"));
    }

    #[test]
    fn traversal_xml_lists_projects_and_injects_both_hooks() {
        let xml = traversal_project_xml(&[
            "C:\\repo\\a\\a.csproj".to_string(),
            "/home/x/b & c/b.csproj".to_string(),
        ]);

        assert!(xml.contains("Include=\"C:\\repo\\a\\a.csproj\""));
        assert!(xml.contains("Include=\"/home/x/b &amp; c/b.csproj\""));
        // Both hooks: plain SDK projects import CustomAfterMicrosoftCommonTargets,
        // multi-TFM outer builds import the CrossTargeting variant instead.
        assert!(xml.contains("CustomAfterMicrosoftCommonTargets=$(MSBuildThisFileDirectory)moon-eval.targets"));
        assert!(xml.contains("CustomAfterMicrosoftCommonCrossTargetingTargets=$(MSBuildThisFileDirectory)moon-eval.targets"));
        assert!(xml.contains("BuildInParallel=\"true\""));
        assert!(xml.contains("ContinueOnError=\"WarnAndContinue\""));
    }

    #[test]
    fn detects_failed_projects_across_short_and_long_path_forms() {
        // Real shape from GitHub's windows-latest runners: we pass a path
        // with an 8.3 short-name prefix (from %TEMP%), MSBuild's error line
        // prints the expanded long form.
        let output = "C:\\Users\\runneradmin\\AppData\\Local\\Temp\\scratch\\broken\\Broken.csproj(1,41): error MSB4025: The project file could not be loaded.";

        let paths = vec![
            "C:\\Users\\RUNNER~1\\AppData\\Local\\Temp\\scratch\\broken/Broken.csproj".to_string(),
            "C:\\Users\\RUNNER~1\\AppData\\Local\\Temp\\scratch\\ok\\Ok.csproj".to_string(),
        ];

        assert_eq!(detect_failed_projects(output, &paths), vec![paths[0].clone()]);
        assert!(detect_failed_projects("no errors here", &paths).is_empty());
    }

    #[test]
    fn normalizes_path_keys() {
        assert_eq!(
            normalize_path_key("C:\\Abs\\Path\\LibA\\LibA.csproj"),
            "c:/abs/path/liba/liba.csproj"
        );
        assert_eq!(normalize_path_key("/home/x/App.csproj"), "/home/x/app.csproj");
    }
}
