// 取件自 goose（https://github.com/block/goose），Apache-2.0，
// Copyright Block, Inc. 见仓库根目录的 NOTICE。
//
// **这份代码此后由 Cortex 自行维护，不再与上游同步。** 改它按本仓库的
// 判据来，不必去看上游怎么写 —— 那边的分支已经不是我们这份的未来。
macro_rules! expose_declarative_provider {
    ($module:ident, $definition:expr) => {
        pub mod $module {
            use anyhow::Result;

            use crate::{
                api_client::TlsConfig,
                base::Provider,
                declarative::{from_json, KeyResolver},
            };

            pub const JSON: &str = include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/declarative/definitions/",
                $definition,
                ".json"
            ));

            pub fn create(
                tls_config: Option<TlsConfig>,
                key_resolver: impl KeyResolver,
            ) -> Result<Box<dyn Provider>> {
                from_json(JSON, tls_config, key_resolver)
            }
        }
    };
}

macro_rules! expose_declarative_providers {
    ($($module:ident),+ $(,)?) => {
        $(expose_declarative_provider!($module, stringify!($module));)+

        pub(crate) fn fixed_provider_configs() -> anyhow::Result<Vec<DeclarativeProviderConfig>> {
            fixed_provider_config_entries()
                .into_iter()
                .map(|(_, json)| deserialize_provider_config(json))
                .collect()
        }

        pub(crate) fn fixed_provider_config_entries() -> Vec<(&'static str, &'static str)> {
            vec![$((concat!(stringify!($module), ".json"), $module::JSON)),+]
        }
    };
}
