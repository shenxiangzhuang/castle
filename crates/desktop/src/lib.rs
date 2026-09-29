#![cfg_attr(
    not(test),
    deny(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::unwrap_used
    )
)]

//! Desktop composition. Execution and durable state belong to harness and agent.
mod app;
#[cfg(test)]
mod architecture_tests;
mod bootstrap;
mod chat;
mod platform;
mod rendering;
mod session;
mod settings;
mod trajectory;
mod workspace;

pub(crate) use bootstrap::APP_NAME;
pub use bootstrap::run;
