use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub vendor_id: u16,
    /// Config files may select only the normal AK-03 PID; 0x2017 is probe-only.
    pub product_id: u16,
    pub interface: u8,
    pub endpoint_out: u8,
    pub send_interval_ms: u64,
    pub poll_interval_ms: u64,
    pub stale_after_ms: u64,
    /// Legacy saturating CPU watts byte is default; decimal is experimental.
    pub cpu_power_encoding: String,
    pub serial_number: Option<String>,
    /// Stable USB sysfs port path (not transient bus/device address).
    pub location_path: Option<String>,
    pub telemetry: TelemetryConfig,
    pub mapping: MappingConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    pub gpu_uuid: Option<String>,
    pub cpu_to_display: bool,
    pub hwmon_cpu_name: Option<String>,
    pub hwmon_cpu_label: Option<String>,
    pub hwmon_cpu_power_name: Option<String>,
    pub hwmon_cpu_power_label: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MappingConfig {
    /// Eight ordered bytes per group. "none" emits zero. Derived metrics:
    /// *_power_hundreds/remainder/fraction and *_clock_hundreds/remainder.
    pub cpu: [String; 8],
    pub gpu: [String; 8],
    pub cpu_power_high: String,
    pub gpu_power_high: String,
}

impl Default for MappingConfig {
    fn default() -> Self {
        Self {
            cpu: [
                "cpu.temp_c".into(),
                "none".into(),
                "none".into(),
                "cpu.util_pct".into(),
                "cpu.power_low".into(),
                "cpu.power_fraction".into(),
                "cpu.clock_hundreds".into(),
                "cpu.clock_remainder".into(),
            ],
            gpu: [
                "gpu.temp_c".into(),
                "none".into(),
                "none".into(),
                "gpu.util_pct".into(),
                "gpu.power_remainder".into(),
                "none".into(),
                "gpu.clock_hundreds".into(),
                "gpu.clock_remainder".into(),
            ],
            cpu_power_high: "cpu.power_high".into(),
            gpu_power_high: "gpu.power_hundreds".into(),
        }
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            vendor_id: 0x5131,
            product_id: 0x200b,
            interface: 0,
            endpoint_out: 1,
            send_interval_ms: 200,
            poll_interval_ms: 1000,
            stale_after_ms: 5000,
            cpu_power_encoding: "legacy".into(),
            serial_number: None,
            location_path: None,
            telemetry: TelemetryConfig::default(),
            mapping: MappingConfig::default(),
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let c: Self = toml::from_str(&text).context("parsing configuration")?;
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.vendor_id == 0x5131,
            "only vendor ID 0x5131 is supported"
        );
        anyhow::ensure!(
            self.product_id == 0x200b,
            "config product_id must be 0x200b; PID 0x2017 is probe-only"
        );
        anyhow::ensure!(
            self.endpoint_out != 0 && self.endpoint_out & 0x80 == 0,
            "endpoint_out must be a nonzero OUT endpoint"
        );
        anyhow::ensure!(
            self.cpu_power_encoding == "legacy" || self.cpu_power_encoding == "decimal",
            "cpu_power_encoding must be 'legacy' or 'decimal'"
        );
        for ms in [
            self.send_interval_ms,
            self.poll_interval_ms,
            self.stale_after_ms,
        ] {
            anyhow::ensure!(
                (10..=300_000).contains(&ms),
                "intervals must be in 10..=300000 ms"
            );
        }
        anyhow::ensure!(
            self.serial_number.is_none() || self.location_path.is_none(),
            "select at most one device using serial_number or location_path"
        );
        anyhow::ensure!(
            self.telemetry.hwmon_cpu_name.is_some() == self.telemetry.hwmon_cpu_label.is_some(),
            "hwmon_cpu_name and hwmon_cpu_label must be configured together"
        );
        anyhow::ensure!(
            self.telemetry.hwmon_cpu_power_name.is_some()
                == self.telemetry.hwmon_cpu_power_label.is_some(),
            "hwmon_cpu_power_name and hwmon_cpu_power_label must be configured together"
        );
        for source in self
            .mapping
            .cpu
            .iter()
            .chain(self.mapping.gpu.iter())
            .chain([&self.mapping.cpu_power_high, &self.mapping.gpu_power_high])
        {
            anyhow::ensure!(
                valid_source(source),
                "unsupported metric mapping source '{source}'"
            );
        }
        Ok(())
    }
}
fn valid_source(s: &str) -> bool {
    if s == "none" {
        return true;
    }
    ["cpu", "gpu"].iter().any(|p| {
        [
            "temp_c",
            "util_pct",
            "power_w",
            "power_low",
            "power_high",
            "power_hundreds",
            "power_remainder",
            "power_fraction",
            "clock_mhz",
            "clock_hundreds",
            "clock_remainder",
        ]
        .iter()
        .any(|m| s == format!("{p}.{m}"))
    }) || s == "ram.util_pct"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_keys_and_invalid_endpoints_and_durations() {
        assert!(toml::from_str::<Config>("surprise = true").is_err());
        let c = Config {
            endpoint_out: 0,
            ..Config::default()
        };
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.telemetry.hwmon_cpu_power_name = Some("power_driver".into());
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.telemetry.hwmon_cpu_label = Some("Package id 0".into());
        assert!(c.validate().is_err());
        let c = Config {
            send_interval_ms: 0,
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }
    #[test]
    fn pid2017_cannot_be_enabled_via_config() {
        let c = Config {
            product_id: 0x2017,
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }
}
