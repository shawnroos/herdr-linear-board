//! Read-only Linear GraphQL client for boardd.

mod cache;
mod client;
mod credential;
mod queries;
mod read;
mod service;

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;

pub use cache::{CacheRead, Debouncer, SnapshotCache, SpaceKey};
pub use client::{LinearClient, LinearConfig, LinearError, Page, API_URL_ENV, DEFAULT_API_URL};
pub use credential::{
    platform_keychain, ApiKey, CredentialResolver, CredentialSource, KeychainRead, KeychainReader,
    NoKeychain, SecurityCli,
};
pub use read::{fetch, names_project, FetchPlan, SpaceRead};
pub use service::LinearService;
