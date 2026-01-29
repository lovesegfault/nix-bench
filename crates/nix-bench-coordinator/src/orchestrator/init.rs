//! Benchmark initialization logic
//!
//! Provides `BenchmarkInitializer` for setting up AWS resources needed for
//! benchmark runs, with progress reporting through the `InitProgressReporter` trait.
//!
//! Resources are recorded to the database immediately after creation to minimize
//! the window where orphaned resources could exist.

use super::progress::{InitProgressReporter, InstanceUpdate};
use super::types::{InstanceState, InstanceStatus};
use crate::aws::context::AwsContext;
use crate::aws::ec2::LaunchInstanceConfig;
use crate::aws::resource_guard::{
    Ec2InstanceGuard, IamRoleGuard, ResourceGuardBuilder, S3BucketGuard, SecurityGroupGuard,
    SecurityGroupRuleGuard, create_cleanup_system,
};
use crate::aws::{
    AccountId, Ec2Client, IamClient, S3Client, extract_error_details, get_coordinator_public_ip,
    get_current_account_id,
};
use crate::config::{AgentConfig, RunConfig, detect_system};
use crate::log_buffer::LogBuffer;
use crate::tui::InitPhase;
use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use nix_bench_common::RunId;
use nix_bench_common::jittered_delay_25;
use nix_bench_common::tls::{
    TlsConfig, generate_agent_cert, generate_ca, generate_coordinator_cert,
};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, error, info, instrument};

/// Delay between instance launches to avoid thundering herd (100ms with jitter)
const LAUNCH_STAGGER_MS: u64 = 100;

/// Map of instance_type -> (ca_cert_pem, agent_cert_pem, agent_key_pem)
type AgentCertificateMap = HashMap<String, (String, String, String)>;

/// Services and state needed during initialization
///
/// Bundles together the AWS clients and resource guards to simplify
/// passing them between initialization helper methods.
struct InitServices<'a> {
    ec2: &'a Ec2Client,
    builder: &'a ResourceGuardBuilder,
    guards: &'a mut ResourceGuards,
}

/// Context holding all resources created during initialization
pub struct InitContext {
    pub run_id: RunId,
    pub bucket_name: String,
    pub account_id: AccountId,
    pub region: String,
    pub ec2: Ec2Client,
    pub s3: S3Client,
    pub instance_profile_name: Option<String>,
    pub security_group_id: Option<String>,
    pub coordinator_ip: Option<String>,
    pub agent_certs: HashMap<String, (String, String, String)>,
    /// TLS configuration for coordinator (required for mTLS)
    pub coordinator_tls_config: TlsConfig,
    pub instances: HashMap<String, InstanceState>,
    /// IAM role name created by nix-bench (for cleanup)
    pub iam_role_name: Option<String>,
    /// Security group rules added by nix-bench (sg_id:cidr pairs, for cleanup)
    pub sg_rules: Vec<String>,
}

impl InitContext {
    pub fn instances_with_ips(&self) -> Vec<(String, String)> {
        self.instances
            .iter()
            .filter_map(|(instance_type, state)| {
                state
                    .public_ip
                    .as_ref()
                    .map(|ip| (instance_type.clone(), ip.clone()))
            })
            .collect()
    }

    pub fn has_instances(&self) -> bool {
        !self.instances.is_empty()
    }
}

/// Initializer for benchmark runs
pub struct BenchmarkInitializer<'a> {
    config: &'a RunConfig,
    run_id: RunId,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
}

impl<'a> BenchmarkInitializer<'a> {
    pub fn new(
        config: &'a RunConfig,
        run_id: RunId,
        bucket_name: String,
        agent_x86_64: Option<String>,
        agent_aarch64: Option<String>,
    ) -> Self {
        Self {
            config,
            run_id,
            bucket_name,
            agent_x86_64,
            agent_aarch64,
        }
    }

    #[instrument(skip_all, fields(run_id = %self.run_id, bucket = %self.bucket_name))]
    pub async fn initialize<R: InitProgressReporter>(&self, reporter: &R) -> Result<InitContext> {
        reporter.report_phase(InitPhase::Starting);
        reporter.report_run_info(self.run_id.as_str(), &self.bucket_name);
        info!(run_id = %self.run_id, bucket = %self.bucket_name, "Starting benchmark run");

        // Phase 1: AWS setup
        let aws = AwsContext::with_profile(
            &self.config.aws.region,
            self.config.aws.aws_profile.as_deref(),
        )
        .await;
        let account_id = get_current_account_id(aws.sdk_config()).await?;
        info!(account_id = %account_id, "AWS account validated");
        reporter.report_account_info(account_id.as_str());

        let ec2 = Ec2Client::from_context(&aws);
        let s3 = S3Client::from_context(&aws);

        // Create the cleanup system for RAII resource protection
        let (registry, executor) = create_cleanup_system();
        let executor_handle = tokio::spawn(executor.run());
        let builder = ResourceGuardBuilder::new(
            registry.clone(),
            self.run_id.as_str(),
            &self.config.aws.region,
        );

        // Track all guards so they aren't dropped until we're done
        let mut guards = ResourceGuards::default();

        // Phase 2: Create S3 bucket and apply tags
        reporter.report_phase(InitPhase::CreatingBucket);
        s3.create_bucket(&self.bucket_name).await?;
        s3.tag_bucket(&self.bucket_name, self.run_id.as_str())
            .await?;
        guards.s3_bucket = Some(builder.s3_bucket(self.bucket_name.clone()));
        debug!(bucket = %self.bucket_name, "S3 bucket created");

        // Phase 3: Create IAM role/profile if needed
        let (instance_profile_name, iam_role_name) = if self.config.aws.instance_profile.is_some() {
            (self.config.aws.instance_profile.clone(), None)
        } else {
            reporter.report_phase(InitPhase::CreatingIamRole);
            let iam = IamClient::from_context(&aws);
            let (role_name, profile_name) = iam
                .create_benchmark_role(self.run_id.as_str(), &self.bucket_name, None)
                .await?;
            guards.iam_role = Some(builder.iam_role(role_name.clone(), profile_name.clone()));
            debug!(role = %role_name, profile = %profile_name, "IAM role/profile created");
            (Some(profile_name), Some(role_name))
        };

        // Phase 4: Upload agent binaries (before launching instances)
        reporter.report_phase(InitPhase::UploadingAgents);
        self.upload_agents(&s3).await?;

        // Phase 5: Setup security group
        let mut services = InitServices {
            ec2: &ec2,
            builder: &builder,
            guards: &mut guards,
        };
        let (security_group_id, coordinator_ip, sg_rule_ids) =
            self.setup_security_group(&mut services).await?;

        // Phase 6: Launch instances
        reporter.report_phase(InitPhase::LaunchingInstances);
        let launched = self
            .launch_instances(
                &mut services,
                security_group_id.as_deref(),
                instance_profile_name.as_deref(),
                reporter,
            )
            .await?;

        if launched.is_empty() {
            reporter.report_phase(InitPhase::Failed("No instances launched".to_string()));
            anyhow::bail!("No instances were launched successfully");
        }

        // Phase 7: Wait for instances and get their dynamic public IPs
        reporter.report_phase(InitPhase::WaitingForInstances);
        let instances = self.wait_for_instances(launched, &ec2, reporter).await?;

        // Phase 8: Generate TLS certificates using dynamic public IPs
        let public_ips: HashMap<String, String> = instances
            .iter()
            .filter_map(|(t, s)| s.public_ip.as_ref().map(|ip| (t.clone(), ip.clone())))
            .collect();

        if public_ips.is_empty() {
            anyhow::bail!("No instances have public IPs - cannot generate TLS certificates");
        }

        let (agent_certs, coordinator_tls_config) = self.generate_certificates(&public_ips)?;

        // Phase 9: Upload agent configs (with TLS certificates)
        self.upload_configs(&s3, &agent_certs).await?;
        info!("Agent configs with TLS certificates uploaded - agents will poll and start gRPC");

        // All resources created and recorded - commit all guards
        guards.commit_all();
        debug!("All resource guards committed");

        // Shutdown cleanup system
        registry.shutdown();
        let _ = executor_handle.await;

        Ok(InitContext {
            run_id: self.run_id.clone(),
            bucket_name: self.bucket_name.clone(),
            account_id,
            region: self.config.aws.region.clone(),
            ec2,
            s3,
            instance_profile_name,
            security_group_id,
            coordinator_ip,
            agent_certs,
            coordinator_tls_config,
            instances,
            iam_role_name,
            sg_rules: sg_rule_ids,
        })
    }

    /// Setup security group
    async fn setup_security_group(
        &self,
        svc: &mut InitServices<'_>,
    ) -> Result<(Option<String>, Option<String>, Vec<String>)> {
        let coordinator_ip = get_coordinator_public_ip().await.ok();
        let mut sg_rule_ids = Vec::new();

        let security_group_id = if let Some(ref sg_id) = self.config.aws.security_group_id {
            // User-provided security group - just add the rule
            if let Some(ref ip) = coordinator_ip {
                let cidr = format!("{}/32", ip);
                if svc.ec2.add_grpc_ingress_rule(sg_id, &cidr).await.is_ok() {
                    let rule_id = format!("{}:{}", sg_id, cidr);
                    svc.guards
                        .sg_rules
                        .push(svc.builder.security_group_rule(sg_id.clone(), cidr.clone()));
                    sg_rule_ids.push(rule_id);
                }
            }
            Some(sg_id.clone())
        } else if let Some(ref ip) = coordinator_ip {
            // Create new security group
            match svc
                .ec2
                .create_security_group(self.run_id.as_str(), &format!("{}/32", ip), None)
                .await
            {
                Ok(sg_id) => {
                    svc.guards.security_group = Some(svc.builder.security_group(sg_id.clone()));
                    debug!(sg_id = %sg_id, "Security group created");
                    Some(sg_id)
                }
                Err(e) => {
                    error!(error = ?e, "Failed to create security group");
                    None
                }
            }
        } else {
            None
        };

        Ok((security_group_id, coordinator_ip, sg_rule_ids))
    }

    /// Launch instances
    async fn launch_instances<R: InitProgressReporter>(
        &self,
        svc: &mut InitServices<'_>,
        security_group_id: Option<&str>,
        instance_profile_name: Option<&str>,
        reporter: &R,
    ) -> Result<HashMap<String, InstanceState>> {
        let mut instances = HashMap::new();
        for (i, instance_type) in self.config.instances.instance_types.iter().enumerate() {
            // Stagger launches after the first instance to avoid thundering herd
            if i > 0 {
                let delay = jittered_delay_25(Duration::from_millis(LAUNCH_STAGGER_MS));
                tokio::time::sleep(delay).await;
            }

            let system = detect_system(instance_type);
            let user_data = super::user_data::generate_user_data(
                &self.bucket_name,
                self.run_id.as_str(),
                instance_type,
            );
            let mut launch_config =
                LaunchInstanceConfig::new(self.run_id.as_str(), instance_type, system, &user_data);
            if let Some(subnet) = &self.config.aws.subnet_id {
                launch_config = launch_config.with_subnet(subnet);
            }
            if let Some(sg) = security_group_id {
                launch_config = launch_config.with_security_group(sg);
            }
            if let Some(profile) = instance_profile_name {
                launch_config = launch_config.with_iam_profile(profile);
            }
            match svc.ec2.launch_instance(launch_config).await {
                Ok(launched) => {
                    svc.guards
                        .ec2_instances
                        .push(svc.builder.ec2_instance(launched.instance_id.clone()));
                    debug!(instance_id = %launched.instance_id, instance_type = %instance_type, "Instance launched");

                    reporter.report_instance_update(InstanceUpdate {
                        instance_type: instance_type.clone(),
                        instance_id: launched.instance_id.clone(),
                        status: InstanceStatus::Launching,
                        public_ip: None,
                    });
                    instances.insert(
                        instance_type.clone(),
                        InstanceState {
                            instance_id: launched.instance_id,
                            instance_type: instance_type.clone(),
                            system,
                            status: InstanceStatus::Launching,
                            run_progress: 0,
                            run_results: Vec::new(),
                            total_runs: self.config.benchmark.runs,
                            public_ip: None,
                            console_output: LogBuffer::default(),
                        },
                    );
                }
                Err(e) => {
                    error!(instance_type = %instance_type, error = ?e, "Failed to launch");
                    reporter.report_instance_update(InstanceUpdate {
                        instance_type: instance_type.clone(),
                        instance_id: String::new(),
                        status: InstanceStatus::Failed,
                        public_ip: None,
                    });

                    // Extract detailed error information for display
                    let details = extract_error_details(&e);
                    let suggestion_line = details
                        .suggestion
                        .map(|s| format!("Suggestion: {}\n\n", s))
                        .unwrap_or_default();

                    // Send error to TUI console output
                    let error_msg = format!(
                        "=== Instance Launch Failed ===\n\n\
                         Instance type: {}\n\
                         Error code: {}\n\
                         Error: {}\n\n\
                         {}\
                         This instance will not be available for benchmarking.",
                        instance_type,
                        details.code.as_deref().unwrap_or("N/A"),
                        details.message,
                        suggestion_line
                    );
                    reporter.report_console_output(instance_type, error_msg);
                }
            }
        }
        Ok(instances)
    }

    fn generate_certificates(
        &self,
        public_ips: &HashMap<String, String>,
    ) -> Result<(AgentCertificateMap, TlsConfig)> {
        if public_ips.is_empty() {
            anyhow::bail!("No public IPs available - cannot generate TLS certificates");
        }

        let ca = generate_ca(self.run_id.as_str())?;
        let coordinator_cert = generate_coordinator_cert(&ca.cert_pem, &ca.key_pem)?;
        let coordinator_tls = TlsConfig {
            ca_cert_pem: ca.cert_pem.clone(),
            cert_pem: coordinator_cert.cert_pem,
            key_pem: coordinator_cert.key_pem,
        };

        let mut agent_certs = HashMap::new();
        for (instance_type, public_ip) in public_ips {
            let cert =
                generate_agent_cert(&ca.cert_pem, &ca.key_pem, instance_type, Some(public_ip))
                    .with_context(|| {
                        format!(
                            "Failed to generate TLS certificate for instance type {}",
                            instance_type
                        )
                    })?;
            agent_certs.insert(
                instance_type.clone(),
                (ca.cert_pem.clone(), cert.cert_pem, cert.key_pem),
            );
        }
        Ok((agent_certs, coordinator_tls))
    }

    async fn upload_agents(&self, s3: &S3Client) -> Result<()> {
        if let Some(ref path) = self.agent_x86_64 {
            s3.upload_file(
                &self.bucket_name,
                &format!("{}/agent-x86_64", self.run_id),
                std::path::Path::new(path),
            )
            .await?;
        }
        if let Some(ref path) = self.agent_aarch64 {
            s3.upload_file(
                &self.bucket_name,
                &format!("{}/agent-aarch64", self.run_id),
                std::path::Path::new(path),
            )
            .await?;
        }
        Ok(())
    }

    async fn upload_configs(
        &self,
        s3: &S3Client,
        agent_certs: &HashMap<String, (String, String, String)>,
    ) -> Result<()> {
        for instance_type in &self.config.instances.instance_types {
            let system = detect_system(instance_type);
            let (ca_cert_pem, agent_cert_pem, agent_key_pem) = agent_certs
                .get(instance_type)
                .map(|(ca, cert, key)| (Some(ca.clone()), Some(cert.clone()), Some(key.clone())))
                .unwrap_or((None, None, None));

            let agent_config = AgentConfig {
                run_id: self.run_id.to_string(),
                bucket: self.bucket_name.clone(),
                region: self.config.aws.region.clone(),
                attr: self.config.benchmark.attr.clone(),
                runs: self.config.benchmark.runs,
                instance_type: instance_type.clone(),
                system,
                flake_ref: self.config.benchmark.flake_ref.clone(),
                build_timeout: self.config.benchmark.build_timeout,
                max_failures: self.config.benchmark.max_failures,
                gc_between_runs: self.config.benchmark.gc_between_runs,
                ca_cert_pem,
                agent_cert_pem,
                agent_key_pem,
            };
            let config_json = serde_json::to_string_pretty(&agent_config)?;
            s3.upload_bytes(
                &self.bucket_name,
                &format!("{}/config-{}.json", self.run_id, instance_type),
                config_json.into_bytes(),
                "application/json",
            )
            .await?;
        }
        Ok(())
    }

    /// Wait for all instances to be running in parallel
    ///
    /// Instances are waited on concurrently and TUI updates happen as each
    /// instance becomes ready, rather than waiting for all to complete.
    async fn wait_for_instances<R: InitProgressReporter>(
        &self,
        mut instances: HashMap<String, InstanceState>,
        ec2: &Ec2Client,
        reporter: &R,
    ) -> Result<HashMap<String, InstanceState>> {
        // Create futures for all instances to wait in parallel
        let mut futures: FuturesUnordered<_> = instances
            .iter()
            .map(|(instance_type, state)| {
                let instance_type = instance_type.clone();
                let instance_id = state.instance_id.clone();
                async move {
                    let result = ec2.wait_for_running(&instance_id, None).await;
                    (instance_type, instance_id, result)
                }
            })
            .collect();

        // Process results as each instance becomes ready
        while let Some((instance_type, instance_id, result)) = futures.next().await {
            if let Some(state) = instances.get_mut(&instance_type) {
                match result {
                    Ok(dynamic_ip) => {
                        state.public_ip = dynamic_ip;
                        state.status = InstanceStatus::Starting;
                        reporter.report_instance_update(InstanceUpdate {
                            instance_type: instance_type.clone(),
                            instance_id: instance_id.clone(),
                            status: InstanceStatus::Starting,
                            public_ip: state.public_ip.clone(),
                        });
                    }
                    Err(e) => {
                        error!(instance_type = %instance_type, error = ?e, "Instance failed to start");
                        state.status = InstanceStatus::Failed;
                        reporter.report_instance_update(InstanceUpdate {
                            instance_type: instance_type.clone(),
                            instance_id: instance_id.clone(),
                            status: InstanceStatus::Failed,
                            public_ip: None,
                        });

                        // Extract detailed error information for display
                        // Note: The error message now includes StateReason from AWS
                        let details = extract_error_details(&e);
                        let suggestion_line = details
                            .suggestion
                            .map(|s| format!("Suggestion: {}\n\n", s))
                            .unwrap_or_default();

                        // Send error to TUI console output
                        let error_msg = format!(
                            "=== Instance Failed to Start ===\n\n\
                             Instance type: {}\n\
                             Instance ID: {}\n\
                             Error code: {}\n\
                             Error: {}\n\n\
                             {}\
                             The instance did not reach 'running' state.",
                            instance_type,
                            instance_id,
                            details.code.as_deref().unwrap_or("N/A"),
                            details.message,
                            suggestion_line
                        );
                        reporter.report_console_output(&instance_type, error_msg);
                    }
                }
            }
        }

        Ok(instances)
    }
}

/// Container for all resource guards created during initialization
///
/// Holds guards until they can be committed after successful DB recording.
/// If dropped without commit (e.g., due to panic or early return), the guards
/// will trigger cleanup of the associated AWS resources.
#[derive(Default)]
struct ResourceGuards {
    s3_bucket: Option<S3BucketGuard>,
    iam_role: Option<IamRoleGuard>,
    security_group: Option<SecurityGroupGuard>,
    sg_rules: Vec<SecurityGroupRuleGuard>,
    ec2_instances: Vec<Ec2InstanceGuard>,
}

impl ResourceGuards {
    /// Commit all guards, indicating successful initialization
    fn commit_all(&mut self) {
        if let Some(guard) = self.s3_bucket.take() {
            guard.commit();
        }
        if let Some(guard) = self.iam_role.take() {
            guard.commit();
        }
        if let Some(guard) = self.security_group.take() {
            guard.commit();
        }
        for guard in self.sg_rules.drain(..) {
            guard.commit();
        }
        for guard in self.ec2_instances.drain(..) {
            guard.commit();
        }
    }
}
