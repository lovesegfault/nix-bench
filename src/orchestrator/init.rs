//! Benchmark initialization logic
//!
//! Provides `BenchmarkInitializer` for setting up AWS resources needed for
//! benchmark runs, with progress reporting through the `InitProgressReporter` trait.

use super::progress::{InitProgressReporter, InstanceUpdate};
use super::types::{InstanceState, InstanceStatus};
use crate::aws::{
    get_coordinator_public_ip, get_current_account_id, AccountId, Ec2Client, IamClient, S3Client,
};
use crate::aws_context::AwsContext;
use crate::config::{detect_system, AgentConfig, RunConfig};
use crate::state::{self, DbPool, ResourceType};
use crate::tls::{generate_agent_cert, generate_ca, generate_coordinator_cert, TlsConfig};
use crate::tui::{InitPhase, LogBuffer};
use anyhow::Result;
use std::collections::HashMap;
use tracing::{error, info, warn};

/// Context holding all resources created during initialization
pub struct InitContext {
    pub run_id: String,
    pub bucket_name: String,
    pub account_id: AccountId,
    pub region: String,
    pub db: DbPool,
    pub ec2: Ec2Client,
    pub s3: S3Client,
    pub instance_profile_name: Option<String>,
    pub security_group_id: Option<String>,
    pub coordinator_ip: Option<String>,
    pub eip_allocations: HashMap<String, (String, String)>,
    pub agent_certs: HashMap<String, (String, String, String)>,
    pub coordinator_tls_config: Option<TlsConfig>,
    pub instances: HashMap<String, InstanceState>,
}

impl InitContext {
    pub fn instances_with_ips(&self) -> Vec<(String, String)> {
        self.instances
            .iter()
            .filter_map(|(instance_type, state)| {
                state.public_ip.as_ref().map(|ip| (instance_type.clone(), ip.clone()))
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
    run_id: String,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
}

impl<'a> BenchmarkInitializer<'a> {
    pub fn new(
        config: &'a RunConfig,
        run_id: String,
        bucket_name: String,
        agent_x86_64: Option<String>,
        agent_aarch64: Option<String>,
    ) -> Self {
        Self { config, run_id, bucket_name, agent_x86_64, agent_aarch64 }
    }

    pub async fn initialize<R: InitProgressReporter>(&self, reporter: &R) -> Result<InitContext> {
        reporter.report_phase(InitPhase::Starting);
        reporter.report_run_info(&self.run_id, &self.bucket_name);
        info!(run_id = %self.run_id, bucket = %self.bucket_name, "Starting benchmark run");

        // Phase 1: AWS setup
        let aws = AwsContext::new(&self.config.region).await;
        let account_id = get_current_account_id(aws.sdk_config()).await?;
        info!(account_id = %account_id, "AWS account validated");
        reporter.report_account_info(account_id.as_str());

        let ec2 = Ec2Client::from_context(&aws);
        let s3 = S3Client::from_context(&aws);

        // Phase 2: Create S3 bucket
        reporter.report_phase(InitPhase::CreatingBucket);
        s3.create_bucket(&self.bucket_name).await?;

        // Phase 3: Create IAM role/profile if needed
        let (instance_profile_name, iam_role_name) = if self.config.instance_profile.is_some() {
            (self.config.instance_profile.clone(), None)
        } else {
            reporter.report_phase(InitPhase::CreatingIamRole);
            let iam = IamClient::from_context(&aws);
            let (role_name, profile_name) = iam
                .create_benchmark_role(&self.run_id, &self.bucket_name, None)
                .await?;
            (Some(profile_name), Some(role_name))
        };

        // Phase 4: Allocate Elastic IPs
        let eip_allocations = self.allocate_elastic_ips(&ec2).await;

        // Phase 5: Generate TLS certificates
        let (agent_certs, coordinator_tls_config) = self.generate_certificates(&eip_allocations)?;

        // Phase 6: Upload agents
        reporter.report_phase(InitPhase::UploadingAgents);
        self.upload_agents(&s3).await?;
        self.upload_configs(&s3, &agent_certs).await?;

        // Phase 7: Setup security group
        let (security_group_id, coordinator_ip, sg_rule_id) = self.setup_security_group(&ec2).await?;

        // Phase 8: Launch instances
        reporter.report_phase(InitPhase::LaunchingInstances);
        let launched = self
            .launch_instances(&ec2, security_group_id.as_deref(), instance_profile_name.as_deref(), reporter)
            .await?;

        if launched.is_empty() {
            reporter.report_phase(InitPhase::Failed("No instances launched".to_string()));
            anyhow::bail!("No instances were launched successfully");
        }

        // Phase 9: Wait for instances
        reporter.report_phase(InitPhase::WaitingForInstances);
        let instances = self.wait_for_instances(launched, &ec2, &eip_allocations, reporter).await?;

        // All async work done, now record to database
        let db = state::open_db().await?;

        state::insert_run(&db, &self.run_id, &account_id, &self.config.region, &self.config.instance_types, &self.config.attr).await?;
        state::insert_resource(&db, &self.run_id, &account_id, ResourceType::S3Bucket, &self.bucket_name, &self.config.region).await?;

        if let Some(ref role_name) = iam_role_name {
            state::insert_resource(&db, &self.run_id, &account_id, ResourceType::IamRole, role_name, &self.config.region).await?;
        }
        if let Some(ref profile_name) = instance_profile_name {
            if iam_role_name.is_some() {
                state::insert_resource(&db, &self.run_id, &account_id, ResourceType::IamInstanceProfile, profile_name, &self.config.region).await?;
            }
        }

        for (allocation_id, _) in eip_allocations.values() {
            state::insert_resource(&db, &self.run_id, &account_id, ResourceType::ElasticIp, allocation_id, &self.config.region).await?;
        }

        if let Some(ref sg_id) = security_group_id {
            if self.config.security_group_id.is_none() {
                state::insert_resource(&db, &self.run_id, &account_id, ResourceType::SecurityGroup, sg_id, &self.config.region).await?;
            }
        }

        if let Some(ref rule_id) = sg_rule_id {
            state::insert_resource(&db, &self.run_id, &account_id, ResourceType::SecurityGroupRule, rule_id, &self.config.region).await?;
        }

        for state in instances.values() {
            state::insert_resource(&db, &self.run_id, &account_id, ResourceType::Ec2Instance, &state.instance_id, &self.config.region).await?;
        }

        Ok(InitContext {
            run_id: self.run_id.clone(),
            bucket_name: self.bucket_name.clone(),
            account_id,
            region: self.config.region.clone(),
            db, ec2, s3,
            instance_profile_name, security_group_id, coordinator_ip,
            eip_allocations, agent_certs, coordinator_tls_config, instances,
        })
    }

    async fn allocate_elastic_ips(&self, ec2: &Ec2Client) -> HashMap<String, (String, String)> {
        let mut eip_allocations = HashMap::new();
        for instance_type in &self.config.instance_types {
            match ec2.allocate_elastic_ip(&self.run_id).await {
                Ok((allocation_id, public_ip)) => {
                    info!(instance_type = %instance_type, public_ip = %public_ip, "Allocated EIP");
                    eip_allocations.insert(instance_type.clone(), (allocation_id, public_ip));
                }
                Err(e) => error!(instance_type = %instance_type, error = ?e, "Failed to allocate EIP"),
            }
        }
        eip_allocations
    }

    fn generate_certificates(&self, eip_allocations: &HashMap<String, (String, String)>) -> Result<(HashMap<String, (String, String, String)>, Option<TlsConfig>)> {
        if eip_allocations.is_empty() {
            warn!("No EIPs allocated, mTLS disabled");
            return Ok((HashMap::new(), None));
        }

        let ca = generate_ca(&self.run_id)?;
        let coordinator_cert = generate_coordinator_cert(&ca.cert_pem, &ca.key_pem)?;
        let coordinator_tls = TlsConfig {
            ca_cert_pem: ca.cert_pem.clone(),
            cert_pem: coordinator_cert.cert_pem,
            key_pem: coordinator_cert.key_pem,
        };

        let mut agent_certs = HashMap::new();
        for (instance_type, (_, public_ip)) in eip_allocations {
            match generate_agent_cert(&ca.cert_pem, &ca.key_pem, instance_type, Some(public_ip)) {
                Ok(cert) => { agent_certs.insert(instance_type.clone(), (ca.cert_pem.clone(), cert.cert_pem, cert.key_pem)); }
                Err(e) => error!(instance_type = %instance_type, error = ?e, "Failed to generate cert"),
            }
        }
        Ok((agent_certs, Some(coordinator_tls)))
    }

    async fn upload_agents(&self, s3: &S3Client) -> Result<()> {
        if let Some(ref path) = self.agent_x86_64 {
            s3.upload_file(&self.bucket_name, &format!("{}/agent-x86_64", self.run_id), std::path::Path::new(path)).await?;
        }
        if let Some(ref path) = self.agent_aarch64 {
            s3.upload_file(&self.bucket_name, &format!("{}/agent-aarch64", self.run_id), std::path::Path::new(path)).await?;
        }
        Ok(())
    }

    async fn upload_configs(&self, s3: &S3Client, agent_certs: &HashMap<String, (String, String, String)>) -> Result<()> {
        for instance_type in &self.config.instance_types {
            let system = detect_system(instance_type);
            let (ca_cert_pem, agent_cert_pem, agent_key_pem) = agent_certs.get(instance_type)
                .map(|(ca, cert, key)| (Some(ca.clone()), Some(cert.clone()), Some(key.clone())))
                .unwrap_or((None, None, None));

            let agent_config = AgentConfig {
                run_id: self.run_id.clone(), bucket: self.bucket_name.clone(),
                region: self.config.region.clone(), attr: self.config.attr.clone(),
                runs: self.config.runs, instance_type: instance_type.clone(),
                system: system.to_string(), flake_ref: self.config.flake_ref.clone(),
                build_timeout: self.config.build_timeout, max_failures: self.config.max_failures,
                ca_cert_pem, agent_cert_pem, agent_key_pem,
            };
            let config_json = serde_json::to_string_pretty(&agent_config)?;
            s3.upload_bytes(&self.bucket_name, &format!("{}/config-{}.json", self.run_id, instance_type), config_json.into_bytes(), "application/json").await?;
        }
        Ok(())
    }

    async fn setup_security_group(&self, ec2: &Ec2Client) -> Result<(Option<String>, Option<String>, Option<String>)> {
        let coordinator_ip = get_coordinator_public_ip().await.ok();
        let mut sg_rule_id = None;

        let security_group_id = if let Some(ref sg_id) = self.config.security_group_id {
            if let Some(ref ip) = coordinator_ip {
                let cidr = format!("{}/32", ip);
                if ec2.add_grpc_ingress_rule(sg_id, &cidr).await.is_ok() {
                    sg_rule_id = Some(format!("{}:{}", sg_id, cidr));
                }
            }
            Some(sg_id.clone())
        } else if let Some(ref ip) = coordinator_ip {
            ec2.create_security_group(&self.run_id, &format!("{}/32", ip), None).await.ok()
        } else { None };

        Ok((security_group_id, coordinator_ip, sg_rule_id))
    }

    async fn launch_instances<R: InitProgressReporter>(&self, ec2: &Ec2Client, security_group_id: Option<&str>, instance_profile_name: Option<&str>, reporter: &R) -> Result<HashMap<String, InstanceState>> {
        let mut instances = HashMap::new();
        for instance_type in &self.config.instance_types {
            let system = detect_system(instance_type);
            let user_data = super::generate_user_data(&self.bucket_name, &self.run_id, instance_type);
            match ec2.launch_instance(&self.run_id, instance_type, system, &user_data, self.config.subnet_id.as_deref(), security_group_id, instance_profile_name).await {
                Ok(launched) => {
                    reporter.report_instance_update(InstanceUpdate { instance_type: instance_type.clone(), instance_id: launched.instance_id.clone(), status: InstanceStatus::Launching, public_ip: None });
                    instances.insert(instance_type.clone(), InstanceState {
                        instance_id: launched.instance_id, instance_type: instance_type.clone(),
                        system: system.to_string(), status: InstanceStatus::Launching,
                        run_progress: 0, total_runs: self.config.runs, durations: Vec::new(),
                        public_ip: None, console_output: LogBuffer::default(),
                    });
                }
                Err(e) => {
                    error!(instance_type = %instance_type, error = ?e, "Failed to launch");
                    reporter.report_instance_update(InstanceUpdate { instance_type: instance_type.clone(), instance_id: String::new(), status: InstanceStatus::Failed, public_ip: None });
                }
            }
        }
        Ok(instances)
    }

    async fn wait_for_instances<R: InitProgressReporter>(&self, mut instances: HashMap<String, InstanceState>, ec2: &Ec2Client, eip_allocations: &HashMap<String, (String, String)>, reporter: &R) -> Result<HashMap<String, InstanceState>> {
        for (instance_type, state) in instances.iter_mut() {
            match ec2.wait_for_running(&state.instance_id, None).await {
                Ok(dynamic_ip) => {
                    if let Some((allocation_id, eip)) = eip_allocations.get(instance_type) {
                        state.public_ip = ec2.associate_elastic_ip(allocation_id, &state.instance_id).await.ok().map(|_| eip.clone()).or(dynamic_ip);
                    } else {
                        state.public_ip = dynamic_ip;
                    }
                    state.status = InstanceStatus::Running;
                    reporter.report_instance_update(InstanceUpdate { instance_type: instance_type.clone(), instance_id: state.instance_id.clone(), status: InstanceStatus::Running, public_ip: state.public_ip.clone() });
                }
                Err(e) => {
                    error!(instance_type = %instance_type, error = ?e, "Instance failed to start");
                    state.status = InstanceStatus::Failed;
                    reporter.report_instance_update(InstanceUpdate { instance_type: instance_type.clone(), instance_id: state.instance_id.clone(), status: InstanceStatus::Failed, public_ip: None });
                }
            }
        }
        Ok(instances)
    }
}
