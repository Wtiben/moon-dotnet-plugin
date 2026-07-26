use moon_pdk_api::config_struct;
use schematic::{Config, Schematic};

/// The task names the plugin can infer.
pub const INFERABLE_TASKS: &[&str] = &["build", "test", "run", "publish"];

/// Which tasks to infer from evaluated MSBuild properties: a boolean to
/// enable/disable all of them, or an explicit list of task names
/// (`build`, `test`, `run`, `publish`) to infer only those.
#[derive(Clone, Debug, PartialEq, Schematic, serde::Deserialize, serde::Serialize)]
#[serde(
    untagged,
    expecting = "expected a boolean or a list of task names (build, test, run, publish)"
)]
pub enum InferTasksSetting {
    Enabled(bool),
    Only(Vec<String>),
}

impl Default for InferTasksSetting {
    fn default() -> Self {
        Self::Enabled(true)
    }
}

impl InferTasksSetting {
    /// Is a specific task name selected for inference?
    pub fn includes(&self, task: &str) -> bool {
        match self {
            Self::Enabled(enabled) => *enabled,
            Self::Only(list) => list.iter().any(|name| name.eq_ignore_ascii_case(task)),
        }
    }

    /// Is any inference enabled at all?
    pub fn any_enabled(&self) -> bool {
        match self {
            Self::Enabled(enabled) => *enabled,
            Self::Only(list) => !list.is_empty(),
        }
    }
}

config_struct!(
    /// Configures and enables the .NET toolchain.
    #[derive(Config)]
    pub struct DotnetToolchainConfig {
        /// Infer moon project dependencies from MSBuild `ProjectReference` items
        /// (runs a real MSBuild evaluation per project).
        #[setting(default = true)]
        pub infer_dependencies: bool,

        /// Infer `build`, `test`, `run`, and `publish` tasks from evaluated
        /// MSBuild properties (OutputType, IsTestProject, output paths).
        /// `true`/`false` toggles all of them; a list of task names infers
        /// only those. Task ids already defined in inherited task files
        /// (`.moon/tasks*`) or a project's `moon.yml` are never overridden.
        pub infer_tasks: InferTasksSetting,

        /// Additional arguments appended to `dotnet restore` during
        /// dependency installation.
        pub restore_args: Vec<String>,

        /// Explicit DOTNET_ROOT to inject into task environments. When unset,
        /// falls back to an existing DOTNET_ROOT env var, then `~/.dotnet`
        /// if that directory exists (matching the proto dotnet plugin layout).
        pub dotnet_root: Option<String>,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_schema_builds() {
        let schema = schematic::SchemaBuilder::build_root::<DotnetToolchainConfig>();
        let json = serde_json::to_string(&schema).unwrap();

        assert!(json.contains("inferDependencies"));
        assert!(json.contains("inferTasks"));
        assert!(json.contains("restoreArgs"));
        assert!(json.contains("dotnetRoot"));

        // `inferTasks` must stay a `bool | string[]` union. The derive produces
        // this from the untagged enum; asserting the shape means a change to the
        // enum cannot silently narrow what the setting accepts.
        assert!(
            json.contains(
                r#""operator":"AnyOf","variants_types":[{"ty":{"type":"Boolean"}},{"ty":{"type":"Array","items_type":{"ty":{"type":"String"}}}}]"#
            ),
            "inferTasks lost its bool | string[] union: {json}"
        );
    }

    #[test]
    fn config_defaults_apply() {
        let config: DotnetToolchainConfig = serde_json::from_value(serde_json::json!({})).unwrap();

        assert!(config.infer_dependencies);
        assert_eq!(config.infer_tasks, InferTasksSetting::Enabled(true));
        assert!(config.infer_tasks.any_enabled());
        assert!(config.restore_args.is_empty());
        assert!(config.dotnet_root.is_none());
    }

    #[test]
    fn infer_tasks_accepts_bool_and_list() {
        let config: DotnetToolchainConfig =
            serde_json::from_value(serde_json::json!({ "inferTasks": false })).unwrap();
        assert!(!config.infer_tasks.any_enabled());
        assert!(!config.infer_tasks.includes("build"));

        let config: DotnetToolchainConfig =
            serde_json::from_value(serde_json::json!({ "inferTasks": ["build", "Test"] })).unwrap();
        assert!(config.infer_tasks.any_enabled());
        assert!(config.infer_tasks.includes("build"));
        assert!(config.infer_tasks.includes("test"));
        assert!(!config.infer_tasks.includes("run"));
        assert!(!config.infer_tasks.includes("publish"));

        let config: DotnetToolchainConfig =
            serde_json::from_value(serde_json::json!({ "inferTasks": [] })).unwrap();
        assert!(!config.infer_tasks.any_enabled());
    }
}
