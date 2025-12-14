//! AWS account validation and identity

use anyhow::{Context, Result};
use std::fmt;
use tracing::info;

/// Strongly-typed AWS account ID (12-digit string)
///
/// This newtype prevents accidentally mixing account IDs with other strings
/// and ensures account validation happens at specific points in the code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccountId(String);

impl AccountId {
    /// Get the account ID as a string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Create an AccountId for testing purposes
    #[cfg(test)]
    pub fn new(s: String) -> Self {
        AccountId(s)
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Fetch the current AWS account ID from credentials via STS GetCallerIdentity
///
/// This operation requires no special permissions - it always succeeds if
/// credentials are valid. Use this to validate credentials and capture the
/// account ID at the start of operations.
pub async fn get_current_account_id(config: &aws_config::SdkConfig) -> Result<AccountId> {
    let sts = aws_sdk_sts::Client::new(config);
    let identity = sts
        .get_caller_identity()
        .send()
        .await
        .context("Failed to get AWS caller identity - check credentials")?;

    let account = identity
        .account()
        .context("No account ID returned from STS GetCallerIdentity")?;

    info!(account_id = %account, "AWS account validated");

    Ok(AccountId(account.to_string()))
}
