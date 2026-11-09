use std::sync::Arc;

use crate::plugin_registry::{FormatProvider, PluginRegistration};

pub struct SdcpV1FormatProvider;

impl FormatProvider for SdcpV1FormatProvider {
    fn default_export_format(&self) -> &'static str {
        "print"
    }
}

pub fn get_plugin_registration() -> PluginRegistration {
    PluginRegistration {
        name: "sdcp-v1".to_string(),
        network_handler: None,
        format_provider: Some(Arc::new(SdcpV1FormatProvider)),
    }
}
