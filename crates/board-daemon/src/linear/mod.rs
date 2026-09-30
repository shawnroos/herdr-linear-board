//! Read-only Linear GraphQL client for boardd.

mod client;
mod credential;
mod queries;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub use client::{LinearClient, LinearConfig, LinearError, Page, API_URL_ENV, DEFAULT_API_URL};
pub use credential::{
    platform_keychain, ApiKey, CredentialResolver, CredentialSource, KeychainRead, KeychainReader,
    NoKeychain, SecurityCli,
};
