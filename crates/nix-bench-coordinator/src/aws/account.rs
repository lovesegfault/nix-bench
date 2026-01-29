//! AWS account validation and identity

use anyhow::{Context, Result};
use tracing::info;

/// Strongly-typed AWS account ID (12-digit string)
///
/// This newtype prevents accidentally mixing account IDs with other strings
/// and ensures account validation happens at specific points in the code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::Display, derive_more::Deref)]
pub struct AccountId(String);

impl AccountId {
    /// Create an AccountId for testing purposes
    #[cfg(test)]
    pub fn new(s: String) -> Self {
        AccountId(s)
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
