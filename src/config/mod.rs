//! 配置模块入口；类型与默认声明在 default_config，加载和目录选择在 model。

mod commands;
mod default_config;
mod model;

pub(crate) use commands::commands;
pub use default_config::ResolverConfig;

#[cfg(test)]
mod tests;
