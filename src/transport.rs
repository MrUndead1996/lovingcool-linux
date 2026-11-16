use anyhow::{bail, ensure, Context, Result};
use nusb::{
    transfer::{Interrupt, Out},
    Device, DeviceInfo, Interface, MaybeFuture,
};
use std::path::Path;
use std::time::Duration;

pub fn devices(vid: u16, pid: u16) -> Result<Vec<DeviceInfo>> {
    Ok(nusb::list_devices()
        .wait()
        .context("enumerating USB devices")?
        .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
        .collect())
}

pub fn choose<'a>(
    devices: &'a [DeviceInfo],
    serial: Option<&str>,
    location: Option<&str>,
) -> Result<Option<&'a DeviceInfo>> {
    let matches: Vec<_> = devices
        .iter()
        .filter(|d| serial.is_none_or(|s| d.serial_number() == Some(s)))
        .filter(|d| location.is_none_or(|p| d.sysfs_path().to_string_lossy() == p))
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0])),
        _ => bail!("multiple matching AK-03 devices; configure serial_number or location_path"),
    }
}

pub fn physical_identity(sysfs_path: &Path) -> Result<String> {
    let canonical = std::fs::canonicalize(sysfs_path)
        .with_context(|| format!("resolving physical USB identity {}", sysfs_path.display()))?;
    Ok(canonical.to_string_lossy().into_owned())
}

pub fn inspect(info: &DeviceInfo, interface: u8, endpoint: u8) -> Result<()> {
    let device = info
        .open()
        .wait()
        .context("opening USB device for inspection")?;
    inspect_open_device(&device, interface, endpoint)
}

fn inspect_open_device(device: &Device, interface: u8, endpoint: u8) -> Result<()> {
    let active = device
        .active_configuration()
        .context("reading active USB configuration")?;
    let target = active
        .interfaces()
        .find(|i| i.interface_number() == interface)
        .with_context(|| format!("interface {interface} absent from active configuration"))?;
    let desc = target
        .alt_settings()
        .find(|alt| alt.alternate_setting() == 0)
        .context("interface's default alternate setting is missing")?;
    check_endpoint(desc.endpoints(), interface, endpoint)
}

fn check_endpoint<'a>(
    mut endpoints: impl Iterator<Item = nusb::descriptors::EndpointDescriptor<'a>>,
    interface: u8,
    endpoint: u8,
) -> Result<()> {
    ensure!(
        endpoint != 0 && endpoint & 0x80 == 0,
        "endpoint address must be OUT and nonzero"
    );
    let ep = endpoints
        .find(|ep| ep.address() == endpoint)
        .with_context(|| {
            format!("OUT endpoint {endpoint:#04x} absent from interface {interface} alternate 0")
        })?;
    ensure!(
        ep.address() & 0x80 == 0,
        "configured endpoint is IN, not OUT"
    );
    ensure!(
        ep.transfer_type() == nusb::descriptors::TransferType::Interrupt,
        "endpoint is not interrupt type"
    );
    ensure!(
        ep.max_packet_size() == 64,
        "endpoint max packet size is {}, expected 64",
        ep.max_packet_size()
    );
    Ok(())
}

/// Owns one open device, detached/claimed interface, and endpoint for its lifetime.
pub struct UsbSession {
    _device: Device,
    _interface: Interface,
    endpoint: nusb::Endpoint<Interrupt, Out>,
}

impl UsbSession {
    pub fn open(info: &DeviceInfo, interface: u8, endpoint: u8) -> Result<Self> {
        let device = info.open().wait().context("opening USB device")?;
        inspect_open_device(&device, interface, endpoint)?;
        // AK-03 exposes HID/usbhid. On Linux, detach and claim is required before
        // userspace endpoint I/O; nusb reattaches on interface release.
        let iface = device
            .detach_and_claim_interface(interface)
            .wait()
            .context("detaching kernel HID driver and claiming interface")?;
        ensure!(
            iface.get_alt_setting() == 0,
            "unexpected active interface alternate setting"
        );
        let descriptor = iface
            .descriptor()
            .context("active interface descriptor unavailable")?;
        check_endpoint(descriptor.endpoints(), interface, endpoint)?;
        let ep = iface.endpoint::<Interrupt, Out>(endpoint)?;
        ensure!(
            ep.max_packet_size() == 64,
            "claimed endpoint packet size mismatch"
        );
        Ok(Self {
            _device: device,
            _interface: iface,
            endpoint: ep,
        })
    }

    pub fn send(&mut self, frame: &[u8; 64]) -> Result<()> {
        let completion = self
            .endpoint
            .transfer_blocking(frame.to_vec().into(), Duration::from_millis(500));
        completion.status.context("interrupt OUT transfer failed")?;
        ensure!(
            completion.actual_len == 64,
            "short USB write: {} bytes",
            completion.actual_len
        );
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct Backoff {
    attempt: u32,
}
impl Backoff {
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
    pub fn delay(&mut self) -> Duration {
        let ms = 500u64
            .saturating_mul(1u64 << self.attempt.min(5))
            .min(15_000);
        self.attempt = self.attempt.saturating_add(1);
        Duration::from_millis(ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reconnect_backoff_is_bounded_and_resets() {
        let mut b = Backoff::default();
        assert_eq!(b.delay(), Duration::from_millis(500));
        for _ in 0..10 {
            assert!(b.delay() <= Duration::from_secs(15));
        }
        b.reset();
        assert_eq!(b.delay(), Duration::from_millis(500));
    }
    #[cfg(unix)]
    #[test]
    fn physical_identity_uses_canonical_sysfs_path() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let physical = root.path().join("usb1-1.2");
        std::fs::create_dir(&physical).unwrap();
        let alias = root.path().join("alias");
        symlink(&physical, &alias).unwrap();
        assert_eq!(
            physical_identity(&physical).unwrap(),
            physical_identity(&alias).unwrap()
        );
    }
}
