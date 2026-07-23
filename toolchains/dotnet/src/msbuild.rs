use moon_pdk_api::AnyResult;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Result of an MSBuild evaluation via `dotnet msbuild -getProperty:... -getItem:...`.
#[derive(Debug, Default, Deserialize)]
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

    #[test]
    fn normalizes_path_keys() {
        assert_eq!(
            normalize_path_key("C:\\Abs\\Path\\LibA\\LibA.csproj"),
            "c:/abs/path/liba/liba.csproj"
        );
        assert_eq!(normalize_path_key("/home/x/App.csproj"), "/home/x/app.csproj");
    }
}
