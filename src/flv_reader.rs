use std::io::{self, SeekFrom};

use anyhow::Result;
use async_stream::try_stream;
use deku::prelude::*;
use futures::Stream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt};

#[derive(Debug, PartialEq, DekuRead, DekuWrite)]
#[deku(magic = b"FLV\x01", endian = "big")]
pub struct FlvHeader {
    #[deku(bits = "5")]
    pub _reserved1: u8,
    #[deku(bits = "1")]
    pub has_audio: bool,
    #[deku(bits = "1")]
    pub _reserved2: u8,
    #[deku(bits = "1")]
    pub has_video: bool,
    pub data_offset: u32,
}

fn format_err(str: &'static str) -> anyhow::Error {
    anyhow::anyhow!("format error {}", str)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoFrameType {
    KeyFrame,
    InterFrame,
    DisposableInterFrame,
    GeneratedKeyFrame,
    VideoInfoOrCommandFrame,
}
fn read_frame_type(frame_type: u8) -> Result<VideoFrameType> {
    Ok(match frame_type {
        1 => VideoFrameType::KeyFrame,
        2 => VideoFrameType::InterFrame,
        3 => VideoFrameType::DisposableInterFrame,
        4 => VideoFrameType::GeneratedKeyFrame,
        5 => VideoFrameType::VideoInfoOrCommandFrame,
        _ => {
            return Err(format_err("unknown video frame type"));
        }
    })
}

#[derive(Debug, DekuRead, DekuWrite, Default)]
#[deku(endian = "big")]
pub struct FlvTag {
    pub tag_type: u8,
    #[deku(bits = 24)]
    pub data_size: u32,
    pub timestamp: u32,
    #[deku(bits = 24)]
    pub stream_id: u32,
    #[deku(skip)]
    pub data: FlvTagData,
}

#[derive(Debug)]
pub enum FlvTagData {
    Audio,
    Video { frame_type: VideoFrameType },
    Script { data: Vec<u8> },
    Other,
}

impl Default for FlvTagData {
    fn default() -> Self {
        FlvTagData::Other
    }
}

async fn read_flv_header<R: AsyncRead + AsyncSeek + Unpin>(mut reader: R) -> Result<FlvHeader> {
    reader.seek(SeekFrom::Start(0)).await?;
    let mut buf = [0u8; 9];

    reader.read_exact(&mut buf).await?;
    let (_, header) = FlvHeader::from_bytes((&buf, 0))?;

    Ok(header)
}

async fn read_flv_tag<R: AsyncRead + AsyncSeek + Unpin>(mut reader: R) -> Result<Option<FlvTag>> {
    let mut tag_header = [0u8; 15];
    match reader.read_exact(&mut tag_header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => Err(e)?,
    };

    let (_, mut tag) = FlvTag::from_bytes((&tag_header[4..], 0))?;
    let data = match tag.tag_type {
        0x8 => {
            reader.seek(SeekFrom::Current(tag.data_size as i64)).await?;
            FlvTagData::Audio
        }
        0x9 => {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf).await?;
            let frame_type = (buf[0] & 0xF0) >> 4;
            reader
                .seek(SeekFrom::Current(tag.data_size as i64 - 1))
                .await?;
            FlvTagData::Video {
                frame_type: read_frame_type(frame_type)?,
            }
        }
        0x12 => {
            let mut data = vec![0u8; tag.data_size as usize];
            reader.read_exact(&mut data).await?;

            FlvTagData::Script { data }
        }
        _ => return Err(format_err("unknown tag type")),
    };
    tag.data = data;

    Ok(Some(tag))
}

pub async fn read_flv<R: AsyncRead + AsyncSeek + Unpin>(
    mut reader: R,
) -> Result<(FlvHeader, impl Stream<Item = Result<FlvTag>>)> {
    let header = read_flv_header(&mut reader).await?;
    let data_offset = header.data_offset as u64;

    Ok((
        header,
        try_stream! {
            reader.seek(SeekFrom::Start(data_offset )).await?;

            while let Some(tag) = read_flv_tag(&mut reader).await? {
                yield tag
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flv_header() {
        let data: &[u8] = &[0x46, 0x4C, 0x56, 0x01, 0x05, 0x00, 0x00, 0x00, 0x09];

        let (_, val) = FlvHeader::from_bytes((data, 0)).unwrap();
        assert_eq!(
            val,
            FlvHeader {
                _reserved1: 0,
                _reserved2: 0,
                has_audio: true,
                has_video: true,
                data_offset: 9,
            }
        );
    }
}
