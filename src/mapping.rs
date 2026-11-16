use crate::{config::MappingConfig, metrics::Snapshot, protocol::DisplayGroup};
use std::time::{Duration, Instant};

fn resolve(
    snapshot: &Snapshot,
    source: &str,
    now: Instant,
    max_age: Duration,
    cpu_encoding: &str,
) -> Option<f64> {
    if source == "none" {
        return Some(0.0);
    }
    if let Some(value) = snapshot.value(source) {
        return snapshot.fresh(source, now, max_age).then_some(value);
    }
    let (base, suffix) = source.rsplit_once('.')?;
    let metric = match suffix {
        "power_hundreds" | "power_remainder" | "power_fraction" | "power_low" | "power_high" => {
            "power_w"
        }
        "clock_hundreds" | "clock_remainder" => "clock_mhz",
        _ => return None,
    };
    let key = format!("{base}.{metric}");
    if !snapshot.fresh(&key, now, max_age) {
        return None;
    }
    let value = snapshot.value(&key)?;
    let value = match metric {
        "power_w" => value.max(0.0),
        "clock_mhz" => value.clamp(0.0, 9999.0),
        _ => value,
    };
    Some(match suffix {
        "power_low" if base == "cpu" && (cpu_encoding == "legacy" || value <= 255.0) => {
            value.floor().clamp(0.0, 255.0)
        }
        "power_high" if base == "cpu" && (cpu_encoding == "legacy" || value <= 255.0) => {
            (value.clamp(0.0, 255.0) / 100.0).floor()
        }
        "power_high" | "power_hundreds" => (value / 100.0).floor(),
        "power_low" => value.floor().rem_euclid(100.0),
        "power_remainder" => value.floor().rem_euclid(100.0),
        "power_fraction" if base == "cpu" && (cpu_encoding == "legacy" || value <= 255.0) => 0.0,
        "power_fraction" => ((value.fract().abs() * 100.0).round()).clamp(0.0, 99.0),
        "clock_hundreds" => (value / 100.0).floor(),
        "clock_remainder" => value.floor().rem_euclid(100.0),
        _ => unreachable!(),
    })
}

fn encode(
    snapshot: &Snapshot,
    sources: &[String; 8],
    now: Instant,
    age: Duration,
    cpu_encoding: &str,
) -> DisplayGroup {
    let mut bytes = [0; 8];
    for (i, source) in sources.iter().enumerate() {
        let max = match i {
            0 => 99.0,
            3 => 100.0,
            _ => 255.0,
        };
        bytes[i] = resolve(snapshot, source, now, age, cpu_encoding)
            .unwrap_or(0.0)
            .clamp(0.0, max) as u8;
    }
    DisplayGroup(bytes)
}

pub fn groups(
    snapshot: &Snapshot,
    map: &MappingConfig,
    now: Instant,
    age: Duration,
    cpu_encoding: &str,
) -> (DisplayGroup, DisplayGroup, u8, u8) {
    let cpu = encode(snapshot, &map.cpu, now, age, cpu_encoding);
    let gpu = encode(snapshot, &map.gpu, now, age, cpu_encoding);
    let cpu_high = resolve(snapshot, &map.cpu_power_high, now, age, cpu_encoding)
        .unwrap_or(0.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    let gpu_high = resolve(snapshot, &map.gpu_power_high, now, age, cpu_encoding)
        .unwrap_or(0.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    (cpu, gpu, cpu_high, gpu_high)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MappingConfig;
    #[test]
    fn freshness_and_independent_power_splits() {
        let now = Instant::now();
        let mut s = Snapshot::default();
        s.set("cpu.power_w", 237.25, now);
        s.set("gpu.power_w", 451.5, now);
        let (cpu, gpu, ch, gh) = groups(
            &s,
            &MappingConfig::default(),
            now,
            Duration::from_secs(3),
            "decimal",
        );
        assert_eq!(cpu.0[4..6], [237, 0]);
        assert_eq!(gpu.0[4..6], [51, 0]);
        assert_eq!((ch, gh), (2, 4));
    }
    #[test]
    fn stale_fields_fall_back_individually() {
        let now = Instant::now();
        let mut s = Snapshot::default();
        s.set("cpu.temp_c", 72.0, now - Duration::from_secs(10));
        s.set("cpu.util_pct", 63.0, now);
        let (cpu, _, _, _) = groups(
            &s,
            &MappingConfig::default(),
            now,
            Duration::from_secs(5),
            "legacy",
        );
        assert_eq!(cpu.0[0], 0);
        assert_eq!(cpu.0[3], 63);
    }
    #[test]
    fn cpu_default_saturates_whole_watts_and_decimal_is_opt_in() {
        let now = Instant::now();
        for (power, low, high) in [
            (99.0, 99, 0),
            (100.0, 100, 1),
            (255.0, 255, 2),
            (256.0, 255, 2),
            (300.0, 255, 2),
        ] {
            let mut s = Snapshot::default();
            s.set("cpu.power_w", power, now);
            let (cpu, _, ch, _) = groups(
                &s,
                &MappingConfig::default(),
                now,
                Duration::from_secs(1),
                "legacy",
            );
            assert_eq!((cpu.0[4], ch), (low, high));
        }
        let mut s = Snapshot::default();
        s.set("cpu.power_w", 300.25, now);
        let (cpu, _, ch, _) = groups(
            &s,
            &MappingConfig::default(),
            now,
            Duration::from_secs(1),
            "decimal",
        );
        assert_eq!((cpu.0[4], cpu.0[5], ch), (0, 25, 3));
        let mut s = Snapshot::default();
        s.set("cpu.power_w", 255.0, now);
        let (cpu, _, ch, _) = groups(
            &s,
            &MappingConfig::default(),
            now,
            Duration::from_secs(1),
            "decimal",
        );
        assert_eq!((cpu.0[4], ch), (255, 2));
    }
    #[test]
    fn gpu_high_power_byte_is_independent_and_preserves_watts_over_255() {
        let now = Instant::now();
        for (power, low, high) in [
            (99.0, 99, 0),
            (100.0, 0, 1),
            (255.0, 55, 2),
            (256.0, 56, 2),
            (300.0, 0, 3),
        ] {
            let mut s = Snapshot::default();
            s.set("gpu.power_w", power, now);
            let (_, gpu, _, gpu_high) = groups(
                &s,
                &MappingConfig::default(),
                now,
                Duration::from_secs(1),
                "legacy",
            );
            assert_eq!((gpu.0[4], gpu_high), (low, high));
        }
    }
    #[test]
    fn default_mapping_matches_legacy_groups_for_same_237w_input() {
        let now = Instant::now();
        let mut s = Snapshot::default();
        for prefix in ["cpu", "gpu"] {
            s.set(format!("{prefix}.temp_c"), 45.0, now);
            s.set(format!("{prefix}.util_pct"), 82.0, now);
            s.set(format!("{prefix}.power_w"), 237.0, now);
            s.set(format!("{prefix}.clock_mhz"), 1234.0, now);
        }
        let (cpu, gpu, ch, gh) = groups(
            &s,
            &MappingConfig::default(),
            now,
            Duration::from_secs(1),
            "legacy",
        );
        assert_eq!(cpu.0, [45, 0, 0, 82, 237, 0, 12, 34]);
        assert_eq!(gpu.0, [45, 0, 0, 82, 37, 0, 12, 34]);
        assert_eq!((ch, gh), (2, 2));
    }
    #[test]
    fn derived_clock_clamps_to_four_digit_display_range() {
        let now = Instant::now();
        let mut s = Snapshot::default();
        s.set("cpu.clock_mhz", 10000.0, now);
        let (cpu, _, _, _) = groups(
            &s,
            &MappingConfig::default(),
            now,
            Duration::from_secs(1),
            "legacy",
        );
        assert_eq!((cpu.0[6], cpu.0[7]), (99, 99));
    }
    #[test]
    fn temperature_and_utilization_truncate_like_legacy_python() {
        let now = Instant::now();
        let mut s = Snapshot::default();
        s.set("cpu.temp_c", 45.9, now);
        s.set("cpu.util_pct", 82.9, now);
        let (cpu, _, _, _) = groups(
            &s,
            &MappingConfig::default(),
            now,
            Duration::from_secs(1),
            "legacy",
        );
        assert_eq!((cpu.0[0], cpu.0[3]), (45, 82));
    }
}
