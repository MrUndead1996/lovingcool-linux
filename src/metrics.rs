use std::{collections::HashMap, time::Instant};

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    values: HashMap<String, (f64, Instant)>,
}
impl Snapshot {
    pub fn set(&mut self, key: impl Into<String>, value: f64, at: Instant) {
        self.values.insert(key.into(), (value, at));
    }
    pub fn value(&self, key: &str) -> Option<f64> {
        self.values.get(key).map(|v| v.0)
    }
    pub fn fresh(&self, key: &str, now: Instant, max_age: std::time::Duration) -> bool {
        self.values
            .get(key)
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) <= max_age)
    }
    pub fn mirror_prefix(&mut self, from: &str, to: &str) {
        self.remove_prefix(to);
        let entries: Vec<_> = self
            .values
            .iter()
            .filter(|(k, _)| k.starts_with(from))
            .map(|(k, v)| (format!("{to}{}", &k[from.len()..]), *v))
            .collect();
        self.values.extend(entries);
    }
    pub fn remove_prefix(&mut self, prefix: &str) {
        self.values.retain(|key, _| !key.starts_with(prefix));
    }
    /// Retain the newest publication per metric while preserving original
    /// monotonic timestamps for readings that failed to refresh this cycle.
    pub fn merge_latest(&mut self, newer: Snapshot) {
        self.values.extend(newer.values);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn freshness_is_per_metric() {
        let now = Instant::now();
        let mut s = Snapshot::default();
        s.set("cpu", 1.0, now - std::time::Duration::from_secs(6));
        s.set("gpu", 2.0, now);
        assert!(!s.fresh("cpu", now, std::time::Duration::from_secs(5)));
        assert!(s.fresh("gpu", now, std::time::Duration::from_secs(5)));
    }
    #[test]
    fn mirror_copies_values_and_monotonic_timestamps() {
        let now = Instant::now();
        let mut s = Snapshot::default();
        s.set("gpu.util_pct", 73.0, now);
        s.mirror_prefix("gpu.", "cpu.");
        assert_eq!(s.value("cpu.util_pct"), Some(73.0));
        assert!(s.fresh("cpu.util_pct", now, std::time::Duration::ZERO));
    }
    #[test]
    fn merge_keeps_last_good_timestamp_when_metric_is_not_refreshed() {
        let t = Instant::now();
        let mut latest = Snapshot::default();
        latest.set("gpu.power_w", 400.0, t);
        latest.merge_latest(Snapshot::default());
        assert_eq!(latest.value("gpu.power_w"), Some(400.0));
        assert!(!latest.fresh(
            "gpu.power_w",
            t + std::time::Duration::from_secs(6),
            std::time::Duration::from_secs(5)
        ));
    }

    #[test]
    fn gpu_mirroring_never_falls_back_to_local_cpu_and_old_gpu_values_expire() {
        let t = Instant::now();
        let mut latest = Snapshot::default();
        latest.set("gpu.util_pct", 73.0, t);
        latest.set("cpu.util_pct", 73.0, t);

        // A new poll has healthy local CPU load, but only partial GPU data.
        let mut local = Snapshot::default();
        local.set("cpu.util_pct", 12.0, t + std::time::Duration::from_secs(1));
        local.set("gpu.temp_c", 61.0, t + std::time::Duration::from_secs(1));
        local.mirror_prefix("gpu.", "cpu.");
        latest.merge_latest(local);

        assert_eq!(latest.value("cpu.util_pct"), Some(73.0));
        assert!(!latest.fresh(
            "cpu.util_pct",
            t + std::time::Duration::from_secs(6),
            std::time::Duration::from_secs(5)
        ));
        assert_eq!(latest.value("cpu.temp_c"), Some(61.0));
    }
}
