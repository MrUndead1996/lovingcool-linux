//! 64-byte direct-USB payload based on legacy host code and supplied static
//! analysis of the vendor application. Hardware behavior remains unverified.
pub const FRAME_LEN: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayGroup(pub [u8; 8]);

pub fn build_frame(
    cpu: DisplayGroup,
    gpu: DisplayGroup,
    cpu_power_hundreds: u8,
    gpu_power_hundreds: u8,
    time: [u8; 8],
) -> [u8; FRAME_LEN] {
    let mut frame = [0; FRAME_LEN];
    // HID report identifier is omitted for the direct USB endpoint transfer.
    frame[..3].copy_from_slice(&[0, 1, 2]);
    frame[3..11].copy_from_slice(&cpu.0);
    frame[13..21].copy_from_slice(&gpu.0);
    frame[25..33].copy_from_slice(&time);
    frame[34] = cpu_power_hundreds;
    frame[35] = gpu_power_hundreds;
    frame[36] = 0;
    frame[37] = 5;
    frame[38] = 0;
    frame[39] = 1;
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_golden_packet_uses_independent_cpu_and_gpu_power_high_bytes() {
        let frame = build_frame(
            DisplayGroup([41, 0, 0, 55, 34, 12, 12, 34]),
            DisplayGroup([71, 0, 0, 88, 67, 56, 98, 76]),
            2,
            9,
            [26, 26, 11, 16, 13, 14, 15, 1],
        );
        let expected: [u8; 64] = [
            0, 1, 2, 41, 0, 0, 55, 34, 12, 12, 34, 0, 0, 71, 0, 0, 88, 67, 56, 98, 76, 0, 0, 0, 0,
            26, 26, 11, 16, 13, 14, 15, 1, 0, 2, 9, 0, 5, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(frame, expected);
    }

    #[test]
    fn legacy_python_packet_golden_for_mirrored_237w_gpu() {
        let cpu = DisplayGroup([45, 0, 0, 82, 237, 0, 12, 34]);
        let gpu = DisplayGroup([45, 0, 0, 82, 37, 0, 12, 34]);
        let frame = build_frame(cpu, gpu, 2, 2, [20, 24, 12, 31, 23, 59, 58, 2]);
        let expected: [u8; 64] = [
            0, 1, 2, 45, 0, 0, 82, 237, 0, 12, 34, 0, 0, 45, 0, 0, 82, 37, 0, 12, 34, 0, 0, 0, 0,
            20, 24, 12, 31, 23, 59, 58, 2, 0, 2, 2, 0, 5, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(frame, expected);
    }
    #[test]
    fn boundaries_are_encoded_in_mapping_not_corrupted_by_packet_writer() {
        let frame = build_frame(
            DisplayGroup([99, 0, 0, 100, 99, 99, 99, 99]),
            DisplayGroup::default(),
            255,
            0,
            [0; 8],
        );
        assert_eq!(&frame[34..36], &[255, 0]);
        assert_eq!(frame.len(), 64);
    }
}
