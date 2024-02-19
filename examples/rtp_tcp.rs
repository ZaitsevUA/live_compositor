use anyhow::Result;
use log::{error, info};
use serde_json::json;
use std::{
    env, fs,
    process::{Command, Stdio},
    thread,
    time::Duration,
};
use video_compositor::{config::config, http, logger, types::Resolution};

use crate::common::write_example_sdp_file;

#[path = "./common/common.rs"]
mod common;

const SAMPLE_FILE_URL: &str = "https://filesamples.com/samples/video/mp4/sample_1280x720.mp4";
const SAMPLE_FILE_PATH: &str = "examples/assets/sample_1280_720.mp4";
const VIDEO_RESOLUTION: Resolution = Resolution {
    width: 1280,
    height: 720,
};

fn main() {
    env::set_var("LIVE_COMPOSITOR_WEB_RENDERER_ENABLE", "0");
    ffmpeg_next::format::network::init();
    logger::init_logger();

    thread::spawn(|| {
        if let Err(err) = start_example_client_code() {
            error!("{err}")
        }
    });

    http::Server::new(config().api_port).run();
}

fn start_example_client_code() -> Result<()> {
    info!("[example] Start listening on output port.");
    let output_sdp = write_example_sdp_file("127.0.0.1", 8002)?;
    Command::new("ffplay")
        .args(["-protocol_whitelist", "file,rtp,udp", &output_sdp])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    thread::sleep(Duration::from_secs(2));

    info!("[example] Download sample.");
    let sample_path = env::current_dir()?.join(SAMPLE_FILE_PATH);
    fs::create_dir_all(sample_path.parent().unwrap())?;
    common::ensure_downloaded(SAMPLE_FILE_URL, &sample_path)?;

    info!("[example] Send register input request.");
    common::post(&json!({
        "type": "register",
        "entity_type": "rtp_input_stream",
        "transport_protocol": "tcp_server",
        "input_id": "input_1",
        "port": 8004,
        "video": {
            "codec": "h264"
        }
    }))?;

    let shader_source = include_str!("./silly.wgsl");
    info!("[example] Register shader transform");
    common::post(&json!({
        "type": "register",
        "entity_type": "shader",
        "shader_id": "shader_example_1",
        "source": shader_source,
    }))?;

    info!("[example] Send register output request.");
    common::post(&json!({
        "type": "register",
        "entity_type": "output_stream",
        "output_id": "output_1",
        "port": 8002,
        "ip": "127.0.0.1",
        "resolution": {
            "width": VIDEO_RESOLUTION.width,
            "height": VIDEO_RESOLUTION.height,
        },
        "encoder_preset": "medium",
        "initial_scene": {
            "type": "shader",
            "id": "shader_node_1",
            "shader_id": "shader_example_1",
            "children": [
                {
                    "id": "input_1",
                    "type": "input_stream",
                    "input_id": "input_1",
                }
            ],
            "resolution": { "width": VIDEO_RESOLUTION.width, "height": VIDEO_RESOLUTION.height },
        }
    }))?;

    std::thread::sleep(Duration::from_millis(500));

    info!("[example] Start pipeline");
    common::post(&json!({
        "type": "start",
    }))?;

    let sample_path_str = sample_path.to_string_lossy().to_string();
    let gst_command = format!("gst-launch-1.0 -v funnel name=fn filesrc location={sample_path_str} ! qtdemux ! h264parse ! rtph264pay config-interval=1 pt=96  ! .send_rtp_sink rtpsession name=session .send_rtp_src ! fn. session.send_rtcp_src ! fn. fn. ! rtpstreampay ! tcpclientsink host=127.0.0.1 port=8004");
    Command::new("bash").arg("-c").arg(gst_command).spawn()?;

    Ok(())
}