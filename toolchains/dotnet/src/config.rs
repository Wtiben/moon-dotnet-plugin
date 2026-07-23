use moon_pdk_api::config_struct;
use schematic::Config;

config_struct!(
    /// Configures and enables the .NET toolchain.
    #[derive(Config)]
    pub struct DotnetToolchainConfig {
        /// Infer moon project dependencies from MSBuild `ProjectReference` items
        /// (runs a real MSBuild evaluation per project).
        #[setting(default = true)]
        pub infer_dependencies: bool,

        /// Infer `test` / `run` tasks from evaluated MSBuild properties
        /// (IsTestProject, OutputType). Experimental; off by default.
        pub infer_tasks: bool,

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
    }

    #[test]
    fn config_defaults_apply() {
        let config: DotnetToolchainConfig = serde_json::from_value(serde_json::json!({})).unwrap();

        assert!(config.infer_dependencies);
        assert!(!config.infer_tasks);
        assert!(config.restore_args.is_empty());
        assert!(config.dotnet_root.is_none());
    }
}
