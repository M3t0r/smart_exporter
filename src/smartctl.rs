use std::vec::Vec;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json;
use anyhow::{Context, Result, bail};

use std::process::Command;
use std::ffi::OsStr;

const SUPPORTED_JSON_FORMAT_VERSION: &[u8] = &[1, 0];

pub trait SmartctlInvoker {
    fn construct_command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>;

    fn call<I, S, O>(&self, args: I) -> Result<(O, Version)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        O: Serialize + DeserializeOwned,
    {
        let mut cmd = self.construct_command(args);
        let output = cmd.output()?;

        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "failed to execute {:?}: {}\n{}\n{}",
                cmd,
                output.status,
                stdout,
                stderr,
            );
        }

        let parsed: Output<O> = serde_json::from_slice(&output.stdout)
            .context("failed to parse smartctl output")?;

        if parsed.json_format_version != SUPPORTED_JSON_FORMAT_VERSION {
            eprintln!(
                "warning: unkown JSON output format from smartctl: {}",
                parsed
                    .json_format_version
                    .iter()
                    .map(|i|i.to_string())
                    .collect::<Vec<_>>()
                    .join(".")
            );
        }

        Ok((parsed.output, parsed.smartctl.version_details))
    }
}

#[derive(Debug)]
pub struct SudoInvoker {}

impl SmartctlInvoker for SudoInvoker{
    fn construct_command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>
    {
        let mut cmd = Command::new("sudo");
        cmd
            .env_clear()
            .env("PATH", "/bin/:/sbin/:/usr/bin/:/usr/sbin/")
            .args(["--non-interactive", "--"])
            .args(["smartctl", "--json"])
            .args(args);
        cmd
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Output<O> {
    json_format_version: Vec<u8>,
    smartctl: InvocationDetails,
    #[serde(flatten)]
    output: O,
}

#[derive(Debug, Deserialize, Serialize)]
struct InvocationDetails {
    #[serde(flatten)]
    version_details: Version,
    argv: Vec<String>,
    exit_status: u8,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Version {
    version: Vec<u8>,
    svn_revision: String,
    platform_info: String,
}

pub mod scan {
    use std::option::Option;
    use std::vec::Vec;
    use std::path::PathBuf;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Scan {
        pub devices: Vec<Device>,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Device {
        pub name: PathBuf,
        pub info_name: String,
        pub r#type: Type,
        pub protocol: Protocol,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    #[serde(rename_all = "lowercase")] 
    pub enum Type {
        Sat,
        Nvme,
    }

    #[derive(Debug, Deserialize, Serialize, Clone, Copy, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelValue)]
    #[serde(rename_all = "UPPERCASE")] 
    pub enum Protocol {
        Ata,
        #[serde(rename = "NVMe")] 
        NVMe,
    }
}
