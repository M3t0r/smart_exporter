use std::path::{Path, PathBuf};
use std::vec::Vec;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json;
use anyhow::{Context, Result, bail};
use slog::debug;

use std::process::Command;
use std::ffi::OsStr;

const SUPPORTED_JSON_FORMAT_VERSION: &[u8] = &[1, 0];

pub trait SmartctlInvoker {
    fn construct_command<I, S>(&mut self, log: &slog::Logger, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>;

    fn call<I, S, O>(&mut self, log: &slog::Logger, args: I) -> Result<(O, Version)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        O: Serialize + DeserializeOwned,
    {
        let mut cmd = self.construct_command(log, args);
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

        let deser = &mut serde_json::Deserializer::from_slice(&output.stdout);
        let parsed: Output<O> = serde_path_to_error::deserialize(deser)
            .map_err(|e| {
                let c = format!("failed to parse smartctl output, JSON path: {}", e.path().to_string());
                anyhow::Error::new(e).context(c)
            })?;

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
pub struct NormalInvoker {}

impl SmartctlInvoker for NormalInvoker{
    fn construct_command<I, S>(&mut self, _: &slog::Logger, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>
    {
        let mut cmd = Command::new("smartctl");
        cmd
            .env_clear()
            .env("PATH", "/bin/:/sbin/:/usr/bin/:/usr/sbin/")
            .args(["--json"])
            .args(args);
        cmd
    }
}

#[derive(Debug)]
pub struct SudoInvoker {}

impl SmartctlInvoker for SudoInvoker{
    fn construct_command<I, S>(&mut self, _: &slog::Logger, args: I) -> Command
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

#[derive(Debug)]
pub struct FileInvoker {
    iteration: usize,
    base: PathBuf,
}

impl FileInvoker {
    pub fn new(base: &Path) -> Self {
        FileInvoker { iteration: 0, base: base.to_owned() }
    }
}

impl SmartctlInvoker for FileInvoker {
    fn construct_command<I, S>(&mut self, log: &slog::Logger, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>
    {
        let mut normalized_arg: String = args.into_iter()
            .fold(String::new(), |acc, arg| acc + &arg.as_ref().to_string_lossy() + "_")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() {c} else  {'_'})
            .collect();
        normalized_arg.pop(); // remove last '_'

        let indexed = self.base.join(format!("{}/{}", self.iteration, normalized_arg));
        let unindexed = self.base.join(normalized_arg);

        let [stdout, stderr, exit] = ["stdout", "stderr", "exit"].map(|suffix| {
            let i = indexed.with_extension(suffix);
            if i.is_file() {
                i
            } else {
                unindexed.with_extension(suffix)
            }
        });

        let cmds = [&stdout, &stderr, &exit].iter().zip(&[
                format!("cat {}", stdout.display()),
                format!("cat {} 1>&2", stderr.display()),
                format!("exit $(cat {})", exit.display()),
            ])
            .filter(|(f, _)| f.is_file())
            .map(|(_, cmd)| cmd.clone())
            .collect::<Vec::<_>>()
            .join("; ");

        debug!(log, "Reading smartctl output from file"; "folder" => self.base.display(), "iteration" => self.iteration, "replacement command" => &cmds);

        let mut cmd = Command::new("sh");
        cmd
            .env_clear()
            .env("PATH", "/bin/:/sbin/:/usr/bin/:/usr/sbin/")
            .args(["-c", &cmds]);
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

pub mod scan {
    pub use crate::smartctl::Device;
    use std::vec::Vec;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Scan {
        pub devices: Vec<Device>,
    }
}

pub mod stats {
    use std::collections::HashMap;
    use serde::{Deserialize, Serialize};
    pub use crate::smartctl::Device;

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct DeviceStats {
        pub device: Device,
        pub model_family: String,
        pub model_name: String,
        pub serial_number: String,
        pub wwn: WorldWideName,

        pub firmware_version: String,
        pub ata_version: ATAVersion,
        pub sata_version: SATAVersion,
        pub in_smartctl_database: bool,

        pub smart_status: Status,
        pub smart_support: Support,

        // pub ata_sct_capabilities: String,
        pub ata_smart_attributes: Attributes,
        // pub ata_smart_data: String,
        // pub ata_smart_error_log: String,
        // pub ata_smart_selective_self_test_log: String,
        // pub ata_smart_self_test_log: String,

        pub power_on_time: PowerOnTime,
        pub power_cycle_count: u64,
        pub temperature: Temperature,
        pub rotation_rate: u16,
        pub interface_speed: InterfaceSpeed,
        pub trim: Trim,
        pub user_capacity: UserCapacity,
        pub logical_block_size: u16,
        pub physical_block_size: u16,

        // intentionally unused fields: local_time

        #[serde(flatten)]
        pub extra_fields: HashMap<String, serde_json::Value>,
    }

    /// https://en.wikipedia.org/wiki/World_Wide_Name
    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct WorldWideName {
        #[serde(rename = "naa")]
        pub network_address_authority: u8, // 4 bits
        #[serde(rename = "oui")]
        pub organizationally_unique_identifier: u32, // 24 bit, https://en.wikipedia.org/wiki/Organizationally_unique_identifier
        pub id: u64,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Status {
        pub passed: bool,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct ATAVersion {
        pub string: String,
        #[serde(rename="major_value")]
        pub major: u64,
        #[serde(rename="minor_value")]
        pub minor: u64,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct SATAVersion {
        pub string: String,
        pub value: u64,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Support {
        pub available: bool,
        pub enabled: bool,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Attributes {
        pub revision: u16,
        pub table: Vec<Attribute>,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Attribute {
        pub id: u16,
        pub name: String,
        #[serde(rename="value")]
        pub current: u8,
        pub worst: u8,
        #[serde(rename="thresh")]
        pub threshold: u8,
        pub flags: Flags,
        pub raw: RawAttributeValue,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Flags {
        pub prefailure: bool,
        pub updated_online: bool,
        pub performance: bool,
        pub error_rate: bool,
        pub event_count: bool,
        pub auto_keep: bool,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct RawAttributeValue {
        pub value: u64,
        pub string: String,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct PowerOnTime {
        pub hours: u64,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Temperature {
        pub current: i64,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct InterfaceSpeed {
        pub current: OneInterfaceSpeed,
        pub max: OneInterfaceSpeed,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct OneInterfaceSpeed {
        pub sata_value: u8,
        pub string: String,
        pub units_per_second: u32,
        pub bits_per_unit: u32,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct Trim {
        pub supported: bool,
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    pub struct UserCapacity {
        pub blocks: u64,
        pub bytes: u64,
    }
}

#[cfg(test)]
mod test {
    use std::{path::{Path, PathBuf}, ffi::OsStr};
    use std::io::BufReader;
    use std::fs::File;

    use anyhow::anyhow;

    use super::SmartctlInvoker;

    fn walk_dir_recursive(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
        if !dir.is_dir() {
            return Err(anyhow!("Not a directory: {}", dir.display()));
        }

        let mut r = vec![];

        for f in dir.read_dir()? {
            let f = f?.path();
            r.push(f.clone());
            if f.is_dir() {
                r.extend(walk_dir_recursive(&f)?);
            }
        }
        Ok(r)
    }

    #[test]
    fn no_pii_in_fixtures() -> anyhow::Result<()> {
        let fixtures: Vec<_> = walk_dir_recursive(&Path::new("tests/"))
            .expect("could not list all test fixture files");
        let fixtures: Vec<_> = fixtures
            .iter()
            .filter(|f| f.is_file() && f.extension() == Some(OsStr::new("stdout")))
            .collect();

        println!("found fixtures: {}", fixtures.len());

        for f in fixtures {
            let reader = BufReader::new(File::open(&f)?);
            let json: serde_json::Value = serde_json::from_reader(reader)?;

            // https://en.wikipedia.org/wiki/World_Wide_Name
            let wwn = json.pointer("/wwn/id");
            assert!(wwn.is_none() || wwn.unwrap().as_i64() == Some(0), "World Wide Name found: {}", f.display());

            let serial = json.pointer("/serial_number");
            assert!(serial.is_none() || serial.unwrap().as_str() == Some(""), "Serial Number found: {}", f.display());
        }
        Ok(())
    }

    #[test]
    fn file_invoker_simple() {
        let log = crate::make_logger();
        let mut invoker = super::FileInvoker::new(Path::new("tests/simple/"));

        let (scan, version): (crate::smartctl::scan::Scan, _) = invoker.call(&log, ["--scan-open"]).expect("could not parse simple/ scan");

        assert_eq!(version.version, vec![7, 4]);
        assert_eq!(scan.devices.len(), 3);
    }

    #[test]
    fn file_invoker_failing() {
        let log = crate::make_logger();
        let mut invoker = super::FileInvoker::new(Path::new("tests/failed_scan/"));

        let r: anyhow::Result<(crate::smartctl::scan::Scan, _)> = invoker.call(&log, ["--scan-open"]);

        assert!(r.is_err());
    }

    #[test]
    fn parse_device_scan() {
        let log = crate::make_logger();
        let mut invoker = super::FileInvoker::new(Path::new("tests/simple/"));

        let (_scan, _version): (crate::smartctl::scan::Scan, _) = invoker.call(&log, ["--scan-open"]).expect("could not parse simple/ scan");
    }

    #[test]
    fn parse_device_stats() {
        let log = crate::make_logger();
        let mut invoker = super::FileInvoker::new(Path::new("tests/simple/"));

        for dev in ["/dev/sda", "/dev/sdb", "/dev/sdc"] {
            let (stats, _version): (crate::smartctl::stats::DeviceStats, _) = invoker
                .call(&log, ["--all", &dev])
                .expect(format!("could not parse simple/ stats for {}", dev).as_str());

            dbg!(dev, stats.extra_fields.keys());
            assert_eq!(stats.extra_fields.len(), 1, "additional fields found"); // one intentionally unused field
        }
    }
}
