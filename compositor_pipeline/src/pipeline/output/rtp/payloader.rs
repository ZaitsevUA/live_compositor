use std::fmt::Debug;

use log::error;
use rand::Rng;
use rtp::codecs::{h264::H264Payloader, opus::OpusPayloader};

use crate::pipeline::{
    rtp::{AUDIO_PAYLOAD_TYPE, VIDEO_PAYLOAD_TYPE},
    structs::{EncodedChunk, EncodedChunkKind},
    AudioCodec, VideoCodec,
};

const H264_CLOCK_RATE: u32 = 90000;
const OPUS_CLOCK_RATE: u32 = 48000;

struct RtpStreamContext {
    ssrc: u32,
    next_sequence_number: u16,
}

impl RtpStreamContext {
    pub fn new() -> Self {
        let mut rng = rand::thread_rng();
        let ssrc = rng.gen::<u32>();
        let next_sequence_number = rng.gen::<u16>();

        RtpStreamContext {
            ssrc,
            next_sequence_number,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PayloadingError {
    #[error("Tried to payload video with non video payloader.")]
    NoVideoPayloader,

    #[error("Tried to payload audio with non audio payloader.")]
    NoAudioPayloader,

    #[error(
        "Tried to payload video with codec {:#?} with payloader for codec {:#?}",
        chunk_codec,
        payloader_codec
    )]
    NonMatchingVideoCodecs {
        chunk_codec: VideoCodec,
        payloader_codec: VideoCodec,
    },

    #[error(
        "Tried to payload audio with codec {:#?} with payloader for codec {:#?}",
        chunk_codec,
        payloader_codec
    )]
    NonMatchingAudioCodecs {
        chunk_codec: AudioCodec,
        payloader_codec: AudioCodec,
    },

    #[error(transparent)]
    RtpLibError(#[from] rtp::Error),
}

pub struct Payloader {
    video: Option<VideoPayloader>,
    audio: Option<AudioPayloader>,
}

enum VideoPayloader {
    H264 {
        payloader: H264Payloader,
        context: RtpStreamContext,
    },
}

enum AudioPayloader {
    Opus {
        payloader: OpusPayloader,
        context: RtpStreamContext,
    },
}

impl Payloader {
    pub fn new(video: Option<VideoCodec>, audio: Option<AudioCodec>) -> Self {
        Self {
            video: video.map(VideoPayloader::new),
            audio: audio.map(AudioPayloader::new),
        }
    }

    pub fn payload(
        &mut self,
        mtu: usize,
        data: EncodedChunk,
    ) -> Result<Vec<rtp::packet::Packet>, PayloadingError> {
        match data.kind {
            EncodedChunkKind::Video(chunk_codec) => {
                let Some(ref mut video_payloader) = self.video else {
                    return Err(PayloadingError::NoVideoPayloader);
                };

                if video_payloader.codec() != chunk_codec {
                    return Err(PayloadingError::NonMatchingVideoCodecs {
                        chunk_codec,
                        payloader_codec: video_payloader.codec(),
                    });
                }

                video_payloader.payload(mtu, data)
            }
            EncodedChunkKind::Audio(chunk_codec) => {
                let Some(ref mut audio_payloader) = self.audio else {
                    return Err(PayloadingError::NoAudioPayloader);
                };

                if audio_payloader.codec() != chunk_codec {
                    return Err(PayloadingError::NonMatchingAudioCodecs {
                        chunk_codec,
                        payloader_codec: audio_payloader.codec(),
                    });
                }

                audio_payloader.payload(mtu, data)
            }
        }
    }
}

impl VideoPayloader {
    pub fn new(codec: VideoCodec) -> Self {
        match codec {
            VideoCodec::H264 => Self::H264 {
                payloader: H264Payloader::default(),
                context: RtpStreamContext::new(),
            },
        }
    }

    pub fn codec(&self) -> VideoCodec {
        match self {
            VideoPayloader::H264 { .. } => VideoCodec::H264,
        }
    }

    pub fn payload(
        &mut self,
        mtu: usize,
        chunk: EncodedChunk,
    ) -> Result<Vec<rtp::packet::Packet>, PayloadingError> {
        match self {
            VideoPayloader::H264 {
                ref mut payloader,
                ref mut context,
            } => payload(
                payloader,
                context,
                chunk,
                mtu,
                VIDEO_PAYLOAD_TYPE,
                H264_CLOCK_RATE,
            ),
        }
    }
}

impl AudioPayloader {
    pub fn new(codec: AudioCodec) -> Self {
        match codec {
            AudioCodec::Opus => Self::Opus {
                payloader: OpusPayloader,
                context: RtpStreamContext::new(),
            },
            AudioCodec::Aac => panic!("Aac audio output is not supported yet"),
        }
    }

    pub fn codec(&self) -> AudioCodec {
        match self {
            AudioPayloader::Opus { .. } => AudioCodec::Opus,
        }
    }

    pub fn payload(
        &mut self,
        mtu: usize,
        chunk: EncodedChunk,
    ) -> Result<Vec<rtp::packet::Packet>, PayloadingError> {
        match self {
            AudioPayloader::Opus {
                ref mut payloader,
                ref mut context,
            } => payload(
                payloader,
                context,
                chunk,
                mtu,
                AUDIO_PAYLOAD_TYPE,
                OPUS_CLOCK_RATE,
            ),
        }
    }
}

fn payload<T: rtp::packetizer::Payloader>(
    payloader: &mut T,
    context: &mut RtpStreamContext,
    chunk: EncodedChunk,
    mtu: usize,
    payload_type: u8,
    clock_rate: u32,
) -> Result<Vec<rtp::packet::Packet>, PayloadingError> {
    let payloads = payloader.payload(mtu, &chunk.data)?;
    let packets_amount = payloads.len();

    Ok(payloads
        .into_iter()
        .enumerate()
        .map(|(i, payload)| {
            let header = rtp::header::Header {
                version: 2,
                padding: false,
                extension: false,
                marker: i == packets_amount - 1, // marker needs to be set on the last packet of each frame
                payload_type,
                sequence_number: context.next_sequence_number,
                timestamp: (chunk.pts.as_secs_f64() * clock_rate as f64) as u32,
                ssrc: context.ssrc,
                ..Default::default()
            };
            context.next_sequence_number = context.next_sequence_number.wrapping_add(1);

            rtp::packet::Packet { header, payload }
        })
        .collect())
}