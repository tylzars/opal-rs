//! The camera as an ordinary webcam: its own USB video at 1080p with no effects, until Ctrl-C.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use opal_c1::{Led, Resolution};
use opal_dai::device::Device;

pub fn run(dir: &Path, flicker: &str) -> Result<()> {
    let banding = crate::mains::mode(flicker).context("flicker is off, 50, 60 or auto")?;
    crate::on_stop()?;
    let booted = crate::boot(dir, true, false)?;
    let dev = Device::connect(&booted, |m| eprintln!("[{}] {}", m.node_id_name, m.payload))?;
    let sensor = crate::sensor_name(&dev)?;
    let pipeline = opal_c1::pipeline(dir, &sensor, Resolution::P1080, opal_c1::Mode::Webcam, banding)?;
    let streams = dev.start_pipeline(&pipeline)?;
    let (control, led) = (
        crate::daemon::stream(&streams, opal_c1::CONTROL_STREAM)?,
        crate::daemon::stream(&streams, opal_c1::LED_STREAM)?,
    );
    let link = dev.link().clone();
    link.write(control, &opal_c1::start_control().to_packet())?;
    link.write(led, &Led::White.packet())?;
    println!("sensor {sensor}: the camera is a 1080p webcam now; Ctrl-C to stop");
    while !crate::stopped() {
        if link.wait_closed(Duration::from_millis(250)) {
            anyhow::bail!("camera disconnected");
        }
    }
    let _ = link.write(control, &opal_c1::stop_control().to_packet());
    let _ = link.write(led, &Led::Off.packet());
    std::thread::sleep(crate::daemon::STOP_SETTLE);
    dev.close();
    Ok(())
}
