//! Opal C1 camera pipeline and controls.

use std::path::Path;

use opal_dai::message::{AudioInConfig, CameraControl, antibanding};
use opal_dai::pipeline::{AudioMic, ColorCamera, IspScale, Pipeline, resolution};

pub const CONTROL_STREAM: &str = "control";
pub const MIC_STREAM: &str = "micCfg";
pub const LED_STREAM: &str = "led";

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

/// Camera into UVC and microphone into UAC, with control streams.
pub fn pipeline(asset_dir: &Path, sensor: &str) -> std::io::Result<Pipeline> {
    let mut p = Pipeline::default();
    let mut control = CameraControl::default();
    control.anti_banding(antibanding::HZ_60);
    let cam = p.color_camera(&ColorCamera {
        initial_control: control,
        board_socket: 0,
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
    let uvc = p.uvc();
    p.link(cam, "video", uvc, "in");
    let ctrl = p.xlink_in(CONTROL_STREAM, 1024);
    p.link(ctrl, "out", cam, "inputControl");

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
