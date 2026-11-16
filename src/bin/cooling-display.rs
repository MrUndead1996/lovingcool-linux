use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use cooling_display::{
    calendar, config::Config, mapping, metrics::Snapshot, protocol, runtime, transport,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(about = "Lovingcool AK-03 telemetry bridge")]
struct Args {
    #[arg(short, long, default_value = "/etc/cooling-display.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Read-only enumeration and descriptor inspection.
    Probe {
        #[arg(long)]
        experimental_pid2017: bool,
    },
    /// Finite synthetic frame test; real USB output requires --send.
    Test {
        #[arg(long)]
        send: bool,
        #[arg(long, default_value_t = 3)]
        duration_seconds: u64,
        #[arg(long, default_value_t = 42)]
        cpu_temp: u8,
        #[arg(long, default_value_t = 50)]
        cpu_util: u8,
        #[arg(long, default_value_t = 125.0)]
        cpu_power: f64,
        #[arg(long, default_value_t = 1200.0)]
        cpu_clock: f64,
        #[arg(long, default_value_t = 55)]
        gpu_temp: u8,
        #[arg(long, default_value_t = 60)]
        gpu_util: u8,
        #[arg(long, default_value_t = 300.0)]
        gpu_power: f64,
        #[arg(long, default_value_t = 1800.0)]
        gpu_clock: f64,
    },
    Run {
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = if args.config == std::path::Path::new("/etc/cooling-display.toml")
        && !args.config.exists()
    {
        Config::default()
    } else {
        Config::load(&args.config)?
    };
    match args.command {
        Command::Probe {
            experimental_pid2017,
        } => probe(&cfg, experimental_pid2017),
        Command::Test {
            send,
            duration_seconds,
            cpu_temp,
            cpu_util,
            cpu_power,
            cpu_clock,
            gpu_temp,
            gpu_util,
            gpu_power,
            gpu_clock,
        } => test_frame(
            &cfg,
            send,
            duration_seconds,
            TestValues {
                cpu_temp,
                cpu_util,
                cpu_power,
                cpu_clock,
                gpu_temp,
                gpu_util,
                gpu_power,
                gpu_clock,
            },
        ),
        Command::Run { dry_run } => run(cfg, dry_run),
    }
}

fn probe(cfg: &Config, experimental: bool) -> Result<()> {
    let pid = if experimental { 0x2017 } else { cfg.product_id };
    if experimental {
        eprintln!("PID 0x2017 is probe-only: usage-page-specific protocol is not implemented");
    }
    let devices = transport::devices(cfg.vendor_id, pid)?;
    if devices.is_empty() {
        println!("No {:04x}:{pid:04x} device found", cfg.vendor_id);
    }
    for d in devices {
        println!(
            "{:04x}:{:04x} bus={} address={} serial={:?} sysfs={}",
            d.vendor_id(),
            d.product_id(),
            d.busnum(),
            d.device_address(),
            d.serial_number(),
            d.sysfs_path().display()
        );
        if pid == 0x200b {
            transport::inspect(&d, cfg.interface, cfg.endpoint_out)?;
            println!("  active configuration/default alternate: interface {}, interrupt OUT {:#04x}, 64 bytes",
                cfg.interface, cfg.endpoint_out);
        } else {
            println!("  no protocol inference or endpoint validation implies send support");
        }
    }
    probe_hwmon();
    match nvml_wrapper::Nvml::init() {
        Ok(nvml) => {
            let count = nvml.device_count().unwrap_or(0);
            for i in 0..count {
                if let Ok(gpu) = nvml.device_by_index(i) {
                    let uuid = gpu.uuid().unwrap_or_else(|_| "<unavailable>".into());
                    let selected = cfg.telemetry.gpu_uuid.as_deref() == Some(uuid.as_str());
                    println!(
                        "GPU UUID: {uuid}{}",
                        if selected { " (configured)" } else { "" }
                    );
                }
            }
        }
        Err(e) => eprintln!("NVML probe unavailable: {e}"),
    }
    Ok(())
}

fn probe_hwmon() {
    let root = std::path::Path::new("/sys/class/hwmon");
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
        println!("hwmon name={} path={}", name.trim(), dir.display());
        let Ok(files) = std::fs::read_dir(dir) else {
            continue;
        };
        for f in files.flatten() {
            let file = f.path();
            let Some(file_name) = file.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if file_name.starts_with("temp") && file_name.ends_with("_input") {
                let stem = file_name.trim_end_matches("_input");
                let label = std::fs::read_to_string(file.with_file_name(format!("{stem}_label")))
                    .unwrap_or_default();
                println!("  {file_name} label={}", label.trim());
            }
            if file_name.starts_with("power") && file_name.ends_with("_input") {
                let stem = file_name.trim_end_matches("_input");
                let label = std::fs::read_to_string(file.with_file_name(format!("{stem}_label")))
                    .unwrap_or_default();
                println!("  {file_name} label={} (µW)", label.trim());
            }
        }
    }
    probe_cpufreq();
}

fn probe_cpufreq() {
    let root = std::path::Path::new("/sys/devices/system/cpu/cpufreq");
    let Ok(entries) = std::fs::read_dir(root) else {
        println!("CPU frequency source unavailable: {}", root.display());
        return;
    };
    let mut policies: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    policies.sort();
    for policy in policies {
        let name = policy
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("policy?");
        let value = std::fs::read_to_string(policy.join("scaling_cur_freq"))
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .map(|khz| format!("{:.1} MHz", khz / 1000.0))
            .unwrap_or_else(|| "unavailable".into());
        println!("CPU frequency {name}: {value}");
    }
}

struct TestValues {
    cpu_temp: u8,
    cpu_util: u8,
    cpu_power: f64,
    cpu_clock: f64,
    gpu_temp: u8,
    gpu_util: u8,
    gpu_power: f64,
    gpu_clock: f64,
}
fn synthetic(cfg: &Config, v: &TestValues) -> ([u8; 64], Snapshot) {
    let now = Instant::now();
    let mut s = Snapshot::default();
    for (key, value) in [
        ("cpu.temp_c", v.cpu_temp as f64),
        ("cpu.util_pct", v.cpu_util as f64),
        ("cpu.power_w", v.cpu_power),
        ("cpu.clock_mhz", v.cpu_clock),
        ("gpu.temp_c", v.gpu_temp as f64),
        ("gpu.util_pct", v.gpu_util as f64),
        ("gpu.power_w", v.gpu_power),
        ("gpu.clock_mhz", v.gpu_clock),
    ] {
        s.set(key, value, now);
    }
    if cfg.telemetry.cpu_to_display {
        s.mirror_prefix("gpu.", "cpu.");
    }
    let (cpu, gpu, ch, gh) = mapping::groups(
        &s,
        &cfg.mapping,
        now,
        Duration::from_secs(1),
        &cfg.cpu_power_encoding,
    );
    (
        protocol::build_frame(cpu, gpu, ch, gh, calendar::now_local()),
        s,
    )
}

fn test_frame(cfg: &Config, send: bool, seconds: u64, values: TestValues) -> Result<()> {
    anyhow::ensure!(
        seconds > 0 && seconds <= 300,
        "duration_seconds must be in 1..=300"
    );
    let (frame, _) = synthetic(cfg, &values);
    println!(
        "synthetic frame: {}",
        frame
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    if !send {
        println!("dry test only; pass --send for timed USB output");
        return Ok(());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    ctrlc::set_handler(move || s.store(true, Ordering::SeqCst))?;
    let info = select_device(cfg)?.context("no selected AK-03 device connected")?;
    let physical_id = transport::physical_identity(info.sysfs_path())?;
    let _lock = runtime::lock_file(&physical_id)?;
    let mut session = transport::UsbSession::open(&info, cfg.interface, cfg.endpoint_out)?;
    if cfg.cpu_power_encoding == "decimal" {
        eprintln!("WARNING: experimental CPU decimal power encoding enabled; hardware behavior unverified");
    }
    let power_key = cpu_power_warning_key(cfg);
    let mut last_power_warning = None;
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end && !stop.load(Ordering::SeqCst) {
        let (frame, snapshot) = synthetic(cfg, &values);
        let power = power_key.as_ref().and_then(|key| snapshot.value(key));
        warn_cpu_power(
            power,
            power_key.as_deref().unwrap_or("unmapped CPU power"),
            cfg.cpu_power_encoding == "legacy",
            &mut last_power_warning,
        );
        session.send(&frame)?;
        runtime::interruptible_wait(&stop, Duration::from_millis(cfg.send_interval_ms));
    }
    Ok(())
}

fn select_device(cfg: &Config) -> Result<Option<nusb::DeviceInfo>> {
    let devices = transport::devices(cfg.vendor_id, cfg.product_id)?;
    transport::choose(
        &devices,
        cfg.serial_number.as_deref(),
        cfg.location_path.as_deref(),
    )
    .map(|d| d.cloned())
}
fn run(cfg: Config, dry: bool) -> Result<()> {
    cfg.validate()?;
    if cfg.cpu_power_encoding == "decimal" {
        eprintln!("WARNING: experimental CPU decimal power encoding enabled; hardware behavior unverified");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))?;

    let shared = Arc::new(RwLock::new(Snapshot::default()));
    let publish = shared.clone();
    let worker_stop = stop.clone();
    let telem = cfg.telemetry.clone();
    let poll = Duration::from_millis(cfg.poll_interval_ms);
    let gpu_uuid = telem.gpu_uuid.clone();
    let worker = thread::spawn(move || {
        let mut cpu = runtime::CpuSampler::default();
        let mut latest = Snapshot::default();
        let mut nvml: Option<runtime::NvmlCollector> = None;
        let mut last_init = Instant::now() - Duration::from_secs(60);
        let mut last_error = String::new();
        while !worker_stop.load(Ordering::SeqCst) {
            let now = Instant::now();
            let mut local = Snapshot::default();
            runtime::collect_linux(&mut local, &telem, &mut cpu);
            if nvml.is_none() && now.duration_since(last_init) >= Duration::from_secs(30) {
                last_init = now;
                match runtime::NvmlCollector::new(gpu_uuid.as_deref()) {
                    Ok(n) => {
                        eprintln!("NVML selected GPU {}", n.uuid());
                        nvml = Some(n);
                        last_error.clear();
                    }
                    Err(e) => {
                        let text = format!("{e:#}");
                        if text != last_error {
                            eprintln!("NVML unavailable: {text}");
                            last_error = text;
                        }
                    }
                }
            }
            let nvml_result = nvml.as_ref().map(|n| n.collect(&mut local, Instant::now()));
            if let Some(result) = nvml_result {
                if result.needs_reinitialization() {
                    let error = result
                        .session_error
                        .unwrap_or_else(|| "unknown NVML session failure".into());
                    let text =
                        format!("NVML session lost: {error}; retrying initialization in 30 s");
                    if text != last_error {
                        eprintln!("{text}");
                    }
                    last_error = text;
                    nvml.take();
                    last_init = Instant::now();
                } else {
                    let errors = result.metric_errors.join("; ");
                    if errors != last_error {
                        if !errors.is_empty() {
                            eprintln!("NVML metric issue: {errors}");
                        }
                        last_error = errors;
                    }
                }
            }
            if telem.cpu_to_display {
                local.mirror_prefix("gpu.", "cpu.");
            }
            latest.merge_latest(local);
            if let Ok(mut guard) = publish.write() {
                *guard = latest.clone();
            }
            runtime::interruptible_wait(&worker_stop, poll);
        }
    });

    let sender_stop = stop.clone();
    let sender_snapshot = shared.clone();
    let sender = thread::spawn(move || sender_loop(cfg, dry, sender_snapshot, sender_stop));
    let mut unexpected = None;
    while !stop.load(Ordering::SeqCst) {
        if sender.is_finished() {
            unexpected = Some("USB sender worker exited unexpectedly");
            stop.store(true, Ordering::SeqCst);
            break;
        }
        if worker.is_finished() {
            unexpected = Some("telemetry worker exited unexpectedly");
            stop.store(true, Ordering::SeqCst);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline && (!sender.is_finished() || !worker.is_finished()) {
        thread::sleep(Duration::from_millis(10));
    }
    if !sender.is_finished() {
        eprintln!("USB worker still stopping after 1500 ms; exiting without waiting on driver I/O");
    }
    if !worker.is_finished() {
        eprintln!("telemetry worker still stopping after 1500 ms; NVML calls cannot be cancelled");
    }
    let sender_result = if sender.is_finished() {
        Some(
            sender
                .join()
                .map_err(|_| anyhow::anyhow!("USB sender worker panicked"))?,
        )
    } else {
        drop(sender);
        None
    };
    let worker_result = if worker.is_finished() {
        Some(
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("telemetry worker panicked"))?,
        )
    } else {
        drop(worker);
        None
    };
    let _ = (sender_result, worker_result);
    if let Some(message) = unexpected {
        anyhow::bail!(message);
    }
    Ok(())
}

struct ActiveUsb {
    usb: transport::UsbSession,
    _lock: std::fs::File,
}
impl ActiveUsb {
    fn send(&mut self, frame: &[u8; 64]) -> Result<()> {
        self.usb.send(frame)
    }
}

fn sender_loop(cfg: Config, dry: bool, shared: Arc<RwLock<Snapshot>>, stop: Arc<AtomicBool>) {
    let mut session = None;
    let mut backoff = transport::Backoff::default();
    let mut state = String::new();
    let mut last_power_warning: Option<Instant> = None;
    let mut last_dry_print: Option<Instant> = None;
    while !stop.load(Ordering::SeqCst) {
        if !dry && session.is_none() {
            match select_device(&cfg) {
                Ok(Some(info)) => {
                    let connect = (|| -> Result<ActiveUsb> {
                        let physical = transport::physical_identity(info.sysfs_path())?;
                        let lock = runtime::lock_file(&physical)?;
                        let usb =
                            transport::UsbSession::open(&info, cfg.interface, cfg.endpoint_out)?;
                        Ok(ActiveUsb { usb, _lock: lock })
                    })();
                    match connect {
                        Ok(active) => {
                            session = Some(active);
                            backoff.reset();
                            state = "connected".into();
                            eprintln!("USB sender connected at {}", info.sysfs_path().display());
                        }
                        Err(e) => {
                            let next = if format!("{e:#}").contains("physical-device lock") {
                                "locked"
                            } else {
                                "disconnected"
                            };
                            transition(
                                &mut state,
                                next,
                                &format!("USB connect/lock failed: {e:#}"),
                            );
                            runtime::interruptible_wait(&stop, backoff.delay());
                            continue;
                        }
                    }
                }
                Ok(None) => {
                    if state != "waiting" {
                        transition(&mut state, "waiting", "AK-03 not found; retrying");
                    }
                    runtime::interruptible_wait(&stop, backoff.delay());
                    continue;
                }
                Err(e) => {
                    transition(
                        &mut state,
                        "enumeration-error",
                        &format!("USB enumeration failed: {e:#}"),
                    );
                    runtime::interruptible_wait(&stop, backoff.delay());
                    continue;
                }
            }
        }
        // Read and encode after a newly connected device is open/claimed; an
        // expensive open must not make the first frame stale.
        let snap = match shared.read() {
            Ok(s) => s.clone(),
            Err(_) => {
                eprintln!("telemetry snapshot lock poisoned");
                return;
            }
        };
        let frame = make_frame(&cfg, &snap);
        let power_key = cpu_power_warning_key(&cfg);
        let power = power_key.as_ref().and_then(|key| {
            snap.fresh(
                key,
                Instant::now(),
                Duration::from_millis(cfg.stale_after_ms),
            )
            .then(|| snap.value(key))
            .flatten()
        });
        warn_cpu_power(
            power,
            power_key.as_deref().unwrap_or("unmapped CPU power"),
            cfg.cpu_power_encoding == "legacy",
            &mut last_power_warning,
        );
        if dry {
            if last_dry_print.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
                println!(
                    "dry-run frame: {}",
                    frame
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                last_dry_print = Some(Instant::now());
            }
            runtime::interruptible_wait(&stop, Duration::from_millis(cfg.send_interval_ms));
            continue;
        }
        match session.as_mut().expect("connected session").send(&frame) {
            Ok(()) => {
                runtime::interruptible_wait(&stop, Duration::from_millis(cfg.send_interval_ms))
            }
            Err(e) => {
                session.take();
                backoff.reset();
                transition(
                    &mut state,
                    "disconnected",
                    &format!("USB send failed: {e:#}"),
                );
                runtime::interruptible_wait(&stop, backoff.delay());
            }
        }
    }
}

fn transition(state: &mut String, next: &str, message: &str) {
    if state != next {
        eprintln!("{message}");
        *state = next.into();
    }
}

fn make_frame(cfg: &Config, snap: &Snapshot) -> [u8; 64] {
    let (cpu, gpu, ch, gh) = mapping::groups(
        snap,
        &cfg.mapping,
        Instant::now(),
        Duration::from_millis(cfg.stale_after_ms),
        &cfg.cpu_power_encoding,
    );
    protocol::build_frame(cpu, gpu, ch, gh, calendar::now_local())
}

fn cpu_power_warning_key(cfg: &Config) -> Option<String> {
    let source = cfg
        .mapping
        .cpu
        .iter()
        .find_map(|s| mapping_power_key(s))
        .or_else(|| mapping_power_key(&cfg.mapping.cpu_power_high))?;
    Some(
        if cfg.telemetry.cpu_to_display && source.starts_with("cpu.") {
            source.replacen("cpu.", "gpu.", 1)
        } else {
            source
        },
    )
}
fn mapping_power_key(source: &str) -> Option<String> {
    let (group, metric) = source.split_once('.')?;
    if !matches!(group, "cpu" | "gpu") {
        return None;
    }
    if metric == "power_w" || metric.starts_with("power_") {
        Some(format!("{group}.power_w"))
    } else {
        None
    }
}

fn warn_cpu_power(power: Option<f64>, metric: &str, legacy: bool, previous: &mut Option<Instant>) {
    if let Some(power) = power {
        if legacy
            && power > 255.0
            && previous.is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
        {
            eprintln!("WARNING: {metric}={power:.2} W exceeds legacy byte capacity; default encoder saturates at 255 W");
            *previous = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn power_warning_tracks_effective_mapping_source_and_mirror() {
        let mut cfg = Config::default();
        assert_eq!(cpu_power_warning_key(&cfg).as_deref(), Some("cpu.power_w"));
        cfg.telemetry.cpu_to_display = true;
        assert_eq!(cpu_power_warning_key(&cfg).as_deref(), Some("gpu.power_w"));
        cfg.telemetry.cpu_to_display = false;
        cfg.mapping.cpu[4] = "ram.util_pct".into();
        cfg.mapping.cpu[5] = "cpu.power_fraction".into();
        cfg.mapping.cpu_power_high = "none".into();
        assert_eq!(cpu_power_warning_key(&cfg).as_deref(), Some("cpu.power_w"));
        cfg.mapping.cpu[5] = "none".into();
        cfg.mapping.cpu_power_high = "gpu.power_hundreds".into();
        assert_eq!(cpu_power_warning_key(&cfg).as_deref(), Some("gpu.power_w"));
    }
}
