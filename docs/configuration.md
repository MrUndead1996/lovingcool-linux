# Configuration

`cooling-display` reads TOML from `/etc/cooling-display.toml` by default. If
that default path is absent, built-in defaults are used (also when that exact
path is passed with `--config`). A missing file at another explicitly selected
path, or any unreadable, malformed, or invalid existing file, is an error.
Pass the global `--config PATH` option **before** the subcommand:

```sh
cooling-display --config /etc/cooling-display.toml run
cooling-display --config "$HOME/.config/cooling-display.toml" run --dry-run
```

Start from the supplied configuration, then edit it:

```sh
mkdir -p "$HOME/.config"
cp config/example.toml "$HOME/.config/cooling-display.toml"
${EDITOR:-vi} "$HOME/.config/cooling-display.toml"
cooling-display --config "$HOME/.config/cooling-display.toml" probe
cooling-display --config "$HOME/.config/cooling-display.toml" run --dry-run
```

When running from the source repository without installing the binary, replace
`cooling-display` with `cargo run --`.

The same file can be used with `test`, `probe`, and `run`. Configuration is
loaded once at startup; edits do not hot-reload. For systemd, edit
`/etc/cooling-display.toml` and restart with
`sudo systemctl restart cooling-display.service`.

## Root settings

All keys are optional; omitted fields use the values below. Unknown keys are
rejected (including misspellings). TOML types must match the table.

| Key | Type | Default | Constraint / meaning |
| --- | --- | --- | --- |
| `vendor_id` | integer | `20785` (`0x5131`) | Only `0x5131` is supported. |
| `product_id` | integer | `8203` (`0x200b`) | Must be `0x200b`. `0x2017` is probe-only and cannot be selected by config. |
| `interface` | integer | `0` | USB interface number (`u8`). |
| `endpoint_out` | integer | `1` | Nonzero OUT endpoint (`u8`; high bit must be clear). |
| `send_interval_ms` | integer | `200` | Send interval, 10–300000 ms. |
| `poll_interval_ms` | integer | `1000` | Telemetry polling interval, 10–300000 ms. |
| `stale_after_ms` | integer | `5000` | Per-metric freshness limit, 10–300000 ms. |
| `cpu_power_encoding` | string | `"legacy"` | `"legacy"` or experimental `"decimal"`; see [CPU power encoding](#cpu-power-encoding). |
| `serial_number` | string | unset | Optional exact USB serial selector. Mutually exclusive with `location_path`. |
| `location_path` | string | unset | Optional stable USB sysfs path selector (for example `/sys/devices/.../usb1/1-2`), not a transient bus/device address. Mutually exclusive with `serial_number`. |

With neither selector set, `run` requires exactly one matching device. Use
`probe` to see serial and sysfs path values and inspect the device before
selecting one.

## Telemetry

All telemetry settings belong under `[telemetry]`:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `gpu_uuid` | string | unset | Selects an NVIDIA GPU by UUID. Required if NVML finds multiple GPUs; `probe` prints available UUIDs. |
| `cpu_to_display` | boolean | `false` | Mirrors GPU metrics into the `cpu.*` metric namespace before mapping. The protocol frame currently hardcodes CPU/GPU selection to `0` (the CPU display block), so mirroring is how GPU telemetry reaches that selected block. It also replaces real CPU readings in that namespace while enabled. It does not change the panel mode flags or turn the GPU group off; configuring only the GPU mapping may therefore not change the visibly selected screen. |
| `hwmon_cpu_name` | string | unset | Exact hwmon `name` for CPU temperature. Must be set together with `hwmon_cpu_label`. |
| `hwmon_cpu_label` | string | unset | Exact temperature label. Must be set together with `hwmon_cpu_name`. |
| `hwmon_cpu_power_name` | string | unset | Exact hwmon `name` for CPU package power. Must be set together with `hwmon_cpu_power_label`. |
| `hwmon_cpu_power_label` | string | unset | Exact power label. Must be set together with `hwmon_cpu_power_name`. |

Use `probe` to inspect the actual hwmon names, labels, and NVIDIA UUIDs on this
machine. For example, when probe shows `name=k10temp` and a temperature label
`Tctl`, configure the exact pair:

```toml
[telemetry]
hwmon_cpu_name = "k10temp"
hwmon_cpu_label = "Tctl"
```

The example is illustrative: use the values reported by your own `probe`.
Temperature is read from the matching `temp*_input` in millidegrees Celsius.
CPU package power is optional and separately selected by its exact name and
power label; `power*_input` is interpreted as microwatts. No RAPL source is
assumed. CPU utilization and RAM utilization come from `/proc`; CPU frequency
is the mean of readable cpufreq `scaling_cur_freq` policies. NVIDIA telemetry
uses NVML. A metric the platform cannot provide is unavailable, not
synthesized: its display byte is zero. An unsupported individual GPU metric
does not make other GPU metrics unavailable.

When `cpu_to_display = true`, the GPU values are copied to `cpu.*` before
encoding. Consequently, using `cpu.*` sources in a custom CPU mapping will
read the mirrored GPU values, not actual CPU readings. The protocol's
CPU/GPU selection is currently hardcoded to `0`; mapping the GPU group alone
does not change that selection or the panel mode flags.

## Mapping bytes

`[mapping]` is optional. Each of `cpu` and `gpu` is an array of exactly eight
source strings, one per byte in order (positions 1–8 below). The defaults are:

```toml
[mapping]
cpu = ["cpu.temp_c", "none", "none", "cpu.util_pct", "cpu.power_low", "cpu.power_fraction", "cpu.clock_hundreds", "cpu.clock_remainder"]
gpu = ["gpu.temp_c", "none", "none", "gpu.util_pct", "gpu.power_remainder", "none", "gpu.clock_hundreds", "gpu.clock_remainder"]
cpu_power_high = "cpu.power_high"
gpu_power_high = "gpu.power_hundreds"
```

| Byte position | Conventional meaning | Default CPU source | Default GPU source |
| --- | --- | --- | --- |
| 1 | Temperature, whole °C | `cpu.temp_c` | `gpu.temp_c` |
| 2 | Temperature fractional component | `none` | `none` |
| 3 | Temperature unit selector (`0` = °C, `1` = °F) | `none` (zero/°C) | `none` (zero/°C) |
| 4 | Utilization, percent | `cpu.util_pct` | `gpu.util_pct` |
| 5 | Power low/remainder byte | `cpu.power_low` | `gpu.power_remainder` |
| 6 | CPU fractional power byte / reserved for GPU | `cpu.power_fraction` | `none` |
| 7–8 | Clock hundreds / remainder | `cpu.clock_hundreds`, `cpu.clock_remainder` | `gpu.clock_hundreds`, `gpu.clock_remainder` |

The independent `cpu_power_high` and `gpu_power_high` settings are additional
power hundreds bytes in the frame, not members of the eight-element arrays.
The recognized raw sources are `none`, `cpu.temp_c`, `cpu.util_pct`,
`cpu.power_w`, `cpu.power_low`, `cpu.power_high`, `cpu.power_hundreds`,
`cpu.power_remainder`, `cpu.power_fraction`, `cpu.clock_mhz`,
`cpu.clock_hundreds`, `cpu.clock_remainder`, the corresponding `gpu.*` names,
and `ram.util_pct`. Derived sources use the corresponding fresh `*.power_w` or
`*.clock_mhz` value. An unavailable or stale source resolves to zero
independently; other fields continue to be encoded.

| Derived source | Encoding |
| --- | --- |
| `*.power_low` | For CPU in legacy mode, whole watts capped at 255; for CPU decimal values above 255 W, and for GPU, whole-watt remainder modulo 100. |
| `*.power_remainder` | Whole-watt remainder modulo 100. |
| `*.power_fraction` | Rounded hundredths of a watt (0–99); for CPU, zero in legacy mode and through 255 W in decimal mode. The default GPU mapping does not use this source. |
| `*.power_high` | CPU-specific high byte: hundreds digit of the capped value in legacy mode and through 255 W in decimal mode; above 255 W, hundreds digit of actual watts. For GPU this alias behaves as hundreds of watts. |
| `*.power_hundreds` | Hundreds of watts, floored. |
| `*.clock_hundreds` / `*.clock_remainder` | For MHz, integer hundreds and integer remainder modulo 100. |

Examples of custom mappings (keep root settings above `[mapping]` if combining
the snippets in one file):

```toml
[mapping]
# Use byte 4, the utilization slot; leave temperature fraction and unit at zero.
cpu = ["cpu.temp_c", "none", "none", "ram.util_pct", "cpu.power_low", "cpu.power_fraction", "cpu.clock_hundreds", "cpu.clock_remainder"]
```

Keep the temperature fraction and unit bytes at zero unless intentionally
encoding those fields; the mapping accepts arbitrary metrics but does not make
them meaningful in every protocol position.

For a GPU power layout that pairs the remainder byte with the separate
hundreds byte, the defaults already do this: byte 5 is
`gpu.power_remainder`, and `gpu_power_high` is `gpu.power_hundreds`. The CPU
defaults use the CPU-specific low/fraction encoding, so changing the CPU
layout requires coordinating its byte 5, byte 6, and `cpu_power_high`.
Likewise, custom mappings can bypass the intended semantic split or the
legacy CPU power policy—for example, mapping a `gpu.*` derived source into the
CPU group does not make it use CPU legacy encoding.

Each group byte is clamped to its field's range (temperature 0–99, utilization
0–100, other group bytes 0–255) and converted to an integer by truncation
toward zero; separate high bytes are rounded to the nearest integer and
clamped to 0–255. The derived split values are integer-valued already. For
derived power sources, negative values are treated as zero. Derived clock
values are limited to 0–9999 MHz before splitting.

Raw `*.power_w` and `*.clock_mhz` sources are accepted, but are not
automatically split or unit-converted. Use the corresponding derived
`power_*` or `clock_*` sources in coordinated byte positions when you want a
display-friendly split.

## CPU power encoding

The default `cpu_power_encoding = "legacy"` retains the established CPU
layout: `cpu.power_low` is whole watts in one byte, saturating at 255, and
`cpu.power_high` is the hundreds digit derived from that same capped value.
Above 255 W, the internally retained measurement does not change the
saturation; the process warns.

`cpu_power_encoding = "decimal"` is experimental. Through 255 W the legacy
byte layout is retained. Above 255 W, `cpu.power_low` becomes the watts
remainder modulo 100, `cpu.power_fraction` holds the rounded hundredths of a
watt, and `cpu.power_high` is the hundreds digit. Hardware behavior above
255 W has not been verified. GPU power splitting is independent of this
setting: its remainder and hundreds sources derive from the GPU watts.
