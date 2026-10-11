//! Copies frames from a capture device to an output device: passthrough <in> <out> <width> <height> [frames]

use std::path::Path;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (w, h) = (args[3].parse().unwrap(), args[4].parse().unwrap());
    let n: u32 = args.get(5).map_or(300, |s| s.parse().unwrap());
    let mut cap = opal_v4l2::Capture::open(Path::new(&args[1]), w, h, opal_v4l2::NV12)?;
    let mut out = opal_v4l2::Output::open(Path::new(&args[2]), w, h, opal_v4l2::NV12)?;
    for _ in 0..n {
        cap.next(Duration::from_secs(2), |f| out.write(f.data))??;
    }
    Ok(())
}
