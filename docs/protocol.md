# AK-03 packet and evidence notes

## Evidence hierarchy

* **Working legacy baseline:** `old/ak03.py` is user-confirmed as working on the
  AK03. Its source remains ignored/reference-only and is not copied into the
  deliverables. The default Rust layout below preserves that packet behavior
  for the same effective (already rounded/clamped) readings.
* **Vendor software static analysis:** extracted from Screen Driver Software
  v2.2.65.0, installer SHA-256
  `e26ab600fe93c595b598751285b17bfcd6199f2651dc44807f7d085e75890ce1`.
  This is analysis evidence, not direct hardware validation. Proprietary code
  and extracted source are not included here.
* **Rust USB implementation:** not yet exercised on physical hardware.

The historical Windows app writes the full 64-byte HID report. The historical
Python direct-USB code omits report ID byte 0 and pads the resulting 63 bytes to
64 bytes; Rust preserves this direct-USB behavior. Do not confuse these
transport-specific payloads.

## Standard PID 0x200b

VID/PID `5131:200b`, interface 0, interrupt OUT `0x01`, 64-byte endpoint
payload are baseline values from the legacy implementation. It builds a
64-byte report beginning `[1, 0, 1, 2]`; after removing its report ID, the
USB payload begins `[0, 1, 2]`.

| USB payload offsets | Meaning |
| --- | --- |
| 3..10 | CPU display group: temp, reserved, unit, utilization, whole-Watt power, zero fractional byte, clock hundreds, clock remainder |
| 13..20 | GPU display group: temp, reserved, unit, utilization, power modulo 100, zero fractional byte, clock hundreds, clock remainder |
| 25..32 | calendar/time fields |
| 34 | CPU power high byte (report offset 35) |
| 35 | GPU power high byte (vendor-app-derived report offset 36; legacy script duplicates the CPU high value here) |
| 36..39 | display mode/cycle/flags from legacy report offsets 37..40 |
| 63 | final zero added when padding the shifted 63-byte report |

The established legacy CPU power encoding first clamps whole watts to 255,
places that whole-Watt value in report byte 8, leaves report byte 9 zero, and
stores `power // 100` in report byte 35. Thus Rust's default CPU encoder
saturates above 255 W and retains the measurement only internally. The
`decimal` CPU policy is a hardware-untested extension only above 255 W; it is
not the default and preserves the legacy byte for readings through 255 W.
Legacy GPU telemetry was rounded to whole watts before encoding. Rust preserves
whole-Watt low-byte behavior while using the independently derived GPU
hundreds byte for powers above the old collector's clamp range.

Legacy numeric byte conversion truncates fractional temperatures/utilization
toward zero; Rust applies the same truncation when mapping those metrics to
packet bytes. Its NVML GPU power rounding uses ties-to-even to match Python's
`round()` on equivalent watt measurements.

Static analysis of the vendor app v2.2.65.0 separately reports, in the
`isLOVINGCOOL` branch, CPU report byte 8 as whole watts and report byte 35 as
`floor(watts / 100)`; GPU report bytes 18/19 as remainder and fractional
hundredths, and report byte 36 as `floor(watts / 100)`. After USB report-ID
shifting, those are payload offsets 7, 34, 17, 18, and 35 respectively.
This differs from the legacy script in some fields (notably its CPU fractional
byte stays zero and its old collector clamps input power before constructing
the report). Treat it as distinct software-derived branch evidence, not a
reason to silently change the known-working Rust default. Exact branch
selection and behavior remain subject to review and hardware testing.

## Experimental PID 0x2017

PID `0x2017` is **probe-only**. Static analysis indicates its normal subclass
uses a per-device 1000 ms send interval, while HID usage page `0x0c` (decimal 12) selects a
different packet layout. The Rust probe does not infer a send protocol from
matching endpoint descriptors; sending remains disabled pending usage-page
validation and a separately recognized profile.

No confirmed blank/clear-screen command was found. A stale metric maps to zero
individually, but that does not establish that the device clears its display.
