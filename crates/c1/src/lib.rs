//! Opal C1 camera pipeline and controls.

use std::path::Path;

pub mod faces;
pub mod hands;

use opal_dai::message::{AudioInConfig, CameraControl, af, awb, control, scene};
use opal_dai::pipeline::{
    AudioMic, ColorCamera, ImageManipConfig, IspScale, Pipeline, Processor, Uvc, orientation, resolution,
};

pub const CONTROL_STREAM: &str = "control";
pub const MIC_STREAM: &str = "micCfg";
pub const LED_STREAM: &str = "led";
pub const CONFIG_STREAM: &str = "config";
/// NV12 video frames from the camera.
pub const VIDEO_STREAM: &str = "rgb";

/// Video size, scaled by the camera from the sensor's 3840x2160 mode (2160p is the mode itself).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Resolution {
    P720,
    #[default]
    P1080,
    P1440,
    P2160,
}

impl Resolution {
    pub const ALL: [Resolution; 4] = [Self::P720, Self::P1080, Self::P1440, Self::P2160];

    pub fn size(self) -> (u32, u32) {
        match self {
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
            Self::P1440 => (2560, 1440),
            Self::P2160 => (3840, 2160),
        }
    }

    /// Sensor to video scale as (numerator, denominator).
    fn scale(self) -> (i32, i32) {
        match self {
            Self::P720 => (1, 3),
            Self::P1080 => (1, 2),
            Self::P1440 => (2, 3),
            Self::P2160 => (1, 1),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P1440 => "1440p",
            Self::P2160 => "2160p",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.name() == name)
    }
}

/// Person segmentation of the preview, FP16 [2, 144, 256]: background and person scores.
pub const SEGMENTATION_STREAM: &str = "segmentation";
/// NN config messages that pause or resume segmentation.
pub const SEGMENTATION_CONTROL_STREAM: &str = "segmentationNet";
pub const SEGMENTATION_WIDTH: u32 = 256;
pub const SEGMENTATION_HEIGHT: u32 = 144;
const SEGMENTATION_BLOB: &str = "49559974de.tlv";

/// Face detections, SSD rows of [image, label, confidence, x0, y0, x1, y1] in `detection_out`.
pub const FACE_STREAM: &str = "faces";
/// NN config messages that pause or resume face detection.
pub const FACE_CONTROL_STREAM: &str = "facesNet";
const FACE_BLOB: &str = "dd0bad95b2.tlv";
const FACE_INPUT: i32 = 300;

const PREVIEW_WIDTH: u32 = 640;
const PREVIEW_HEIGHT: u32 = 360;
/// Parts of the pipeline that are paused while not in use: a crop of the preview and the network it feeds.
/// Both are paused, as a paused network left with a running crop can hold up the other crops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    Segmentation,
    Face,
    Palm,
}

impl Branch {
    pub const ALL: [Branch; 3] = [Self::Segmentation, Self::Face, Self::Palm];

    /// Stream for NN config messages to the network.
    pub fn network_control(self) -> &'static str {
        match self {
            Self::Segmentation => SEGMENTATION_CONTROL_STREAM,
            Self::Face => FACE_CONTROL_STREAM,
            Self::Palm => hands::PALM_CONTROL_STREAM,
        }
    }

    /// Stream for configs to the crop.
    pub fn crop_control(self) -> &'static str {
        match self {
            Self::Segmentation => "segmentationCrop",
            Self::Face => "facesCrop",
            Self::Palm => "palmCrop",
        }
    }

    /// The crop's config, dropping every frame unless `active`.
    pub fn crop(self, active: bool) -> ImageManipConfig {
        let c = match self {
            Self::Segmentation => ImageManipConfig::resize(SEGMENTATION_WIDTH as i32, SEGMENTATION_HEIGHT as i32, true),
            Self::Face => ImageManipConfig::resize(FACE_INPUT, FACE_INPUT, false),
            Self::Palm => ImageManipConfig::thumbnail(hands::PALM_INPUT, hands::PALM_INPUT),
        };
        c.skipping(!active)
    }
}

/// Size of the frames the face, segmentation and hand networks work from.
pub const PREVIEW_SIZE: (u32, u32) = (PREVIEW_WIDTH, PREVIEW_HEIGHT);

/// GPIOs of the status LED's red, green and blue.
const LED_GPIOS: [i32; 3] = [58, 37, 34];

/// Script on the camera that drives the status LED from [red, green, blue] bytes, each 0 or 1.
fn led_script() -> String {
    let [r, g, b] = LED_GPIOS;
    format!(
        r#"
import GPIO

PINS = ({r}, {g}, {b})
for pin in PINS:
    GPIO.setup(pin, GPIO.OUT)
    GPIO.write(pin, 0)

while True:
    rgb = node.io["led"].get().getData()
    for pin, value in zip(PINS, rgb):
        GPIO.write(pin, 1 if value else 0)
"#
    )
}

/// Status LED states.
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

/// What the camera runs beside its video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Video to the host, with the networks behind the effects and gestures.
    Full,
    /// Low power: video to the host, no networks.
    Plain,
    /// Low power: video straight to the camera's own USB video output, no networks.
    Webcam,
}

/// Camera video, person segmentation, face detection and hand landmarks to the host over XLink, microphone into UAC,
/// with control streams. Low power modes have no networks.
pub fn pipeline(
    asset_dir: &Path,
    sensor: &str,
    res: Resolution,
    mode: Mode,
    anti_banding: u8,
) -> std::io::Result<Pipeline> {
    let (width, height) = res.size();
    let (num, den) = res.scale();
    let blob = |name: &str| std::fs::read(asset_dir.join(name));
    let mut p = Pipeline::default();
    p.set_xlink_chunk_size(0);
    let mut init = CameraControl::default();
    init.anti_banding(anti_banding)
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
        preview_width: PREVIEW_WIDTH,
        preview_height: PREVIEW_HEIGHT,
        video_width: width as i32,
        video_height: height as i32,
        resolution: resolution::K4,
        // Asking for more than 30 keeps frame times short: at 30 the camera stretches each frame past its exposure
        // (26 fps in room light against 29 at 36). Its 4K readout tops out near 37.
        fps: 36.0,
        isp_scale: if num < den {
            IspScale {
                horiz_num: num,
                horiz_den: den,
                vert_num: num,
                vert_den: den,
            }
        } else {
            IspScale::default()
        },
        ..Default::default()
    });
    if let Some(file) = tuning_file(sensor) {
        p.set_camera_tuning(blob(file)?);
    }
    let ctrl = p.xlink_in(CONTROL_STREAM, 1024);
    p.link(ctrl, "out", cam, "inputControl");
    let config = p.xlink_in(CONFIG_STREAM, 256);
    p.link(config, "out", cam, "inputConfig");

    let mic = p.audio_mic(&AudioMic::default());
    let uac = p.uac(AudioInConfig::default());
    p.link(mic, "out", uac, "input");
    let mic_cfg = p.xlink_in(MIC_STREAM, 256);
    p.link(mic_cfg, "out", mic, "inputConfig");

    if mode == Mode::Webcam {
        // The LED turns white while an app has the video open.
        let leds = |level| LED_GPIOS.into_iter().map(|g| (g, level)).collect();
        let uvc = p.uvc(&Uvc {
            gpio_init: leds(0),
            gpio_stream_on: leds(1),
            gpio_stream_off: leds(0),
        });
        p.link(cam, "video", uvc, "in");
    } else {
        let video = p.xlink_out(VIDEO_STREAM, false, 3, -1.0);
        p.link(cam, "video", video, "in");
    }
    if mode == Mode::Full {
        let (sw, sh) = (SEGMENTATION_WIDTH as i32, SEGMENTATION_HEIGHT as i32);
        let seg_size = p.image_manip(Branch::Segmentation.crop(false), sw * sh * 3, 1);
        p.link(cam, "preview", seg_size, "inputImage");
        let seg_crop = p.xlink_in(Branch::Segmentation.crop_control(), 1024);
        p.link(seg_crop, "out", seg_size, "inputConfig");
        let seg = p.neural_network(blob(SEGMENTATION_BLOB)?, 2, 2, 0);
        p.link(seg_size, "out", seg, "in");
        let seg_out = p.xlink_out(SEGMENTATION_STREAM, false, 1, 30.0);
        p.link(seg, "out", seg_out, "in");
        let seg_ctrl = p.xlink_in(SEGMENTATION_CONTROL_STREAM, 256);
        p.link(seg_ctrl, "out", seg, "inConfig");

        let face_size = p.image_manip(Branch::Face.crop(false), FACE_INPUT * FACE_INPUT * 3, 4);
        p.link(cam, "preview", face_size, "inputImage");
        let face_crop = p.xlink_in(Branch::Face.crop_control(), 1024);
        p.link(face_crop, "out", face_size, "inputConfig");
        let face = p.neural_network(blob(FACE_BLOB)?, 2, 2, 1);
        p.link(face_size, "out", face, "in");
        let face_out = p.xlink_out(FACE_STREAM, false, 1, 30.0);
        p.link(face, "out", face_out, "in");
        let face_ctrl = p.xlink_in(FACE_CONTROL_STREAM, 256);
        p.link(face_ctrl, "out", face, "inConfig");

        let (pd, lm) = (hands::PALM_INPUT, hands::LANDMARK_INPUT);
        let palm_size = p.image_manip(Branch::Palm.crop(false), pd * pd * 3, 1);
        p.link(cam, "preview", palm_size, "inputImage");
        let palm_crop = p.xlink_in(Branch::Palm.crop_control(), 1024);
        p.link(palm_crop, "out", palm_size, "inputConfig");
        let palm = p.neural_network(blob(hands::PALM_BLOB)?, 2, 1, 0);
        p.link(palm_size, "out", palm, "in");
        let palm_out = p.xlink_out(hands::PALM_STREAM, false, 1, 15.0);
        p.link(palm, "out", palm_out, "in");
        let palm_ctrl = p.xlink_in(hands::PALM_CONTROL_STREAM, 256);
        p.link(palm_ctrl, "out", palm, "inConfig");
        let hand_crop = p.image_manip_with(ImageManipConfig::resize(lm, lm, false), lm * lm * 3, 1, true);
        p.link(cam, "preview", hand_crop, "inputImage");
        let crop_in = p.xlink_in(hands::CROP_STREAM, 1024);
        p.link(crop_in, "out", hand_crop, "inputConfig");
        let landmarks = p.neural_network(blob(hands::LANDMARK_BLOB)?, 2, 1, 0);
        p.link(hand_crop, "out", landmarks, "in");
        let landmarks_out = p.xlink_out(hands::LANDMARK_STREAM, false, 2, -1.0);
        p.link(landmarks, "out", landmarks_out, "in");
    }

    let led = p.script(&led_script(), &["led"], &[], Processor::LeonMss);
    let led_in = p.xlink_in(LED_STREAM, 64);
    p.link(led_in, "out", led, "led");
    Ok(p)
}
