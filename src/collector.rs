use std::path::PathBuf;
use std::time::{Instant, Duration};
use std::sync::atomic::Ordering;

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use prometheus_client::registry::Registry;
use prometheus_client::encoding::{EncodeLabelSet, text::encode};
use prometheus_client::metrics::info::Info;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::family::Family;
use slog::{error, info, debug, o};

use crate::smartctl;
use crate::smartctl::stats::DeviceStats;

pub type WorkerChannel = mpsc::Sender<oneshot::Sender<Result<String, String>>>;
pub type CollectResult = Result<String, String>;

#[derive(Debug)]
pub struct Collector<I>
where
    I: smartctl::SmartctlInvoker + std::fmt::Debug + std::marker::Send
{
    invoker: I,
    registry: Registry,
    last_read_version: Option<smartctl::Version>,
    device_rescan_interval: Duration,
    last_device_scan: Option<Instant>,
    devices: Vec<Device>,
    metrics: Metrics,
}

#[derive(Debug, Default)]
pub struct Metrics {
    device_up: Family::<DeviceUpLabels, Gauge>,
    power_on_hours: Family::<DeviceUnqiueLabels, Counter>,
    power_cycle_count: Family::<DeviceUnqiueLabels, Counter>,
    temperature_celsius: Family::<DeviceUnqiueLabels, Gauge>,
    bytes_total: Family::<DeviceUnqiueLabels, Gauge>,
    blocks_total: Family::<DeviceUnqiueLabels, Gauge>,
    rotations_per_minute: Family::<DeviceUnqiueLabels, Gauge>,
    logical_block_size: Family::<DeviceUnqiueLabels, Gauge>,
    physical_block_size: Family::<DeviceUnqiueLabels, Gauge>,
    interface_bytes_per_unit: Family::<InterfaceSpeedLabels, Gauge>,
    interface_units_per_second: Family::<InterfaceSpeedLabels, Gauge>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct DeviceUpLabels {
    path: String,
    protocol: smartctl::Protocol,
    model_name: String,
    model_family: String,
    serial_number: String,
    wwn_naa: u8,
    wwn_oui: u32,
    wwn_id: u64,
    firmware_version: String,
    in_smartctl_database: &'static str,
    ata_version: String,
    sata_version: String,
    trim: &'static str,
    smart_available: &'static str,
    smart_enabled: &'static str,
}

impl From<DeviceStats> for DeviceUpLabels {
    fn from(value: DeviceStats) -> Self {
        Self { 
            path: String::from(value.device.name.to_string_lossy()),
            protocol: value.device.protocol,
            model_name: value.model_name.clone(),
            model_family: value.model_family.clone(),
            serial_number: value.serial_number.clone(),
            wwn_naa: value.wwn.network_address_authority,
            wwn_oui: value.wwn.organizationally_unique_identifier,
            wwn_id: value.wwn.id,
            firmware_version: value.firmware_version.clone(),
            in_smartctl_database: if value.smart_support.enabled { "yes" } else { "no" },
            ata_version: value.ata_version.string.clone(),
            sata_version: value.sata_version.string.clone(),
            trim: if value.trim.supported { "yes" } else { "no" },
            smart_available: if value.smart_support.available { "yes" } else { "no" },
            smart_enabled: if value.smart_support.enabled { "yes" } else { "no" },
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct DeviceUnqiueLabels {
    path: String,
}

impl From<DeviceStats> for DeviceUnqiueLabels {
    fn from(value: DeviceStats) -> Self {
        Self {
            path: String::from(value.device.name.to_string_lossy()),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct InterfaceSpeedLabels {
    path: String,
    interface: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct InfoLabels {
    version: String,
}

impl Default for InfoLabels {
    fn default() -> Self {
        Self{
            version: env!("CARGO_PKG_VERSION").to_owned(), 
        }
    }
}

#[derive(Debug)]
pub struct Device {
    name: PathBuf,
}

impl From<smartctl::scan::Device> for Device {
    fn from(value: smartctl::scan::Device) -> Self {
        Self { name: value.name }
    }
}

impl<I> Collector<I>
where
    I: smartctl::SmartctlInvoker + std::fmt::Debug + std::marker::Send + 'static
{
    pub fn with(invoker: I) -> Self {
        let metrics = Metrics::default();
        let mut registry = Registry::default();

        registry.register(
            "smartctl_exporter_info",
            "",
            Info::<InfoLabels>::new(InfoLabels::default()),
        );
        registry.register(
            "smart_device_up",
            "Information and availability of device",
            metrics.device_up.clone(),
        );

        registry.register(
            "smart_device_power_on_hours",
            "",
            metrics.power_on_hours.clone(),
        );
        registry.register(
            "smart_device_power_cycle_count",
            "",
            metrics.power_cycle_count.clone(),
        );
        registry.register(
            "smart_device_temperature_celsius",
            "",
            metrics.temperature_celsius.clone(),
        );
        registry.register(
            "smart_device_bytes_total",
            "",
            metrics.bytes_total.clone(),
        );
        registry.register(
            "smart_device_blocks_total",
            "",
            metrics.blocks_total.clone(),
        );
        registry.register(
            "smart_device_rotations_per_minute",
            "",
            metrics.rotations_per_minute.clone(),
        );
        registry.register(
            "smart_device_logical_block_size",
            "",
            metrics.logical_block_size.clone(),
        );
        registry.register(
            "smart_device_physical_block_size",
            "",
            metrics.physical_block_size.clone(),
        );
        registry.register(
            "smart_device_interface_bytes_per_unit",
            "",
            metrics.interface_bytes_per_unit.clone(),
        );
        registry.register(
            "smart_device_interface_units_per_second",
            "",
            metrics.interface_units_per_second.clone(),
        );

        Self{
            invoker,
            registry,
            last_read_version: None,
            device_rescan_interval: Duration::from_secs(30),
            last_device_scan: None,
            devices: vec![],
            metrics,
        }
    }

    /// Collects all metrics from the system using smartclt and encodes it
    /// into Prometheus text format
    pub async fn collect(&mut self, log: &slog::Logger) -> CollectResult {
        let now = Instant::now();
        if self.last_device_scan.is_none() || (now - self.last_device_scan.unwrap() > self.device_rescan_interval) {
            self.last_device_scan = Some(now);
            info!(log, "re-scanning devices");

            let (scan, last_read_version): (smartctl::scan::Scan, _) =
                self.invoker.call(log, ["--scan-open"])
                    .map_err(|e| format!("failed to run smartctl: {:?}", e))?;

            self.devices = scan.devices.into_iter().map(Into::into).collect();
            self.last_read_version = Some(last_read_version);
        }

        // reset all known disks
        self.metrics.device_up.clear();
        self.metrics.power_on_hours.clear();
        self.metrics.power_cycle_count.clear();
        self.metrics.temperature_celsius.clear();
        self.metrics.bytes_total.clear();
        self.metrics.blocks_total.clear();
        self.metrics.rotations_per_minute.clear();
        self.metrics.logical_block_size.clear();
        self.metrics.physical_block_size.clear();
        self.metrics.interface_bytes_per_unit.clear();
        self.metrics.interface_units_per_second.clear();

        for d in &self.devices {
            let dev_path = d.name.to_string_lossy();
            info!(log, "collecting stats for device"; "device" => dev_path.as_ref());

            let (stats, _): (smartctl::stats::DeviceStats, _) =
                self.invoker.call(log, ["--all", dev_path.as_ref()])
                .map_err(|e| format!("failed to collect device stats: {:?}", e))?;

            let labels = stats.clone().into();

            self.metrics.device_up.get_or_create(&stats.clone().into()).set(1);

            self.metrics.power_on_hours.get_or_create(&labels).inner().store(stats.power_on_time.hours, Ordering::Relaxed);
            self.metrics.power_cycle_count.get_or_create(&labels).inner().store(stats.power_cycle_count, Ordering::Relaxed);
            self.metrics.temperature_celsius.get_or_create(&labels).set(stats.temperature.current);
            self.metrics.bytes_total.get_or_create(&labels).set(stats.user_capacity.bytes.try_into().expect("too many bytes capacity (failed to convert u64 to i64)"));
            self.metrics.blocks_total.get_or_create(&labels).set(stats.user_capacity.blocks.try_into().expect("too many blocks capacity (failed to convert u64 to i64)"));
            self.metrics.rotations_per_minute.get_or_create(&labels).set(stats.rotation_rate.into());
            self.metrics.logical_block_size.get_or_create(&labels).set(stats.logical_block_size.into());
            self.metrics.physical_block_size.get_or_create(&labels).set(stats.physical_block_size.into());

            self.metrics.interface_bytes_per_unit.get_or_create(
                &InterfaceSpeedLabels { path: dev_path.to_string(), interface: "device" }
            ).set((stats.interface_speed.current.bits_per_unit / 8).into());
            self.metrics.interface_units_per_second.get_or_create(
                &InterfaceSpeedLabels { path: dev_path.to_string(), interface: "device" }
            ).set((stats.interface_speed.current.units_per_second).into());

            self.metrics.interface_bytes_per_unit.get_or_create(
                &InterfaceSpeedLabels { path: dev_path.to_string(), interface: "interface" }
            ).set((stats.interface_speed.max.bits_per_unit / 8).into());
            self.metrics.interface_units_per_second.get_or_create(
                &InterfaceSpeedLabels { path: dev_path.to_string(), interface: "interface" }
            ).set((stats.interface_speed.max.units_per_second).into());
        }

        let mut buffer = String::new();
        encode(&mut buffer, &self.registry)
            .map_err(|e| format!("failed to encode prometheus exposition: {:?}", e))?;
        Ok(buffer)
    }

    /// Consumes `self` and starts a [tokio worker task](tokio::spawn). The
    /// returned Client can communicate with the worker task over [`mpsc`]
    /// channels and request an encoded Prometheus exposition. This function
    /// also returns the tasks [`JoinHandle`] you should await.
    pub fn start_worker(mut self, log: &slog::Logger) -> (Client, JoinHandle<()>) {
        let log = log.new(o!("worker" => "collector"));

        let (tx, mut rx) = mpsc::channel::<oneshot::Sender<CollectResult>>(1);
        let handle = tokio::spawn(async move {
            while let Some(return_channel) = rx.recv().await {
                debug!(log, "got collect request in worker task");
                debug!(log, "{:?}", self);
                let collect_result = self.collect(&log).await;
                if let Err(e) = return_channel.send(collect_result) {
                    error!(log, "failed to send response to client"; "error" => format!("{:?}", e));
                }
            }
        });

        (Client{channel: tx}, handle)
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    channel: WorkerChannel,
}

/// Communicates with a [`Collector`] in a [tokio worker task](tokio::spawn)
/// via [`mpsc`] channels
impl Client {
    /// Calls [`collect()`](Collector::collect()) on the [`Collector`] in
    /// the connected task
    pub async fn collect(&self) -> CollectResult {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.channel
            .clone()
            .send(resp_tx)
            .await
            .map_err(|e| format!("failed to communicate with collector worker (tx): {:?}", e))?;

        resp_rx
            .await
            .map_err(|e| format!("failed to communicate with collector worker (rx): {:?}", e))?
    }
}

mod missing_atomics {
    /// prometheus_client doesn't implement it's Atomic trait for a couple pairings that we are
    /// using
    use std::sync::atomic::{AtomicU64 as OtherAtomicU64, AtomicU16 as OtherAtomicU16, Ordering};
    use prometheus_client::metrics::gauge::Atomic;

    #[derive(Default, Debug)]
    pub struct AtomicU16(OtherAtomicU16);
    impl Atomic<u16> for AtomicU16 {
        fn inc(&self) -> u16 {
            self.inc_by(1)
        }

        fn inc_by(&self, v: u16) -> u16 {
            self.0.fetch_add(v, Ordering::Relaxed)
        }

        fn dec(&self) -> u16 {
            self.dec_by(1)
        }

        fn dec_by(&self, v: u16) -> u16 {
            self.0.fetch_sub(v, Ordering::Relaxed)
        }

        fn set(&self, v: u16) -> u16 {
            self.0.swap(v, Ordering::Relaxed)
        }

        fn get(&self) -> u16 {
            self.0.load(Ordering::Relaxed)
        }
    }

    #[derive(Default, Debug)]
    pub struct AtomicU64(OtherAtomicU64);
    impl Atomic<u64> for AtomicU64 {
        fn inc(&self) -> u64 {
            self.inc_by(1)
        }

        fn inc_by(&self, v: u64) -> u64 {
            self.0.fetch_add(v, Ordering::Relaxed)
        }

        fn dec(&self) -> u64 {
            self.dec_by(1)
        }

        fn dec_by(&self, v: u64) -> u64 {
            self.0.fetch_sub(v, Ordering::Relaxed)
        }

        fn set(&self, v: u64) -> u64 {
            self.0.swap(v, Ordering::Relaxed)
        }

        fn get(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }
}
