//! Pipeline description sent to the device, and starting it.

use std::collections::BTreeMap;
use std::thread;

use crate::device::{Device, Error, STREAM_SIZE};
use crate::message::{AudioInConfig, CameraControl};
use crate::msgpack::{self, Value};
use crate::nop::{self, EmptyArray};
use crate::{mp_map, nop_struct};

const ASSET_STREAM: &str = "__stream_asset_storage";
const ASSET_ALIGN: usize = 64;

#[derive(Clone, Copy, Debug)]
enum IoKind {
    MSender = 0,
    SReceiver = 3,
}

#[derive(Clone, Debug)]
struct Io {
    group: &'static str,
    name: String,
    kind: IoKind,
    blocking: bool,
    queue_size: i32,
    wait_for_message: bool,
}

fn input(name: &str, blocking: bool, queue_size: i32, wait: bool) -> Io {
    Io {
        group: "",
        name: name.into(),
        kind: IoKind::SReceiver,
        blocking,
        queue_size,
        wait_for_message: wait,
    }
}

fn output(name: &str) -> Io {
    Io {
        group: "",
        name: name.into(),
        kind: IoKind::MSender,
        blocking: false,
        queue_size: 8,
        wait_for_message: false,
    }
}

struct Node {
    name: &'static str,
    properties: Vec<u8>,
    io: Vec<Io>,
    assets: Vec<(String, Vec<u8>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeId(i64);

struct Link {
    from: NodeId,
    output: String,
    to: NodeId,
    input: String,
}

#[derive(Default)]
pub struct Pipeline {
    nodes: Vec<Node>,
    links: Vec<Link>,
    tuning: Option<Vec<u8>>,
    xlink_in: Vec<String>,
    xlink_out: Vec<String>,
    /// None leaves the device default.
    xlink_chunk_size: Option<i32>,
}

// Node properties, in wire order.

#[derive(Clone, Debug, Default)]
pub struct IspScale {
    pub horiz_num: i32,
    pub horiz_den: i32,
    pub vert_num: i32,
    pub vert_den: i32,
}
nop_struct!(IspScale {
    horiz_num,
    horiz_den,
    vert_num,
    vert_den
});

pub mod resolution {
    pub const P1080: i32 = 0;
    pub const P1200: i32 = 1;
    pub const K4: i32 = 2;
    pub const MP5: i32 = 3;
    pub const MP12: i32 = 4;
    pub const MP13: i32 = 5;
    pub const P720: i32 = 6;
    pub const P800: i32 = 7;
}

/// Device processors that run scripts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Processor {
    LeonCss = 0,
    LeonMss = 1,
}

pub mod orientation {
    pub const AUTO: i32 = -1;
    pub const NORMAL: i32 = 0;
    pub const HORIZONTAL_MIRROR: i32 = 1;
    pub const VERTICAL_FLIP: i32 = 2;
    pub const ROTATE_180: i32 = 3;
}

#[derive(Clone, Debug)]
pub struct ColorCamera {
    pub initial_control: CameraControl,
    pub board_socket: i32,
    pub image_orientation: i32,
    pub color_order: i32,
    pub interleaved: bool,
    pub fp16: bool,
    pub preview_height: u32,
    pub preview_width: u32,
    pub video_width: i32,
    pub video_height: i32,
    pub still_width: i32,
    pub still_height: i32,
    pub resolution: i32,
    pub fps: f32,
    pub sensor_crop_x: f32,
    pub sensor_crop_y: f32,
    pub preview_keep_aspect_ratio: bool,
    pub raw_stream_metadata_only: bool,
    pub raw_pdaf_windows_8x6: bool,
    pub pd_off_x: u16,
    pub pd_off_y: u16,
    pub pd_win_w: u16,
    pub pd_win_h: u16,
    pub isp_scale: IspScale,
}
nop_struct!(ColorCamera {
    initial_control,
    board_socket,
    image_orientation,
    color_order,
    interleaved,
    fp16,
    preview_height,
    preview_width,
    video_width,
    video_height,
    still_width,
    still_height,
    resolution,
    fps,
    sensor_crop_x,
    sensor_crop_y,
    preview_keep_aspect_ratio,
    raw_stream_metadata_only,
    raw_pdaf_windows_8x6,
    pd_off_x,
    pd_off_y,
    pd_win_w,
    pd_win_h,
    isp_scale
});

impl Default for ColorCamera {
    fn default() -> Self {
        Self {
            initial_control: CameraControl::default(),
            board_socket: -1,
            image_orientation: orientation::AUTO,
            color_order: 0,
            interleaved: true,
            fp16: false,
            preview_height: 300,
            preview_width: 300,
            video_width: -1,
            video_height: -1,
            still_width: -1,
            still_height: -1,
            resolution: resolution::P1080,
            fps: 30.0,
            sensor_crop_x: -1.0,
            sensor_crop_y: -1.0,
            preview_keep_aspect_ratio: true,
            raw_stream_metadata_only: false,
            raw_pdaf_windows_8x6: false,
            pd_off_x: 0,
            pd_off_y: 0,
            pd_win_w: 0,
            pd_win_h: 0,
            isp_scale: IspScale::default(),
        }
    }
}

struct XLinkIn {
    stream_name: String,
    max_data_size: u32,
    num_frames: u32,
}
nop_struct!(XLinkIn {
    stream_name,
    max_data_size,
    num_frames
});

struct XLinkOut {
    max_fps_limit: f32,
    stream_name: String,
    metadata_only: bool,
}
nop_struct!(XLinkOut {
    max_fps_limit,
    stream_name,
    metadata_only
});

/// GPIO levels the UVC node sets at start and when the host starts or stops streaming.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Uvc {
    pub gpio_init: BTreeMap<i32, i32>,
    pub gpio_stream_on: BTreeMap<i32, i32>,
    pub gpio_stream_off: BTreeMap<i32, i32>,
}
nop_struct!(Uvc {
    gpio_init,
    gpio_stream_on,
    gpio_stream_off
});

struct Script {
    script_uri: String,
    script_name: String,
    processor: i32,
}
nop_struct!(Script {
    script_uri,
    script_name,
    processor
});

#[derive(Clone, Debug, Default, PartialEq)]
struct Point2f {
    x: f32,
    y: f32,
}
nop_struct!(Point2f { x, y });

#[derive(Clone, Debug, Default, PartialEq)]
struct Size2f {
    width: f32,
    height: f32,
}
nop_struct!(Size2f { width, height });

#[derive(Clone, Debug, Default, PartialEq)]
struct RotatedRect {
    center: Point2f,
    size: Size2f,
    angle: f32,
}
nop_struct!(RotatedRect { center, size, angle });

#[derive(Clone, Debug, Default, PartialEq)]
struct CropRect {
    xmin: f32,
    ymin: f32,
    xmax: f32,
    ymax: f32,
}
nop_struct!(CropRect { xmin, ymin, xmax, ymax });

#[derive(Clone, Debug, PartialEq)]
struct CropConfig {
    crop_rect: CropRect,
    crop_rotated_rect: RotatedRect,
    enable_center_crop_rectangle: bool,
    crop_ratio: f32,
    width_height_aspect_ratio: f32,
    enable_rotated_rect: bool,
    normalized_coords: bool,
}
nop_struct!(CropConfig {
    crop_rect,
    crop_rotated_rect,
    enable_center_crop_rectangle,
    crop_ratio,
    width_height_aspect_ratio,
    enable_rotated_rect,
    normalized_coords
});

impl Default for CropConfig {
    fn default() -> Self {
        Self {
            crop_rect: CropRect::default(),
            crop_rotated_rect: RotatedRect::default(),
            enable_center_crop_rectangle: false,
            crop_ratio: 1.0,
            width_height_aspect_ratio: 1.0,
            enable_rotated_rect: false,
            normalized_coords: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ResizeConfig {
    width: i32,
    height: i32,
    lock_aspect_ratio_fill: bool,
    bg_red: u8,
    bg_green: u8,
    bg_blue: u8,
    warp_four_points: EmptyArray,
    normalized_coords: bool,
    enable_warp_4pt: bool,
    warp_matrix_3x3: EmptyArray,
    enable_warp_matrix: bool,
    warp_border_replicate: bool,
    rotation_angle_deg: f32,
    enable_rotation: bool,
    keep_aspect_ratio: bool,
}
nop_struct!(ResizeConfig {
    width,
    height,
    lock_aspect_ratio_fill,
    bg_red,
    bg_green,
    bg_blue,
    warp_four_points,
    normalized_coords,
    enable_warp_4pt,
    warp_matrix_3x3,
    enable_warp_matrix,
    warp_border_replicate,
    rotation_angle_deg,
    enable_rotation,
    keep_aspect_ratio
});

impl Default for ResizeConfig {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            lock_aspect_ratio_fill: false,
            bg_red: 0,
            bg_green: 0,
            bg_blue: 0,
            warp_four_points: EmptyArray,
            normalized_coords: true,
            enable_warp_4pt: false,
            warp_matrix_3x3: EmptyArray,
            enable_warp_matrix: false,
            warp_border_replicate: false,
            rotation_angle_deg: 0.0,
            enable_rotation: false,
            keep_aspect_ratio: true,
        }
    }
}

/// Frame type that keeps the input's type.
const FRAME_TYPE_NONE: i32 = 32;

#[derive(Clone, Debug, PartialEq)]
struct FormatConfig {
    frame_type: i32,
    flip_horizontal: bool,
    flip_vertical: bool,
}
nop_struct!(FormatConfig {
    frame_type,
    flip_horizontal,
    flip_vertical
});

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            frame_type: FRAME_TYPE_NONE,
            flip_horizontal: false,
            flip_vertical: false,
        }
    }
}

/// What an ImageManip node does to each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImageManipConfig {
    crop: CropConfig,
    resize: ResizeConfig,
    format: FormatConfig,
    enable_crop: bool,
    enable_resize: bool,
    enable_format: bool,
    reuse_previous_image: bool,
    skip_current_image: bool,
}
nop_struct!(ImageManipConfig {
    crop,
    resize,
    format,
    enable_crop,
    enable_resize,
    enable_format,
    reuse_previous_image,
    skip_current_image
});

impl ImageManipConfig {
    /// Crops a rotated rectangle, centre and size normalised to the frame, angle in degrees clockwise, and resizes
    /// it to `width` x `height`.
    pub fn rotated_crop(center: (f32, f32), size: (f32, f32), angle: f32, width: i32, height: i32) -> Self {
        let mut c = Self::resize(width, height, false);
        c.crop.crop_rotated_rect = RotatedRect {
            center: Point2f {
                x: center.0,
                y: center.1,
            },
            size: Size2f {
                width: size.0,
                height: size.1,
            },
            angle,
        };
        c.crop.enable_rotated_rect = true;
        c.crop.normalized_coords = true;
        c.enable_crop = true;
        c
    }

    pub fn to_packet(&self) -> Vec<u8> {
        crate::message::packet(&[], crate::message::IMAGE_MANIP_CONFIG, &nop::to_vec(self))
    }

    /// With `skip`, every frame is dropped until another config arrives.
    pub fn skipping(mut self, skip: bool) -> Self {
        self.skip_current_image = skip;
        self
    }

    /// Fits the frame into `width` x `height`, padding the rest with black.
    pub fn thumbnail(width: i32, height: i32) -> Self {
        let mut c = Self::resize(width, height, true);
        c.resize.lock_aspect_ratio_fill = true;
        c
    }

    /// Resizes to `width` x `height`, letterboxing when `keep_aspect` is set, stretching otherwise.
    pub fn resize(width: i32, height: i32, keep_aspect: bool) -> Self {
        let mut c = Self::default();
        c.resize.width = width;
        c.resize.height = height;
        c.resize.keep_aspect_ratio = keep_aspect;
        c.enable_resize = true;
        c
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ImageManipProps {
    initial_config: ImageManipConfig,
    output_frame_size: i32,
    num_frames_pool: i32,
    mesh_width: i32,
    mesh_height: i32,
    mesh_uri: String,
}
nop_struct!(ImageManipProps {
    initial_config,
    output_frame_size,
    num_frames_pool,
    mesh_width,
    mesh_height,
    mesh_uri
});

impl Default for ImageManipProps {
    fn default() -> Self {
        Self {
            initial_config: ImageManipConfig::default(),
            output_frame_size: 1 << 20,
            num_frames_pool: 4,
            mesh_width: 0,
            mesh_height: 0,
            mesh_uri: String::new(),
        }
    }
}

struct NeuralNetwork {
    blob_size: Option<u32>,
    blob_uri: String,
    num_frames: u32,
    num_threads: u32,
    num_nce_per_thread: u32,
    num_frames_to_skip: i32,
}
nop_struct!(NeuralNetwork {
    blob_size,
    blob_uri,
    num_frames,
    num_threads,
    num_nce_per_thread,
    num_frames_to_skip
});

#[derive(Clone, Debug)]
pub struct AudioMic {
    pub initial_config: AudioInConfig,
    pub stream_back_mic: bool,
    pub enable_agc: bool,
    pub sample_rate_hz: u32,
    pub i2s_read_size_samples: u32,
    pub xlink_send_size_samples: u32,
    pub xlink_send_interleaved: bool,
    pub xlink_apply_mic_gain: bool,
    pub xlink_sample_size_bytes: u8,
}
nop_struct!(AudioMic {
    initial_config,
    stream_back_mic,
    enable_agc,
    sample_rate_hz,
    i2s_read_size_samples,
    xlink_send_size_samples,
    xlink_send_interleaved,
    xlink_apply_mic_gain,
    xlink_sample_size_bytes
});

impl Default for AudioMic {
    fn default() -> Self {
        Self {
            initial_config: AudioInConfig::default(),
            stream_back_mic: false,
            enable_agc: false,
            sample_rate_hz: 48000,
            i2s_read_size_samples: 1440,
            xlink_send_size_samples: 1440,
            xlink_send_interleaved: true,
            xlink_apply_mic_gain: true,
            xlink_sample_size_bytes: 3,
        }
    }
}

struct Uac {
    initial_config: AudioInConfig,
}
nop_struct!(Uac { initial_config });

impl Pipeline {
    fn add(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        NodeId(self.nodes.len() as i64 - 1)
    }

    pub fn color_camera(&mut self, props: &ColorCamera) -> NodeId {
        self.add(Node {
            name: "ColorCamera",
            properties: nop::to_vec(props),
            io: vec![
                input("inputConfig", false, 1, false),
                input("inputControl", true, 8, false),
                output("video"),
                output("preview"),
                output("still"),
                output("isp"),
                output("raw"),
            ],
            assets: vec![],
        })
    }

    /// Stream the host writes messages to. Output "out".
    pub fn xlink_in(&mut self, stream: &str, max_data_size: u32) -> NodeId {
        self.xlink_in.push(stream.into());
        let props = XLinkIn {
            stream_name: stream.into(),
            max_data_size,
            num_frames: 8,
        };
        self.add(Node {
            name: "XLinkIn",
            properties: nop::to_vec(&props),
            io: vec![output("out")],
            assets: vec![],
        })
    }

    /// Stream the host reads messages from. Input "in". `max_fps` -1 means no limit.
    pub fn xlink_out(&mut self, stream: &str, blocking: bool, queue_size: i32, max_fps: f32) -> NodeId {
        self.xlink_out.push(stream.into());
        let props = XLinkOut {
            max_fps_limit: max_fps,
            stream_name: stream.into(),
            metadata_only: false,
        };
        self.add(Node {
            name: "XLinkOut",
            properties: nop::to_vec(&props),
            io: vec![input("in", blocking, queue_size, true)],
            assets: vec![],
        })
    }

    /// USB video output. Input "in".
    pub fn uvc(&mut self, props: &Uvc) -> NodeId {
        self.add(Node {
            name: "UVC",
            properties: nop::to_vec(props),
            io: vec![input("in", true, 8, true)],
            assets: vec![],
        })
    }

    /// Microphone. Input "inputConfig", outputs "out" and "outBack".
    pub fn audio_mic(&mut self, props: &AudioMic) -> NodeId {
        self.add(Node {
            name: "AudioMic",
            properties: nop::to_vec(props),
            io: vec![input("inputConfig", false, 4, false), output("out"), output("outBack")],
            assets: vec![],
        })
    }

    /// USB audio output. Inputs "inputConfig" and "input".
    pub fn uac(&mut self, initial_config: AudioInConfig) -> NodeId {
        self.add(Node {
            name: "UAC",
            properties: nop::to_vec(&Uac { initial_config }),
            io: vec![input("inputConfig", false, 4, false), input("input", false, 4, false)],
            assets: vec![],
        })
    }

    /// Python script on one of the device's processors. `inputs` and `outputs` are the names in its `node.io` map.
    pub fn script(&mut self, source: &str, inputs: &[&str], outputs: &[&str], processor: Processor) -> NodeId {
        let props = Script {
            script_uri: "asset:__script".into(),
            script_name: "<script>".into(),
            processor: processor as i32,
        };
        let mut io: Vec<Io> = inputs
            .iter()
            .map(|n| Io {
                group: "io",
                ..input(n, true, 8, false)
            })
            .collect();
        io.extend(outputs.iter().map(|n| Io {
            group: "io",
            ..output(n)
        }));
        self.add(Node {
            name: "Script",
            properties: nop::to_vec(&props),
            io,
            assets: vec![("__script".into(), source.as_bytes().to_vec())],
        })
    }

    /// Resizes or crops frames. Inputs "inputImage" (dropped when busy) and "inputConfig", output "out".
    pub fn image_manip(&mut self, config: ImageManipConfig, max_output_size: i32, image_queue: i32) -> NodeId {
        self.image_manip_with(config, max_output_size, image_queue, false)
    }

    /// Like `image_manip`; with `wait_for_config` each frame waits for a config on "inputConfig".
    pub fn image_manip_with(
        &mut self,
        config: ImageManipConfig,
        max_output_size: i32,
        image_queue: i32,
        wait_for_config: bool,
    ) -> NodeId {
        let props = ImageManipProps {
            initial_config: config,
            output_frame_size: max_output_size,
            ..Default::default()
        };
        self.add(Node {
            name: "ImageManip",
            properties: nop::to_vec(&props),
            io: vec![
                input("inputConfig", true, 8, wait_for_config),
                input("inputImage", false, image_queue, true),
                output("out"),
            ],
            assets: vec![],
        })
    }

    /// Neural network. Inputs "in" (frames, dropped when busy) and "inConfig", outputs "out" and "passthrough".
    pub fn neural_network(&mut self, blob: Vec<u8>, pool_frames: u32, num_threads: u32, nce_per_thread: u32) -> NodeId {
        let props = NeuralNetwork {
            blob_size: Some(blob.len() as u32),
            blob_uri: "asset:__blob".into(),
            num_frames: pool_frames,
            num_threads,
            num_nce_per_thread: nce_per_thread,
            num_frames_to_skip: 0,
        };
        self.add(Node {
            name: "NeuralNetwork",
            properties: nop::to_vec(&props),
            io: vec![
                input("in", false, 1, true),
                input("inConfig", false, 5, false),
                output("out"),
                output("passthrough"),
            ],
            assets: vec![("__blob".into(), blob)],
        })
    }

    pub fn set_camera_tuning(&mut self, blob: Vec<u8>) {
        self.tuning = Some(blob);
    }

    /// Size the device splits XLink writes into; 0 sends each message in one piece.
    pub fn set_xlink_chunk_size(&mut self, bytes: i32) {
        self.xlink_chunk_size = Some(bytes);
    }

    pub fn link(&mut self, from: NodeId, output: &str, to: NodeId, input: &str) {
        self.links.push(Link {
            from,
            output: output.into(),
            to,
            input: input.into(),
        });
    }

    fn schema(&self) -> Value {
        let connections = self
            .links
            .iter()
            .map(|l| {
                mp_map! {
                    "node1Id" => l.from.0, "node1Output" => l.output.as_str(), "node1OutputGroup" => io_group(self, l.from, &l.output),
                    "node2Id" => l.to.0, "node2Input" => l.input.as_str(), "node2InputGroup" => io_group(self, l.to, &l.input),
                }
            })
            .collect::<Vec<_>>();
        let global = mp_map! {
            "calibData" => Value::Nil,
            "cameraTuningBlobSize" => self.tuning.as_ref().map(|t| t.len() as u32),
            "cameraTuningBlobUri" => if self.tuning.is_some() { "asset:camTuning" } else { "" },
            "imageManipAdjustCmxSize" => 0i32,
            "leonCssFrequencyHz" => 700e6f64,
            "leonMssFrequencyHz" => 700e6f64,
            "pipelineName" => Value::Nil,
            "pipelineVersion" => Value::Nil,
            "xlinkChunkSize" => self.xlink_chunk_size.unwrap_or(-1),
        };
        let nodes = self
            .nodes
            .iter()
            .enumerate()
            .map(|(id, n)| {
                let io = n
                    .io
                    .iter()
                    .map(|io| {
                        Value::Array(vec![
                            Value::Array(vec![io.group.into(), io.name.as_str().into()]),
                            mp_map! {
                                "blocking" => io.blocking, "group" => io.group, "name" => io.name.as_str(),
                                "queueSize" => io.queue_size, "type" => io.kind as i32, "waitForMessage" => io.wait_for_message,
                            },
                        ])
                    })
                    .collect::<Vec<_>>();
                Value::Array(vec![
                    (id as i64).into(),
                    mp_map! { "id" => id as i64, "ioInfo" => io, "name" => n.name, "properties" => msgpack::bytes(&n.properties) },
                ])
            })
            .collect::<Vec<_>>();
        mp_map! { "connections" => connections, "globalProperties" => global, "nodes" => nodes }
    }

    /// Lays out every asset in one 64-byte aligned block. Returns (map, storage).
    fn assets(&self) -> (Value, Vec<u8>) {
        let mut entries: Vec<(String, &[u8])> = Vec::new();
        if let Some(t) = &self.tuning {
            entries.push(("/pipeline/camTuning".into(), t));
        }
        for (id, n) in self.nodes.iter().enumerate() {
            let mut a: Vec<_> = n.assets.iter().collect();
            a.sort_by(|x, y| x.0.cmp(&y.0));
            entries.extend(a.into_iter().map(|(k, d)| (format!("/node/{id}/{k}"), d.as_slice())));
        }
        let mut storage = Vec::new();
        let mut map = BTreeMap::new();
        for (key, data) in entries {
            storage.resize(storage.len().div_ceil(ASSET_ALIGN) * ASSET_ALIGN, 0);
            map.insert(
                key,
                mp_map! { "alignment" => ASSET_ALIGN as u32, "offset" => storage.len() as u32, "size" => data.len() as u32 },
            );
            storage.extend_from_slice(data);
        }
        (mp_map! { "map" => Value::Map(map) }, storage)
    }

    /// Streams the host should open, as (name, is_input).
    pub fn streams(&self) -> impl Iterator<Item = (&str, bool)> {
        self.xlink_in
            .iter()
            .map(|s| (s.as_str(), true))
            .chain(self.xlink_out.iter().map(|s| (s.as_str(), false)))
    }
}

fn io_group(p: &Pipeline, node: NodeId, name: &str) -> &'static str {
    p.nodes[node.0 as usize]
        .io
        .iter()
        .find(|io| io.name == name)
        .map_or("", |io| io.group)
}

impl Device {
    /// Sends `pipeline` and starts it. Returns the opened host streams.
    pub fn start_pipeline(&self, pipeline: &Pipeline) -> Result<BTreeMap<String, opal_xlink::StreamId>, Error> {
        if self.call("isPipelineRunning", vec![])?.as_bool() == Some(true) {
            return Err(Error::Remote("pipeline already running".into()));
        }
        let mut streams = BTreeMap::new();
        for (name, is_input) in pipeline.streams() {
            let id = self.link().open_stream(name, if is_input { STREAM_SIZE } else { 1 })?;
            streams.insert(name.to_owned(), id);
        }
        self.call("setPipelineSchema", vec![pipeline.schema()])?;

        let (map, storage) = pipeline.assets();
        if !storage.is_empty() {
            self.call("setAssets", vec![map])?;
            let len = storage.len() as u32;
            let handle = self
                .call("memAlloc", vec![len.into()])?
                .as_u64()
                .ok_or(Error::BadResponse)?;
            let link = self.link().clone();
            let writer = thread::spawn(move || -> Result<(), opal_xlink::Error> {
                let s = link.open_stream(ASSET_STREAM, STREAM_SIZE)?;
                link.write_split(s, &storage, STREAM_SIZE as usize)
            });
            let read = self.call(
                "readFromXLink",
                vec![ASSET_STREAM.into(), handle.into(), (len as u64).into()],
            );
            writer.join().expect("asset writer")?;
            read?;
            self.call("setAssetStorage", vec![handle.into(), (len as u64).into()])?;
        }

        self.call("printAssets", vec![])?;
        let built = self.call("buildPipeline", vec![])?;
        let built = built.as_array().ok_or(Error::BadResponse)?;
        if built.first().and_then(Value::as_bool) != Some(true) {
            return Err(Error::Remote(
                built
                    .get(1)
                    .and_then(Value::as_str)
                    .unwrap_or("build failed")
                    .to_owned(),
            ));
        }
        self.call("startPipeline", vec![])?;
        Ok(streams)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    #[test]
    fn color_camera_default_bytes() {
        let want = hex(
            "b9 18 b9 1d 00 03 00 00 00 00 80 ff b9 03 00 00 00 b9 05 00 00 00 00 00 b9 05 00 00 00 00 00 00 00 00 00 \
             00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 ff ff 00 01 00 81 2c 01 81 2c 01 ff ff ff ff 00 88 00 00 f0 \
             41 88 00 00 80 bf 88 00 00 80 bf 01 00 00 00 00 00 00 b9 04 00 00 00 00",
        );
        assert_eq!(nop::to_vec(&ColorCamera::default()), want);
    }

    #[test]
    fn small_node_bytes() {
        let x = XLinkIn {
            stream_name: "ctrl".into(),
            max_data_size: 5 << 20,
            num_frames: 8,
        };
        assert_eq!(nop::to_vec(&x), hex("b9 03 bd 04 63 74 72 6c 82 00 00 50 00 08"));
        let o = XLinkOut {
            max_fps_limit: -1.0,
            stream_name: "rgb".into(),
            metadata_only: false,
        };
        assert_eq!(nop::to_vec(&o), hex("b9 03 88 00 00 80 bf bd 03 72 67 62 00"));
        let u = Uvc {
            gpio_init: BTreeMap::new(),
            gpio_stream_on: BTreeMap::new(),
            gpio_stream_off: BTreeMap::new(),
        };
        assert_eq!(nop::to_vec(&u), hex("b9 03 bb 00 bb 00 bb 00"));
        assert_eq!(
            nop::to_vec(&ImageManipProps::default()),
            hex(
                "b9 06 b9 08 b9 07 b9 04 88 00 00 00 00 88 00 00 00 00 88 00 00 00 00 88 00 00 00 00 b9 03 b9 02 \
                 88 00 00 00 00 88 00 00 00 00 b9 02 88 00 00 00 00 88 00 00 00 00 88 00 00 00 00 00 88 00 00 80 \
                 3f 88 00 00 80 3f 00 01 b9 0f 00 00 00 00 00 00 ba 00 01 00 ba 00 00 00 88 00 00 00 00 00 01 b9 \
                 03 20 00 00 00 00 00 00 00 86 00 00 10 00 04 00 00 bd 00"
            )
        );
        let resize = nop::to_vec(&ImageManipConfig::resize(300, 300, false));
        let at = resize.windows(2).position(|w| w == [0xb9, 0x0f]).unwrap();
        assert_eq!(&resize[at..at + 8], hex("b9 0f 85 2c 01 85 2c 01"));
        assert_eq!(resize[at + 26], 0, "keep aspect ratio off");
        let keep = nop::to_vec(&ImageManipConfig::resize(256, 144, true));
        let at = keep.windows(2).position(|w| w == [0xb9, 0x0f]).unwrap();
        assert_eq!(keep[at + 26], 1);
        let nn = NeuralNetwork {
            blob_size: Some(1_000_000),
            blob_uri: "asset:__blob".into(),
            num_frames: 8,
            num_threads: 0,
            num_nce_per_thread: 0,
            num_frames_to_skip: 0,
        };
        assert_eq!(
            nop::to_vec(&nn),
            hex("b9 06 82 40 42 0f 00 bd 0c 61 73 73 65 74 3a 5f 5f 62 6c 6f 62 08 00 00 00")
        );
        let u = Uvc {
            gpio_init: BTreeMap::from([(58, 1)]),
            ..Default::default()
        };
        assert_eq!(nop::to_vec(&u), hex("b9 03 bb 01 3a 01 bb 00 bb 00"));
        assert_eq!(
            nop::to_vec(&AudioMic::default()),
            hex("b9 09 b9 01 b9 03 88 00 00 80 3f 00 00 00 00 81 80 bb 81 a0 05 81 a0 05 01 01 03")
        );
        assert_eq!(
            nop::to_vec(&Uac {
                initial_config: AudioInConfig::default()
            }),
            hex("b9 01 b9 01 b9 03 88 00 00 80 3f 00 00")
        );
    }

    #[test]
    fn asset_layout_is_aligned() {
        let mut p = Pipeline::default();
        p.set_camera_tuning(vec![1; 10]);
        p.script("x", &[], &[], Processor::LeonMss);
        let (_, storage) = p.assets();
        assert_eq!(storage.len(), 64 + 1);
        assert_eq!(storage[64], b'x');
    }
}
