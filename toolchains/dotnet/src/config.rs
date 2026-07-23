use moon_pdk_api::config_struct;
use schematic::Config;

config_struct!(
    /// Configures and enables the .NET toolchain.
    #[derive(Config)]
    pub struct DotnetToolchainConfig {
        /// Infer moon project dependencies from MSBuild ProjectReference items.
        #[setting(default = true)]
        pub infer_dependencies: bool,
    }
);
