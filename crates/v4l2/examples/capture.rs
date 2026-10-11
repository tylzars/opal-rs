//! Captures frames and prints the rate: capture <device> <width> <height> [frames]

use std::path::Path;
use std::time::{Duration, Instant};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dev = Path::new(&args[1]);
    let (w, h): (u32, u32) = (args[2].parse().unwrap(), args[3].parse().unwrap());
    let n: u32 = args.get(4).map_or(120, |s| s.parse().unwrap());
    println!("{:?}", opal_v4l2::describe(dev)?);
    let mut cap = opal_v4l2::Capture::open(dev, w, h, opal_v4l2::NV12)?;
    println!("{:?}", cap.format);
    let start = Instant::now();
    let mut last = 0;
    for _ in 0..n {
        last = cap.next(Duration::from_secs(2), |f| {
            if f.sequence % 30 == 0 {
                let y = &f.data[..(w * h) as usize];
                let mean = y.iter().step_by(97).map(|&v| v as u64).sum::<u64>() / (y.len() / 97) as u64;
                let row = w as usize;
                let sharp = (0..h as usize - 1)
                    .step_by(7)
                    .flat_map(|r| (0..row - 1).step_by(5).map(move |c| r * row + c))
                    .map(|i| {
                        (y[i] as i32 - y[i + 1] as i32).unsigned_abs() as u64
                            + (y[i] as i32 - y[i + row] as i32).unsigned_abs() as u64
                    })
                    .sum::<u64>();
                println!("seq {} mean luma {mean} sharpness {sharp}", f.sequence);
            }
            f.sequence
        })?;
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{n} frames in {secs:.2}s = {:.1} fps (last seq {last})",
        f64::from(n) / secs
    );
    Ok(())
}
