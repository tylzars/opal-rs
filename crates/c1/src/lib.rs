//! Opal C1 camera pipeline and controls.

use std::path::Path;

use opal_dai::message::{AudioInConfig, CameraControl, af, antibanding, awb, control, scene};
use opal_dai::pipeline::{AudioMic, ColorCamera, IspScale, Pipeline, orientation, resolution};

pub const CONTROL_STREAM: &str = "control";
pub const MIC_STREAM: &str = "micCfg";
pub const LED_STREAM: &str = "led";
pub const CONFIG_STREAM: &str = "config";
/// NV12 video frames from the camera.
pub const VIDEO_STREAM: &str = "rgb";

pub const VIDEO_WIDTH: u32 = 1920;
pub const VIDEO_HEIGHT: u32 = 1080;

/// Person segmentation of the preview, FP16 [2, 144, 256]: background and person scores.
pub const SEGMENTATION_STREAM: &str = "google_meet_segmentation";
/// NN config messages that pause or resume segmentation.
pub const SEGMENTATION_CONTROL_STREAM: &str = "ctrlSegmenter";
pub const SEGMENTATION_WIDTH: u32 = 256;
pub const SEGMENTATION_HEIGHT: u32 = 144;
const SEGMENTATION_BLOB: &str = "49559974de.tlv";

/// Drives the status LED from [red, green, blue] bytes, each 0 or 1 (GPIO 58, 37, 34).
const LED_SCRIPT: &str = r#"
import GPIO

PINS = (58, 37, 34)
for pin in PINS:
    GPIO.setup(pin, GPIO.OUT)
    GPIO.write(pin, 0)

while True:
    rgb = node.io["led"].get().getData()
    for pin, value in zip(PINS, rgb):
        GPIO.write(pin, 1 if value else 0)
"#;

/// Status LED states used by Opal Composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Led {
    Off,
    White,
    Blue,
}

impl Led {
    /// Message for the LED stream.
    pub fn packet(self) -> Vec<u8> {
        let rgb: [u8; 3] = match self {
            Led::Off => [0, 0, 0],
            Led::White => [1, 1, 1],
            Led::Blue => [0, 0, 1],
        };
        opal_dai::message::buffer_packet(&rgb)
    }
}

/// Sensor tuning file for each sensor the C1 ships with.
pub fn tuning_file(sensor: &str) -> Option<&'static str> {
    match sensor {
        "IMX582" => Some("Opal582_v2.bin"),
        "IMX378" => Some("Opal_v0.12.bin"),
        _ => None,
    }
}

/// Autofocus lens position range (infinity, macro) for each sensor.
pub fn lens_range(sensor: &str) -> Option<(u8, u8)> {
    match sensor {
        "IMX582" => Some((110, 206)),
        "IMX378" => Some((120, 206)),
        _ => None,
    }
}

/// Starts streaming with continuous autofocus, auto exposure and auto white balance.
pub fn start_control() -> CameraControl {
    let mut c = CameraControl::default();
    c.start_streaming()
        .auto_focus_mode(af::CONTINUOUS_VIDEO)
        .auto_focus_trigger()
        .auto_white_balance(awb::AUTO)
        .scene_mode(scene::FACE_PRIORITY)
        .control_mode(control::USE_SCENE_MODE)
        .auto_exposure()
        .saturation(0)
        .exposure_compensation(0);
    c
}

pub fn stop_control() -> CameraControl {
    let mut c = CameraControl::default();
    c.stop_streaming();
    c
}

/// Camera video and person segmentation to the host over XLink, microphone into UAC, with control streams.
pub fn pipeline(asset_dir: &Path, sensor: &str) -> std::io::Result<Pipeline> {
    let mut p = Pipeline::default();
    let mut init = CameraControl::default();
    init.anti_banding(antibanding::HZ_60)
        .auto_exposure()
        .scene_mode(scene::FACE_PRIORITY)
        .control_mode(control::USE_SCENE_MODE);
    if let Some((infinity, macro_)) = lens_range(sensor) {
        init.auto_focus_lens_range(infinity, macro_)
            .auto_focus_lens_limit(infinity, macro_);
    }
    let cam = p.color_camera(&ColorCamera {
        initial_control: init,
        board_socket: 0,
        image_orientation: orientation::ROTATE_180,
        interleaved: false,
        preview_width: SEGMENTATION_WIDTH,
        preview_height: SEGMENTATION_HEIGHT,
        video_width: VIDEO_WIDTH as i32,
        video_height: VIDEO_HEIGHT as i32,
        resolution: resolution::K4,
        fps: 30.0,
        isp_scale: IspScale {
            horiz_num: 1,
            horiz_den: 2,
            vert_num: 1,
            vert_den: 2,
        },
        ..Default::default()
    });
    if let Some(file) = tuning_file(sensor) {
        p.set_camera_tuning(std::fs::read(asset_dir.join(file))?);
    }
    let video = p.xlink_out(VIDEO_STREAM, false, 3, -1.0);
    p.link(cam, "video", video, "in");

    let seg = p.neural_network(std::fs::read(asset_dir.join(SEGMENTATION_BLOB))?, 2, 2);
    p.link(cam, "preview", seg, "in");
    let seg_out = p.xlink_out(SEGMENTATION_STREAM, false, 1, 30.0);
    p.link(seg, "out", seg_out, "in");
    let seg_ctrl = p.xlink_in(SEGMENTATION_CONTROL_STREAM, 256);
    p.link(seg_ctrl, "out", seg, "inConfig");
    let ctrl = p.xlink_in(CONTROL_STREAM, 1024);
    p.link(ctrl, "out", cam, "inputControl");
    let config = p.xlink_in(CONFIG_STREAM, 256);
    p.link(config, "out", cam, "inputConfig");

    let mic = p.audio_mic(&AudioMic::default());
    let uac = p.uac(AudioInConfig::default());
    p.link(mic, "out", uac, "input");
    let mic_cfg = p.xlink_in(MIC_STREAM, 256);
    p.link(mic_cfg, "out", mic, "inputConfig");

    let led = p.script(LED_SCRIPT, &["led"], &[]);
    let led_in = p.xlink_in(LED_STREAM, 64);
    p.link(led_in, "out", led, "led");
    Ok(p)
}
