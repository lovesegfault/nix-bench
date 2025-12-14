//! S3 integration tests - actually call AWS APIs
//!
//! These tests are marked `#[ignore]` and only run with:
//! ```
//! AWS_PROFILE=your_profile cargo test --test aws_s3_integration -- --ignored
//! ```

mod aws_test_helpers;

use aws_test_helpers::*;
use nix_bench_coordinator::aws::S3Client;
use std::path::PathBuf;
use tempfile::NamedTempFile;
use std::io::Write;

/// Test bucket create, upload, and delete lifecycle
#[tokio::test]
#[ignore]
async fn test_bucket_lifecycle() {
    let region = get_test_region();
    let client = S3Client::new(&region)
        .await
        .expect("AWS credentials required - set AWS_PROFILE or AWS_ACCESS_KEY_ID");

    let run_id = test_run_id();
    let bucket_name = format!("nix-bench-{}", run_id);

    // Create bucket
    client
        .create_bucket(&bucket_name)
        .await
        .expect("Should create bucket");

    // Create a temp file to upload
    let mut temp_file = NamedTempFile::new().expect("Should create temp file");
    writeln!(temp_file, "test content for integration test").expect("Should write to temp file");
    let temp_path = temp_file.path().to_path_buf();

    // Upload file
    client
        .upload_file(&bucket_name, "test-file.txt", &temp_path)
        .await
        .expect("Should upload file");

    // Delete bucket (also deletes all objects)
    client
        .delete_bucket(&bucket_name)
        .await
        .expect("Should delete bucket");
}

/// Test uploading bytes with content type
#[tokio::test]
#[ignore]
async fn test_upload_bytes() {
    let region = get_test_region();
    let client = S3Client::new(&region)
        .await
        .expect("AWS credentials required");

    let run_id = test_run_id();
    let bucket_name = format!("nix-bench-{}", run_id);

    // Create bucket
    client
        .create_bucket(&bucket_name)
        .await
        .expect("Should create bucket");

    // Upload JSON bytes
    let json_data = r#"{"test": "data", "number": 42}"#;
    client
        .upload_bytes(
            &bucket_name,
            "test-data.json",
            json_data.as_bytes().to_vec(),
            "application/json",
        )
        .await
        .expect("Should upload JSON bytes");

    // Upload more bytes to test multiple uploads
    let config_data = r#"{"instance_type": "c7a.medium", "runs": 5}"#;
    client
        .upload_bytes(
            &bucket_name,
            "config.json",
            config_data.as_bytes().to_vec(),
            "application/json",
        )
        .await
        .expect("Should upload config bytes");

    // Cleanup
    client
        .delete_bucket(&bucket_name)
        .await
        .expect("Should delete bucket");
}
