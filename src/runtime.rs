use crate::{config::TelemetryConfig, metrics::Snapshot};
use anyhow::{ensure, Context, Result};
use nvml_wrapper::{
    enum_wrappers::device::{Clock, TemperatureSensor},
    error::NvmlError,
    Nvml,
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub fn meminfo_metrics(text: &str, at: Instant, out: &mut Snapshot) {
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let (key, rest) = match line.split_once(':') {
            Some(v) => v,
            None => continue,
        };
        let value = rest
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<f64>().ok());
        match key {
            "MemTotal" => total = value,
            "MemAvailable" => available = value,
            _ => {}
        }
    }
    if let (Some(t), Some(a)) = (total, available) {
        if t > 0.0 && a <= t {
            out.set("ram.util_pct", (100.0 * (t - a) / t).clamp(0.0, 100.0), at);
        }
    }
}

#[derive(Default)]
pub struct CpuSampler {
    previous: Option<[u64; 8]>,
}
impl CpuSampler {
    pub fn parse_line(line: &str) -> Option<[u64; 8]> {
        let mut words = line.split_whitespace();
        if words.next()? != "cpu" {
            return None;
        }
        let v: Vec<u64> = words
            .take(8)
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()
            .ok()?;
        (v.len() == 8).then(|| v.try_into().unwrap())
    }
    pub fn collect_text(&mut self, text: &str, at: Instant, out: &mut Snapshot) {
        let Some(current) = text.lines().find_map(Self::parse_line) else {
            self.previous = None;
            return;
        };
        if let Some(previous) = self.previous {
            if current.iter().zip(previous).any(|(n, o)| n < &o) {
                self.previous = Some(current);
                return;
            }
            let delta: [u64; 8] = std::array::from_fn(|i| current[i] - previous[i]);
            let total: u64 = delta.iter().sum(); // first eight only: guest counters are excluded
            let idle = delta[3] + delta[4];
            if total > 0 {
                out.set(
                    "cpu.util_pct",
                    100.0 * (total - idle) as f64 / total as f64,
                    at,
                );
            }
        }
        self.previous = Some(current);
    }
    pub fn collect(&mut self, at: Instant, out: &mut Snapshot) {
        match fs::read_to_string("/proc/stat") {
            Ok(text) => self.collect_text(&text, at, out),
            Err(_) => self.previous = None,
        }
    }
}

/// Deterministically locate the exact configured hwmon name+label pair.
pub fn hwmon_temperature(root: &Path, name: &str, label: &str) -> Result<Option<f64>> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .context("reading hwmon root")?
        .flatten()
        .map(|e| e.path())
        .collect();
    dirs.sort();
    for dir in dirs {
        let sensor_name = fs::read_to_string(dir.join("name"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        if sensor_name != name {
            continue;
        }
        let mut inputs: Vec<PathBuf> = fs::read_dir(&dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with("temp") && n.ends_with("_input"))
            })
            .collect();
        inputs.sort();
        for input in inputs {
            let stem = input
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .trim_end_matches("_input");
            let label_path = dir.join(format!("{stem}_label"));
            if fs::read_to_string(label_path).unwrap_or_default().trim() != label {
                continue;
            }
            let raw = fs::read_to_string(input)?.trim().parse::<f64>()?;
            return Ok(Some(raw / 1000.0));
        }
    }
    Ok(None)
}

/// hwmon power*_input values are reported in microwatts by the hwmon ABI.
pub fn hwmon_power_watts(root: &Path, name: &str, label: &str) -> Result<Option<f64>> {
    let mut dirs: Vec<_> = fs::read_dir(root)?.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs {
        if fs::read_to_string(dir.join("name"))
            .unwrap_or_default()
            .trim()
            != name
        {
            continue;
        }
        let mut inputs: Vec<_> = fs::read_dir(&dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("power") && n.ends_with("_input"))
            })
            .collect();
        inputs.sort();
        for input in inputs {
            let stem = input
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .trim_end_matches("_input");
            if fs::read_to_string(dir.join(format!("{stem}_label")))
                .unwrap_or_default()
                .trim()
                != label
            {
                continue;
            }
            let microwatts = fs::read_to_string(input)?.trim().parse::<f64>()?;
            return Ok(Some(microwatts / 1_000_000.0));
        }
    }
    Ok(None)
}

/// Arithmetic mean of readable `scaling_cur_freq` policy values, in MHz.
pub fn cpu_frequency_mhz(root: &Path) -> Option<f64> {
    let mut policies: Vec<_> = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("policy"))
        })
        .collect();
    policies.sort();
    let values: Vec<f64> = policies
        .iter()
        .filter_map(|p| fs::read_to_string(p.join("scaling_cur_freq")).ok())
        .filter_map(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .map(|khz| khz / 1000.0)
        .collect();
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

pub fn collect_linux(snapshot: &mut Snapshot, cfg: &TelemetryConfig, cpu: &mut CpuSampler) {
    let now = Instant::now();
    if let Ok(mem) = fs::read_to_string("/proc/meminfo") {
        meminfo_metrics(&mem, now, snapshot);
    }
    cpu.collect(now, snapshot);
    if let Some(mhz) = cpu_frequency_mhz(Path::new("/sys/devices/system/cpu/cpufreq")) {
        snapshot.set("cpu.clock_mhz", mhz, now);
    }
    if let (Some(name), Some(label)) = (&cfg.hwmon_cpu_name, &cfg.hwmon_cpu_label) {
        if let Ok(Some(t)) = hwmon_temperature(Path::new("/sys/class/hwmon"), name, label) {
            snapshot.set("cpu.temp_c", t, now);
        }
    }
    if let (Some(name), Some(label)) = (&cfg.hwmon_cpu_power_name, &cfg.hwmon_cpu_power_label) {
        if let Ok(Some(power)) = hwmon_power_watts(Path::new("/sys/class/hwmon"), name, label) {
            snapshot.set("cpu.power_w", power, now);
        }
    }
}

pub struct NvmlCollector {
    nvml: Nvml,
    uuid: String,
}

#[derive(Debug, Default)]
pub struct NvmlPoll {
    pub metric_errors: Vec<String>,
    pub session_error: Option<String>,
}
impl NvmlPoll {
    pub fn needs_reinitialization(&self) -> bool {
        self.session_error.is_some()
    }
}

fn is_fatal_nvml_session_error(error: &NvmlError) -> bool {
    matches!(
        error,
        NvmlError::GpuLost
            | NvmlError::ResetRequired
            | NvmlError::DriverNotLoaded
            | NvmlError::Uninitialized
            | NvmlError::NotFound
            | NvmlError::LibRmVersionMismatch
    )
}

fn legacy_round_watts(milliwatts: u32) -> u32 {
    (milliwatts as f64 / 1000.0).round_ties_even() as u32
}

impl NvmlCollector {
    pub fn new(wanted: Option<&str>) -> Result<Self> {
        let nvml = Nvml::init().context("loading and initializing NVML")?;
        let count = nvml.device_count().context("enumerating NVIDIA GPUs")?;
        ensure!(count > 0, "NVML reports no GPUs");
        let uuid = if let Some(uuid) = wanted {
            nvml.device_by_uuid(uuid)
                .with_context(|| format!("configured GPU UUID {uuid} not found"))?
                .uuid()?
        } else {
            ensure!(
                count == 1,
                "NVML found {count} GPUs; set telemetry.gpu_uuid to select one"
            );
            nvml.device_by_index(0)?.uuid()?
        };
        Ok(Self { nvml, uuid })
    }
    pub fn uuid(&self) -> &str {
        &self.uuid
    }
    /// Unsupported metrics are isolated; device/session failures request reinitialization.
    pub fn collect(&self, out: &mut Snapshot, at: Instant) -> NvmlPoll {
        let mut poll = NvmlPoll::default();
        let device = match self.nvml.device_by_uuid(self.uuid.clone()) {
            Ok(d) => d,
            Err(e) => {
                poll.session_error = Some(format!("GPU {} unavailable: {e}", self.uuid));
                return poll;
            }
        };
        macro_rules! metric {
            ($method:expr, $key:literal, $scale:expr) => {
                if poll.session_error.is_none() {
                    match $method {
                        Ok(v) => out.set($key, v as f64 / $scale, at),
                        Err(e) if is_fatal_nvml_session_error(&e) => {
                            poll.session_error = Some(format!("{}: {e}", $key));
                        }
                        Err(e) => poll.metric_errors.push(format!("{}: {e}", $key)),
                    }
                }
            };
        }
        metric!(
            device.temperature(TemperatureSensor::Gpu),
            "gpu.temp_c",
            1.0
        );
        metric!(
            device.utilization_rates().map(|u| u.gpu),
            "gpu.util_pct",
            1.0
        );
        metric!(
            device.power_usage().map(legacy_round_watts),
            "gpu.power_w",
            1.0
        );
        metric!(device.clock_info(Clock::Graphics), "gpu.clock_mhz", 1.0);
        poll
    }
}

pub fn lock_file(identity: &str) -> Result<fs::File> {
    let dir = if let Some(path) = std::env::var_os("COOLING_DISPLAY_LOCK_DIR") {
        PathBuf::from(path)
    } else if let Some(path) = std::env::var_os("XDG_RUNTIME_DIR") {
        let base = PathBuf::from(path);
        let md = fs::symlink_metadata(&base).context("checking XDG_RUNTIME_DIR")?;
        validate_private_metadata(&md, true)
            .context("XDG_RUNTIME_DIR must be a real, user-owned private directory")?;
        base.join("cooling-display")
    } else {
        PathBuf::from("/tmp").join(format!("cooling-display-{}", effective_uid()))
    };
    lock_file_in(&dir, identity)
}

fn lock_file_in(dir: &Path, identity: &str) -> Result<fs::File> {
    use fs2::FileExt;
    use std::fs::OpenOptions;
    ensure_private_directory(dir)?;
    let path = dir.join(format!("{:016x}.lock", stable_lock_hash(identity)));
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    ensure_private_lock_file(&file)?;
    file.try_lock_exclusive()
        .context("another cooling-display sender holds this physical-device lock")?;
    Ok(file)
}

fn stable_lock_hash(identity: &str) -> u64 {
    identity.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn ensure_private_directory(dir: &Path) -> Result<()> {
    anyhow::ensure!(dir.is_absolute(), "lock directory must be absolute");
    #[cfg(unix)]
    {
        use std::path::Component;
        let mut prefix = PathBuf::new();
        for component in dir.components() {
            match component {
                Component::RootDir => prefix.push("/"),
                Component::Normal(part) => prefix.push(part),
                _ => anyhow::bail!("lock directory must not contain . or .. components"),
            }
            match fs::symlink_metadata(&prefix) {
                Ok(md) => {
                    anyhow::ensure!(
                        !md.file_type().is_symlink(),
                        "symlink component in lock directory"
                    );
                    anyhow::ensure!(md.is_dir(), "non-directory component in lock directory");
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && prefix == dir => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    match fs::symlink_metadata(dir) {
        Ok(md) => validate_private_metadata(&md, true)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                match builder.create(dir) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.into()),
                }
            }
            #[cfg(not(unix))]
            fs::create_dir(dir)?;
            let md = fs::symlink_metadata(dir)?;
            validate_private_metadata(&md, true)?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_metadata(md: &fs::Metadata, directory: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    anyhow::ensure!(
        if directory { md.is_dir() } else { md.is_file() },
        "lock path has wrong file type"
    );
    anyhow::ensure!(
        md.uid() == unsafe { libc::geteuid() },
        "lock path is not owned by this user"
    );
    anyhow::ensure!(
        md.mode() & 0o077 == 0,
        "lock path must have private permissions (mode {:o})",
        md.mode() & 0o777
    );
    Ok(())
}
#[cfg(not(unix))]
fn validate_private_metadata(md: &fs::Metadata, directory: bool) -> Result<()> {
    anyhow::ensure!(
        if directory { md.is_dir() } else { md.is_file() },
        "lock path has wrong file type"
    );
    Ok(())
}
fn ensure_private_lock_file(file: &fs::File) -> Result<()> {
    let md = file.metadata()?;
    validate_private_metadata(&md, false)
}
#[cfg(unix)]
fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}
#[cfg(not(unix))]
fn effective_uid() -> u32 {
    0
}

pub fn interruptible_wait(stop: &std::sync::atomic::AtomicBool, duration: Duration) {
    let end = Instant::now() + duration;
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_ignores_guest_fields_and_handles_counter_reset() {
        let mut c = CpuSampler::default();
        let mut s = Snapshot::default();
        let now = Instant::now();
        c.collect_text("cpu 100 10 20 60 10 0 0 0 500 400\n", now, &mut s);
        c.collect_text("cpu 110 10 30 70 10 0 0 0 900 800\n", now, &mut s);
        assert!((s.value("cpu.util_pct").unwrap() - 66.6667).abs() < 0.01);
        let mut reset = Snapshot::default();
        c.collect_text("cpu 1 1 1 1 1 1 1 1\n", now, &mut reset);
        assert_eq!(reset.value("cpu.util_pct"), None);
    }
    #[test]
    fn meminfo_requires_both_values_and_does_not_invent_usage() {
        let now = Instant::now();
        let mut s = Snapshot::default();
        meminfo_metrics("MemTotal: 100 kB\n", now, &mut s);
        assert_eq!(s.value("ram.util_pct"), None);
        meminfo_metrics("MemTotal: 100 kB\nMemAvailable: 25 kB\n", now, &mut s);
        assert_eq!(s.value("ram.util_pct"), Some(75.0));
    }
    #[test]
    fn hwmon_selection_is_name_and_label_deterministic() {
        let root = tempfile::tempdir().unwrap();
        let d = root.path().join("hwmon7");
        fs::create_dir(&d).unwrap();
        fs::write(d.join("name"), "coretemp\n").unwrap();
        fs::write(d.join("temp1_label"), "Package id 0\n").unwrap();
        fs::write(d.join("temp1_input"), "42000\n").unwrap();
        assert_eq!(
            hwmon_temperature(root.path(), "coretemp", "Package id 0").unwrap(),
            Some(42.0)
        );
        assert_eq!(
            hwmon_temperature(root.path(), "coretemp", "Core 0").unwrap(),
            None
        );
    }
    #[test]
    fn configured_hwmon_cpu_power_uses_exact_name_label_and_microwatts() {
        let root = tempfile::tempdir().unwrap();
        let d = root.path().join("hwmon2");
        fs::create_dir(&d).unwrap();
        fs::write(d.join("name"), "cpu_telemetry\n").unwrap();
        fs::write(d.join("power1_label"), "CPU package\n").unwrap();
        fs::write(d.join("power1_input"), "87500000\n").unwrap();
        assert_eq!(
            hwmon_power_watts(root.path(), "cpu_telemetry", "CPU package").unwrap(),
            Some(87.5)
        );
        assert_eq!(
            hwmon_power_watts(root.path(), "cpu_telemetry", "DRAM").unwrap(),
            None
        );
    }
    #[test]
    fn cpufreq_policy_aggregate_is_deterministic_and_skips_missing_values() {
        let root = tempfile::tempdir().unwrap();
        for (name, value) in [("policy10", "3000000"), ("policy2", "2000000")] {
            let dir = root.path().join(name);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("scaling_cur_freq"), value).unwrap();
        }
        fs::create_dir(root.path().join("not-a-policy")).unwrap();
        assert_eq!(cpu_frequency_mhz(root.path()), Some(2500.0));
    }
    #[test]
    fn advisory_lock_releases_even_though_lock_file_persists() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("locks");
        let id = "physical-usb-sysfs-id";
        let first = lock_file_in(&dir, id).unwrap();
        let clone = first.try_clone().unwrap();
        assert!(lock_file_in(&dir, id).is_err());
        drop(first);
        assert!(lock_file_in(&dir, id).is_err());
        drop(clone);
        let second = lock_file_in(&dir, id).unwrap();
        drop(second);
    }
    #[test]
    fn per_device_locks_do_not_serialize_distinct_devices() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("private-locks");
        let a = lock_file_in(&dir, "sysfs-usb1-1-2").unwrap();
        let b = lock_file_in(&dir, "sysfs-usb1-1-3").unwrap();
        drop((a, b));
    }
    #[cfg(unix)]
    #[test]
    fn lock_directory_symlink_is_rejected_without_chmod_target() {
        use std::os::unix::fs::{symlink, MetadataExt};
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        let before = fs::metadata(&target).unwrap().mode() & 0o777;
        let link = root.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(lock_file_in(&link, "device").is_err());
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, before);
    }
    #[test]
    fn watts_round_like_legacy_python_round() {
        assert_eq!(legacy_round_watts(237_500), 238);
        assert_eq!(legacy_round_watts(238_500), 238);
    }
    #[test]
    fn unsupported_metric_does_not_classify_as_lost_nvml_session() {
        assert!(!is_fatal_nvml_session_error(&NvmlError::NotSupported));
        assert!(!is_fatal_nvml_session_error(&NvmlError::FunctionNotFound));
        assert!(is_fatal_nvml_session_error(&NvmlError::GpuLost));
        assert!(is_fatal_nvml_session_error(&NvmlError::NotFound));
        assert!(!NvmlPoll {
            metric_errors: vec!["gpu.power_w: not supported".into()],
            session_error: None,
        }
        .needs_reinitialization());
        assert!(NvmlPoll {
            metric_errors: vec![],
            session_error: Some("GPU lost".into()),
        }
        .needs_reinitialization());
    }
}
