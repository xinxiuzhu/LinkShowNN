//! 多站点链接解析插件。配置保存后刷新运行快照，进行中的后台任务沿用已有参数。
//!
//! 每个业务模块对应一个目录：plugin 装配宿主资源，config 声明配置；
//! resolver 解析各平台，render 生成卡片，download 下载媒体，delivery 发送消息；
//! live 管直播订阅，runtime 编排处理流程。目录职责见 docs/STRUCTURE.md。

mod auth;
mod config;
mod delivery;
mod download;
mod live;
mod plugin;
mod render;
mod resolver;
mod runtime;

pub use plugin::LinkShowNN;

nuono_plugin::register_plugin!(LinkShowNN);

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[tokio::test]
    async fn plugin_factory_and_teardown_use_host_runtime() {
        let mut plugin = nuono_create_plugin();
        assert_eq!(plugin.name(), "LinkShowNN");
        plugin.on_disable().await.unwrap();
        plugin.on_unload().await.unwrap();
        drop(plugin);
    }
}

#[cfg(test)]
mod sdk_configuration_contract {
    #[test]
    fn declared_configuration_is_valid() {
        let plugin = super::nuono_create_plugin();
        nuono_plugin::config_validation::validate_config_fields(&plugin.config_fields()).unwrap();
    }
}
