//! Shared product configuration, defaults and persistence.
mod models;
mod settings;
pub use models::{
    ConfiguredModel, DEEPSEEK_PROVIDER_ID, INSTRUCTIONS, OPENAI_PROVIDER_ID, build_model,
    default_provider_profile,
};
pub use settings::{AppStoreSource, Appearance, ProviderModel, ProviderProfile, SettingsStore};

pub(crate) use models::initial_session_title;
