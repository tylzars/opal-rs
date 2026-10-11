//! Opal C1 camera pipeline and controls.

use std::path::Path;

use opal_dai::message::{CameraControl, antibanding};
use opal_dai::pipeline::{ColorCamera, IspScale, Pipeline, resolution};

pub const CONTROL_STREAM: &str = "control";

/// Sensor tuning file for each sensor the C1 ships with.
pub fn tuning_file(sensor: &str) -> Option<&'static str> {
    match sensor {
        "IMX582" => Some("Opal582_v2.bin"),
        "IMX378" => Some("Opal_v0.12.bin"),
        _ => None,
    }
}

/// Camera into UVC, with a control stream.
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
    Ok(p)
}
