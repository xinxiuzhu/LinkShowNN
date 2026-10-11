//! 多站点链接解析插件。配置在插件加载时取快照，修改后重新加载生效。
//!
//! resolver 负责站点解析，download 负责媒体任务，live 负责直播订阅；
//! delivery 统一生成消息，runtime 编排消息处理，plugin 装配宿主资源。

mod default_config;
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
