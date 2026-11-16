# cooling-display

Linux telemetry bridge for Lovingcool AK-03 USB display (`5131:200b`).

The display uses 64-byte endpoint payloads formed by removing the report-ID byte
from a 64-byte HID report and padding the remaining 63 bytes with one zero.
Independent static analysis of vendor Screen Driver Software v2.2.65.0 is
documented in the [protocol notes](docs/protocol.md). No vendor binary/source is
included.

## Install from a release (recommended)

For a published version, open the
[GitHub Releases page](https://github.com/MrUndead1996/lovingcool-linux/releases)
and download **both** the archive and its matching `.sha256` file. Choose the
asset for your machine:

```sh
uname -m
# x86_64 -> cooling-display-x86_64-unknown-linux-gnu.tar.gz
# aarch64 -> cooling-display-aarch64-unknown-linux-gnu.tar.gz
```

In the directory containing the downloaded files, verify the checksum, extract
the archive, and install the binary:

```sh
sha256sum -c cooling-display-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf cooling-display-x86_64-unknown-linux-gnu.tar.gz
cd cooling-display
sudo install -D -m 0755 cooling-display /usr/bin/cooling-display
```

For an aarch64 machine, use `cooling-display-aarch64-unknown-linux-gnu.tar.gz`
in both checksum and extraction commands instead. The extracted directory
contains the binary, example config, docs, packaging files, and licenses.
These are GNU/Linux builds for the listed architectures.

### Build from source (optional)

With Rust and Cargo installed, build and install the binary from the repository:

```sh
cargo build --locked --release
sudo install -D -m 0755 target/release/cooling-display /usr/bin/cooling-display
```

### Run or test

```sh
cooling-display --config config/example.toml probe
cooling-display --config config/example.toml test
cooling-display --config config/example.toml test --send --duration-seconds 3
cooling-display --config config/example.toml run --dry-run
cooling-display --config config/example.toml run
```

When running from the source repository without installing the binary, replace
`cooling-display` with `cargo run --`.

## Configuration

Configuration is optional: if `/etc/cooling-display.toml` does not exist, the
program uses built-in defaults (also when that exact path is passed with
`--config`). A missing file at another explicitly selected path, or any
unreadable, malformed, or invalid existing file, is an error. Copy the example
to a writable location, edit it, then pass the global `--config` option
**before** the command:

```sh
cp config/example.toml "$HOME/cooling-display.toml"
${EDITOR:-vi} "$HOME/cooling-display.toml"
cooling-display --config "$HOME/cooling-display.toml" probe
cooling-display --config "$HOME/cooling-display.toml" run --dry-run
```

When running from the source repository without installing the binary, replace
`cooling-display` with `cargo run --`.

For a single NVIDIA GPU no GPU selector is needed. For multiple GPUs, set
`telemetry.gpu_uuid` to a UUID printed by `probe`. CPU temperature uses the
exact hwmon name and label printed by `probe` (for example `k10temp` and
`Tctl`, if those are the values actually reported on your system); optional
CPU package power similarly requires its exact name/label pair. CPU/GPU
screen-byte mappings and all supported settings are described in the
[configuration reference](docs/configuration.md).

To show NVIDIA telemetry in the currently selected CPU display block, use
`cpu_to_display = true`. For multiple GPUs, replace the commented UUID with
one reported by `probe`:

```toml
[telemetry]
cpu_to_display = true
# gpu_uuid = "GPU-REPLACE-WITH-UUID-FROM-PROBE"
```

For a CPU temperature sensor, set the exact pair reported by `probe` (this
example applies only if the machine reports `k10temp` and `Tctl`):

```toml
[telemetry]
hwmon_cpu_name = "k10temp"
hwmon_cpu_label = "Tctl"
```

These are alternatives or can be combined as keys in a single `[telemetry]`
table; do not repeat the table header in one TOML file. Mirroring replaces the
CPU metrics with GPU metrics in the CPU display block. Configuration details
and byte mapping semantics are in the reference.

### Install as a system service

Run these commands from either the extracted release directory or the source
repository. The binary must already be installed using one of the methods
above. Review `config/example.toml` and the service account policy first:

```sh
sudo groupadd --system cooling-display
sudo useradd --system --no-create-home --shell /usr/sbin/nologin \
  --gid cooling-display cooling-display
sudo install -D -m 0640 -o root -g cooling-display \
  config/example.toml /etc/cooling-display.toml
sudo install -D -m 0644 packaging/70-cooling-display.rules \
  /etc/udev/rules.d/70-cooling-display.rules
sudo install -D -m 0644 packaging/cooling-display.service \
  /etc/systemd/system/cooling-display.service
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=usb
sudo systemctl daemon-reload
sudo systemctl enable --now cooling-display.service
```

If the `cooling-display` group or user already exists, omit its corresponding
creation command. NVIDIA telemetry additionally requires that the service user
can access the host's NVIDIA device nodes (commonly `/dev/nvidia*`); configure
the host's NVIDIA/udev permissions accordingly. AK-03 hardware has been verified
with the `5131:200b` probe, a synthetic temperature test frame, and live NVIDIA
telemetry displayed on the screen.

To change a service configuration, edit `/etc/cooling-display.toml` and run
`sudo systemctl restart cooling-display.service`; configuration is read at
startup and is not hot-reloaded.

`test` prints one finite synthetic frame and never sends unless `--send` is
explicitly supplied. It supports selectable CPU/GPU temperatures, utilization,
power, and clocks. `probe` only enumerates and inspects; PID `0x2017` is
probe-only because its HID usage-page-specific protocol is not implemented.

`run` requires exactly one matching device, or a configured `serial_number` or
stable `location_path`. It checks the active USB configuration's default
alternate setting, detaches/claims the Linux HID interface once per connection,
and keeps the endpoint open until disconnect. Reconnect uses bounded backoff.
The sender uses a per-user advisory lock that is released by the OS on exit.
Lock identity is the canonical physical USB sysfs path, not config selection.
Standalone runs use `$XDG_RUNTIME_DIR/cooling-display` when available, or a
private per-UID directory under `/tmp`. The systemd unit uses
`/run/cooling-display`; set `COOLING_DISPLAY_LOCK_DIR=/run/cooling-display`
for a standalone process that must contend with that service instance.
Shutdown signals both workers and waits at most 1500 ms before returning rather
than joining potentially stalled driver calls. NVML calls are synchronous and
cannot be cancelled; if one is stuck, the telemetry worker is abandoned as the
process exits.

CPU utilization and RAM usage are read from `/proc`. CPU temperature requires
an exact hwmon name and label selected after inspecting `probe` output. CPU
frequency is the mean of readable `scaling_cur_freq` values across cpufreq
policies; if the kernel exposes none, it remains unavailable. CPU power is
optional: configure the exact hwmon name and power label from `probe`; input is
interpreted in µW. No RAPL assumption is made, and absent power sensors map to
zero. NVIDIA metrics use runtime-loaded `nvml-wrapper`; when more than one GPU
exists, `telemetry.gpu_uuid` is required. Stale values retain their last
timestamp and fall back independently after the freshness limit.

CPU power encoding defaults to `legacy`: whole Watts are stored in one byte,
saturating at 255, with the existing high-byte field. Above 255 W, the
measurement is retained internally but the default encoded value saturates;
the process warns. `cpu_power_encoding = "decimal"` is an explicitly
experimental extension only for values above 255 W; values through 255 W retain
the same byte layout. Its over-255 behavior has not been hardware-tested.
GPU whole-Watt power is independently encoded into low and hundreds bytes.

Stale metrics fall back independently to zero. No confirmed clear-screen command
is known, so zero fallback does not guarantee that a stale display is cleared.

The supplied udev rule grants access only to `5131:200b`. The CI workflow is
configured to test natively on x86_64 and aarch64 Linux; tagged releases package
the binary, configuration, docs, licensing, udev/systemd files, and SHA-256
checksums.

## Evidence and verification

The probe, synthetic temperature display, and live NVIDIA telemetry have been
verified on AK-03 hardware (`5131:200b`). Vendor-app findings in the protocol
notes are based on static analysis. Power values above 255 W and service
reconnection have not been hardware-tested.

`cargo test --locked`, `cargo fmt --check`, and
`cargo clippy --all-targets --locked -- -D warnings` run in CI.
