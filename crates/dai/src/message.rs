//! Messages exchanged with pipeline nodes over XLink streams.

use crate::nop;
use crate::nop_struct;

pub const BUFFER: i32 = 0;
pub const IMG_FRAME: i32 = 1;
pub const NN_DATA: i32 = 2;
pub const CAMERA_CONTROL: i32 = 5;
pub const AUDIO_IN_CONFIG: i32 = 19;

/// Packet layout: data, libnop metadata, then the type and metadata length as LE i32.
pub fn packet(data: &[u8], kind: i32, metadata: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + metadata.len() + 8);
    out.extend_from_slice(data);
    out.extend_from_slice(metadata);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&(metadata.len() as i32).to_le_bytes());
    out
}

/// Splits a packet into (data, type, metadata).
pub fn split(packet: &[u8]) -> Option<(&[u8], i32, &[u8])> {
    let n = packet.len();
    if n < 8 {
        return None;
    }
    let kind = i32::from_le_bytes(packet[n - 8..n - 4].try_into().unwrap());
    let meta = usize::try_from(i32::from_le_bytes(packet[n - 4..].try_into().unwrap())).ok()?;
    let data_end = (n - 8).checked_sub(meta)?;
    Some((&packet[..data_end], kind, &packet[data_end..n - 8]))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManualExposure {
    pub exposure_time_us: u32,
    pub sensitivity_iso: u32,
    pub frame_duration_us: u32,
}
nop_struct!(ManualExposure {
    exposure_time_us,
    sensitivity_iso,
    frame_duration_us
});

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Region {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub priority: u32,
}
nop_struct!(Region {
    x,
    y,
    width,
    height,
    priority
});

pub mod cmd {
    pub const START_STREAM: u8 = 1;
    pub const STOP_STREAM: u8 = 2;
    pub const MOVE_LENS: u8 = 4;
    pub const AF_TRIGGER: u8 = 5;
    pub const AE_MANUAL: u8 = 6;
    pub const AE_AUTO: u8 = 7;
    pub const AWB_MODE: u8 = 8;
    pub const ANTIBANDING_MODE: u8 = 10;
    pub const EXPOSURE_COMPENSATION: u8 = 11;
    pub const AE_LOCK: u8 = 13;
    pub const AWB_LOCK: u8 = 16;
    pub const SATURATION: u8 = 28;
    pub const AF_MODE: u8 = 26;
    pub const BRIGHTNESS: u8 = 31;
    pub const SHARPNESS: u8 = 35;
    pub const CONTRAST: u8 = 44;
    pub const AE_REGION: u8 = 45;
    pub const AF_REGION: u8 = 46;
    pub const LUMA_DENOISE: u8 = 47;
    pub const CHROMA_DENOISE: u8 = 48;
    pub const WB_COLOR_TEMP: u8 = 49;
    pub const AF_LENS_RANGE: u8 = 51;
    pub const AF_LENS_LIMIT: u8 = 52;
}

pub mod af {
    pub const OFF: u8 = 0;
    pub const AUTO: u8 = 1;
    pub const MACRO: u8 = 2;
    pub const CONTINUOUS_VIDEO: u8 = 3;
    pub const CONTINUOUS_PICTURE: u8 = 4;
}

pub mod awb {
    pub const OFF: u8 = 0;
    pub const AUTO: u8 = 1;
}

pub mod antibanding {
    pub const OFF: u8 = 0;
    pub const HZ_50: u8 = 1;
    pub const HZ_60: u8 = 2;
    pub const AUTO: u8 = 3;
}

/// Camera control message. Field order is the wire order.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraControl {
    pub cmd_mask: u64,
    pub auto_focus_mode: u8,
    pub lens_position: u8,
    pub lens_pos_auto_infinity: u8,
    pub lens_pos_auto_macro: u8,
    pub lens_pos_limit_min: u8,
    pub lens_pos_limit_max: u8,
    pub exp_manual: ManualExposure,
    pub ae_region: Region,
    pub af_region: Region,
    pub awb_mode: u8,
    pub scene_mode: u8,
    pub anti_banding_mode: u8,
    pub ae_lock_mode: bool,
    pub awb_lock_mode: bool,
    pub capture_intent: u8,
    pub control_mode: u8,
    pub effect_mode: u8,
    pub frame_sync_mode: u8,
    pub exp_compensation: i8,
    pub brightness: i8,
    pub contrast: i8,
    pub saturation: i8,
    pub sharpness: u8,
    pub luma_denoise: u8,
    pub chroma_denoise: u8,
    pub wb_color_temp: u16,
    pub low_power_num_frames_burst: u8,
    pub low_power_num_frames_discard: u8,
}
nop_struct!(CameraControl {
    cmd_mask,
    auto_focus_mode,
    lens_position,
    lens_pos_auto_infinity,
    lens_pos_auto_macro,
    lens_pos_limit_min,
    lens_pos_limit_max,
    exp_manual,
    ae_region,
    af_region,
    awb_mode,
    scene_mode,
    anti_banding_mode,
    ae_lock_mode,
    awb_lock_mode,
    capture_intent,
    control_mode,
    effect_mode,
    frame_sync_mode,
    exp_compensation,
    brightness,
    contrast,
    saturation,
    sharpness,
    luma_denoise,
    chroma_denoise,
    wb_color_temp,
    low_power_num_frames_burst,
    low_power_num_frames_discard
});

impl Default for CameraControl {
    fn default() -> Self {
        Self {
            cmd_mask: 0,
            auto_focus_mode: af::CONTINUOUS_VIDEO,
            lens_position: 0,
            lens_pos_auto_infinity: 0,
            lens_pos_auto_macro: 0,
            lens_pos_limit_min: 0,
            lens_pos_limit_max: 255,
            exp_manual: ManualExposure::default(),
            ae_region: Region::default(),
            af_region: Region::default(),
            awb_mode: 0,
            scene_mode: 0,
            anti_banding_mode: 0,
            ae_lock_mode: false,
            awb_lock_mode: false,
            capture_intent: 0,
            control_mode: 0,
            effect_mode: 0,
            frame_sync_mode: 0,
            exp_compensation: 0,
            brightness: 0,
            contrast: 0,
            saturation: 0,
            sharpness: 0,
            luma_denoise: 0,
            chroma_denoise: 0,
            wb_color_temp: 0,
            low_power_num_frames_burst: 0,
            low_power_num_frames_discard: 0,
        }
    }
}

impl CameraControl {
    fn set(&mut self, c: u8) -> &mut Self {
        self.cmd_mask |= 1 << c;
        self
    }

    pub fn manual_focus(&mut self, position: u8) -> &mut Self {
        self.lens_position = position;
        self.set(cmd::MOVE_LENS)
    }

    pub fn auto_focus_mode(&mut self, mode: u8) -> &mut Self {
        self.auto_focus_mode = mode;
        self.set(cmd::AF_MODE)
    }

    pub fn auto_focus_trigger(&mut self) -> &mut Self {
        self.set(cmd::AF_TRIGGER)
    }

    pub fn auto_focus_region(&mut self, x: u16, y: u16, width: u16, height: u16) -> &mut Self {
        self.af_region = Region {
            x,
            y,
            width,
            height,
            priority: 1,
        };
        self.set(cmd::AF_REGION)
    }

    pub fn auto_focus_lens_limit(&mut self, min: u8, max: u8) -> &mut Self {
        self.lens_pos_limit_min = min;
        self.lens_pos_limit_max = max;
        self.set(cmd::AF_LENS_LIMIT)
    }

    pub fn auto_exposure(&mut self) -> &mut Self {
        self.set(cmd::AE_AUTO)
    }

    pub fn manual_exposure(&mut self, exposure_us: u32, iso: u32) -> &mut Self {
        self.exp_manual = ManualExposure {
            exposure_time_us: exposure_us,
            sensitivity_iso: iso,
            frame_duration_us: 0,
        };
        self.set(cmd::AE_MANUAL)
    }

    pub fn auto_exposure_region(&mut self, x: u16, y: u16, width: u16, height: u16) -> &mut Self {
        self.ae_region = Region {
            x,
            y,
            width,
            height,
            priority: 1,
        };
        self.set(cmd::AE_REGION)
    }

    pub fn exposure_compensation(&mut self, ev: i8) -> &mut Self {
        self.exp_compensation = ev;
        self.set(cmd::EXPOSURE_COMPENSATION)
    }

    pub fn auto_exposure_lock(&mut self, lock: bool) -> &mut Self {
        self.ae_lock_mode = lock;
        self.set(cmd::AE_LOCK)
    }

    pub fn anti_banding(&mut self, mode: u8) -> &mut Self {
        self.anti_banding_mode = mode;
        self.set(cmd::ANTIBANDING_MODE)
    }

    pub fn auto_white_balance(&mut self, mode: u8) -> &mut Self {
        self.awb_mode = mode;
        self.set(cmd::AWB_MODE)
    }

    pub fn auto_white_balance_lock(&mut self, lock: bool) -> &mut Self {
        self.awb_lock_mode = lock;
        self.set(cmd::AWB_LOCK)
    }

    pub fn white_balance(&mut self, kelvin: u16) -> &mut Self {
        self.wb_color_temp = kelvin;
        self.set(cmd::WB_COLOR_TEMP)
    }

    pub fn brightness(&mut self, v: i8) -> &mut Self {
        self.brightness = v;
        self.set(cmd::BRIGHTNESS)
    }

    pub fn contrast(&mut self, v: i8) -> &mut Self {
        self.contrast = v;
        self.set(cmd::CONTRAST)
    }

    pub fn saturation(&mut self, v: i8) -> &mut Self {
        self.saturation = v;
        self.set(cmd::SATURATION)
    }

    pub fn sharpness(&mut self, v: u8) -> &mut Self {
        self.sharpness = v;
        self.set(cmd::SHARPNESS)
    }

    pub fn luma_denoise(&mut self, v: u8) -> &mut Self {
        self.luma_denoise = v;
        self.set(cmd::LUMA_DENOISE)
    }

    pub fn chroma_denoise(&mut self, v: u8) -> &mut Self {
        self.chroma_denoise = v;
        self.set(cmd::CHROMA_DENOISE)
    }

    pub fn to_packet(&self) -> Vec<u8> {
        packet(&[], CAMERA_CONTROL, &nop::to_vec(self))
    }
}

/// Plain buffer message, e.g. for Script nodes.
pub fn buffer_packet(data: &[u8]) -> Vec<u8> {
    packet(data, BUFFER, &[])
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioInConfigData {
    pub mic_gain: f32,
    pub disable_output: bool,
    pub pass_through: bool,
}
nop_struct!(AudioInConfigData {
    mic_gain,
    disable_output,
    pass_through
});

#[derive(Clone, Debug, PartialEq)]
pub struct AudioInConfig {
    pub config: AudioInConfigData,
}
nop_struct!(AudioInConfig { config });

impl Default for AudioInConfig {
    fn default() -> Self {
        Self {
            config: AudioInConfigData {
                mic_gain: 1.0,
                disable_output: false,
                pass_through: false,
            },
        }
    }
}

impl AudioInConfig {
    pub fn gain_db(db: f32) -> Self {
        let mut c = Self::default();
        c.config.mic_gain = 10f32.powf(db / 20.0);
        c
    }

    pub fn to_packet(&self) -> Vec<u8> {
        packet(&[], AUDIO_IN_CONFIG, &nop::to_vec(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .filter(|t| *t != "|")
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    const DEFAULT_META: &str = "b9 1d 00 03 00 00 00 00 80 ff b9 03 00 00 00 b9 05 00 00 00 00 00 b9 05 00 00 00 00 00 \
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00";

    #[test]
    fn default_metadata() {
        assert_eq!(nop::to_vec(&CameraControl::default()), hex(DEFAULT_META));
    }

    #[test]
    fn manual_focus_packet() {
        let p = CameraControl::default().manual_focus(130).to_packet();
        let mut want = hex(DEFAULT_META);
        want.splice(2..5, hex("10 03 80 82"));
        want.extend(hex("05 00 00 00 31 00 00 00"));
        assert_eq!(p, want);
    }

    #[test]
    fn manual_exposure_metadata() {
        let m = nop::to_vec(CameraControl::default().manual_exposure(20000, 800));
        assert_eq!(
            &m[..20],
            &hex("b9 1d 40 03 00 00 00 00 80 ff b9 03 81 20 4e 81 20 03 00 b9")[..]
        );
    }

    #[test]
    fn audio_config_packet() {
        let mut c = AudioInConfig::default();
        c.config.mic_gain = 2.0;
        assert_eq!(
            c.to_packet(),
            hex("b9 01 b9 03 88 00 00 00 40 00 00 13 00 00 00 0b 00 00 00")
        );
    }

    #[test]
    fn split_round_trip() {
        let p = packet(&[1, 2], 7, &[9]);
        assert_eq!(split(&p), Some((&[1u8, 2][..], 7, &[9u8][..])));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TensorInfo {
    pub order: i32,
    pub data_type: i32,
    pub num_dimensions: u32,
    pub dims: Vec<u32>,
    pub strides: Vec<u32>,
    pub name: String,
    pub offset: u32,
}
nop_struct!(TensorInfo {
    order,
    data_type,
    num_dimensions,
    dims,
    strides,
    name,
    offset
});

pub const TENSOR_FP16: i32 = 0;

#[derive(Clone, Debug, PartialEq)]
pub struct NnData {
    pub tensors: Vec<TensorInfo>,
    pub batch_size: u32,
    pub sequence_num: i64,
    pub ts: crate::device::Timestamp,
    pub ts_device: crate::device::Timestamp,
}
nop_struct!(NnData {
    tensors,
    batch_size,
    sequence_num,
    ts,
    ts_device
});

impl NnData {
    /// Parses an NNData packet and returns its metadata and tensor bytes.
    pub fn parse(packet: &[u8]) -> Option<(NnData, &[u8])> {
        let (data, kind, meta) = split(packet)?;
        if kind != NN_DATA {
            return None;
        }
        Some((nop::from_slice(meta).ok()?, data))
    }

    /// The named tensor's values, converted to f32. Only FP16 tensors are supported.
    pub fn tensor_f32(&self, data: &[u8], name: &str) -> Option<Vec<f32>> {
        let t = self.tensors.iter().find(|t| t.name == name)?;
        if t.data_type != TENSOR_FP16 {
            return None;
        }
        let count = t.dims.iter().product::<u32>() as usize;
        let bytes = data.get(t.offset as usize..t.offset as usize + count * 2)?;
        Some(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| f16_to_f32(u16::from_le_bytes(*b)))
                .collect(),
        )
    }
}

/// IEEE 754 half to single precision.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if frac == 0 => sign,
        0 => {
            // subnormal: normalise
            let mut e = 127 - 15 + 1;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((f & 0x3ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | (frac << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod nn_tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    #[test]
    fn nn_data_vector() {
        let meta = hex(
            "b9 05 ba 01 b9 07 85 21 43 00 02 bc 08 01 00 00 00 0a 00 00 00 bc 08 14 00 00 00 02 00 00 00 \
             bd 03 6f 75 74 00 01 05 b9 02 00 00 b9 02 00 00",
        );
        let d: NnData = nop::from_slice(&meta).unwrap();
        assert_eq!(d.tensors[0].name, "out");
        assert_eq!(d.tensors[0].dims, [1, 10]);
        assert_eq!(d.sequence_num, 5);
    }

    #[test]
    fn half_floats() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8);
        assert_eq!(f16_to_f32(0), 0.0);
    }
}
