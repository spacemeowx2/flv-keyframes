use crate::keyframes::Keyframes;
use crate::patch::Patch;
use amf::amf0;
use anyhow::Result;
use bytecodec::{
    io::{IoDecodeExt, IoEncodeExt, ReadBuf},
    Decode, Encode,
};
use bytes::{Buf, BufMut};
use flv_codec::{
    FileDecoder, FrameType, ScriptDataTag, StreamId, Tag, TagEncoder, Timestamp, VideoTag,
};
use std::io::{Cursor, SeekFrom};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
    task,
};

fn has_keyframes(v: amf0::Value) -> bool {
    match v.try_into_pairs() {
        Ok(mut iter) => iter.any(|i| i.0 == "keyframes"),
        Err(_) => false,
    }
}

fn insert_keyframes(metadata: amf0::Value, keyframes: Keyframes) -> amf0::Value {
    fn map_amf0((key, value): (String, amf::Value)) -> (String, amf0::Value) {
        (
            key,
            match value {
                amf::Value::Amf0(v) => v,
                _ => unreachable!(),
            },
        )
    }
    let keyframes = std::iter::once(keyframes.into_amf0());
    let value = metadata
        .try_into_pairs()
        .map(|i| amf0::object(i.map(map_amf0).chain(keyframes)));
    match value {
        Ok(v) => v,
        Err(v) => v,
    }
}

fn make_patched(metadata: amf0::Value) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::<u8>::new());
    amf0::string("onMetaData").write_to(&mut buf).unwrap();
    metadata.write_to(&mut buf).unwrap();
    let mut encoder = TagEncoder::<Vec<u8>>::new();
    let mut out: Vec<u8> = Vec::new();
    encoder
        .start_encoding(Tag::ScriptData(ScriptDataTag {
            timestamp: Timestamp::new(0),
            stream_id: StreamId::new(0).unwrap(),
            data: buf.into_inner(),
        }))
        .unwrap();
    encoder.encode_all(&mut out).unwrap();
    let prev_size = out.len();
    let mut tail = Vec::new();
    tail.put_u32(prev_size as u32);

    out.append(&mut tail);
    out
}

fn format_err() -> Result<()> {
    Err(anyhow::anyhow!("format error"))
}

async fn read_flv_header(file: &mut File) -> Result<()> {
    file.seek(SeekFrom::Start(0)).await?;
    let mut buf = [0u8; 9];
    file.read_exact(&mut buf).await?;
    // TODO: check flv header
    Ok(())
}

struct FlvTag {
    data_size: u32,
    stream_id: u32,
    timestamp: u32,
    data: FlvTagData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum VideoFrameType {
    KeyFrame,
    InterFrame,
    DisposableInterFrame,
    GeneratedKeyFrame,
    VideoInfoOrCommandFrame,
}
fn read_frame_type(frame_type: u32) -> Result<VideoFrameType> {
    Ok(match frame_type {
        1 => VideoFrameType::KeyFrame,
        2 => VideoFrameType::InterFrame,
        3 => VideoFrameType::DisposableInterFrame,
        4 => VideoFrameType::GeneratedKeyFrame,
        5 => VideoFrameType::VideoInfoOrCommandFrame,
        _ => {
            format_err()?;
            unreachable!();
        }
    })
}
enum FlvTagData {
    Audio,
    Video { frame_type: VideoFrameType },
    Script,
}
async fn read_flv_tag(file: &mut File) -> Result<FlvTag> {
    let mut prev_size = [0u8; 4];
    file.read_exact(&mut prev_size).await?;
    let mut tag_header = [0u8; 12];
    file.read_exact(&mut tag_header).await?;

    let tag_type = tag_header[0];
    tag_header[0] = 0;
    let mut tag_reader = &tag_header[..];
    let data_size = tag_reader.get_u32();
    let timestamp = tag_reader.get_u32();
    let stream_id = tag_reader.get_u32();

    Ok(FlvTag {
        data_size,
        timestamp,
        stream_id,
        data: match tag_type {
            0x8 => {
                file.seek(SeekFrom::Current(data_size as i64)).await?;
                FlvTagData::Audio
            }
            0x9 => {
                let mut buf = [0u8; 4];
                file.read_exact(&mut buf).await?;
                let frame_type = (&buf[..]).get_u32();
                file.seek(SeekFrom::Current(data_size as i64 - 4)).await?;
                FlvTagData::Video {
                    frame_type: read_frame_type(frame_type)?,
                }
            }
            0x12 => {
                file.seek(SeekFrom::Current(data_size as i64)).await?;

                FlvTagData::Script {}
            }
            _ => {
                format_err()?;
                unreachable!();
            }
        },
    })
}

pub async fn generate_patch(mut file: File) -> Result<Option<Patch>> {
    let mut keyframes = Keyframes::new();
    let mut offset: u64 = 13; // flv header
    let mut metadata_offset: u64 = 0;
    let mut metadata_size: u64 = 0;
    let mut metadata: Option<amf0::Value> = None;

    read_flv_header(&mut file).await?;

    loop {
        let FlvTag {
            timestamp, data, ..
        } = read_flv_tag(&mut file).await?;
        match data {
            FlvTagData::Audio => {}
            FlvTagData::Video { frame_type } => {
                if frame_type == VideoFrameType::KeyFrame {
                    keyframes.add(offset, (timestamp as f64) / 1000f64);
                }
            }
            FlvTagData::Script => {}
        };
    }
    // FlvTagData::ScriptData(tag @ ScriptDataTag { .. }) => {
    //     let data = &mut &tag.data[..];
    //     let mut amf_decoder = amf0::Decoder::new(data);
    //     let data = match amf_decoder.decode()? {
    //         amf0::Value::String(name) if name == "onMetaData" => {
    //             amf_decoder.decode()?
    //         }
    //         _ => return Err(anyhow::anyhow!("InvalidData")),
    //     };
    //     metadata_offset = offset;
    //     metadata_size = tag.tag_size() as u64 + 4;
    //     let has_keyframes = has_keyframes(data.clone());
    //     if has_keyframes {
    //         return Ok(None);
    //     }
    //     metadata = Some(data);
    // }
    todo!();
    // task::spawn_blocking(move || {
    //     let mut buf = ReadBuf::new(vec![0; 4096]);
    //     while !buf.stream_state().is_eos() {
    //         buf.fill(&mut file)?;
    //         decoder.decode_from_read_buf(&mut buf)?;
    //         if decoder.is_idle() {
    //             let tag = decoder.finish_decoding()?;
    //             let tag_size = tag.tag_size() as u64;
    //             match tag {
    //                 Tag::Audio(_) => {}
    //                 Tag::Video(VideoTag {
    //                     timestamp,
    //                     frame_type,
    //                     ..
    //                 }) => {
    //                     if frame_type == FrameType::KeyFrame {
    //                         keyframes.add(offset, (timestamp.value() as f64) / 1000f64);
    //                     }
    //                 }
    //                 Tag::ScriptData(tag @ ScriptDataTag { .. }) => {
    //                     let data = &mut &tag.data[..];
    //                     let mut amf_decoder = amf0::Decoder::new(data);
    //                     let data = match amf_decoder.decode()? {
    //                         amf0::Value::String(name) if name == "onMetaData" => {
    //                             amf_decoder.decode()?
    //                         }
    //                         _ => return Err(anyhow::anyhow!("InvalidData")),
    //                     };
    //                     metadata_offset = offset;
    //                     metadata_size = tag.tag_size() as u64 + 4;
    //                     let has_keyframes = has_keyframes(data.clone());
    //                     if has_keyframes {
    //                         return Ok(None);
    //                     }
    //                     metadata = Some(data);
    //                 }
    //             };
    //             // data + pre tag size
    //             offset += tag_size + 4;
    //         }
    //     }
    //     Result::Ok(metadata.map(|m| {
    //         let patched_len =
    //             make_patched(insert_keyframes(m.clone(), keyframes.clone())).len() as i64;
    //         keyframes.offset = (patched_len - metadata_size as i64) as f64;
    //         let patched = make_patched(insert_keyframes(m, keyframes));
    //         Patch {
    //             origin_pos: metadata_offset,
    //             origin_size: metadata_size,
    //             patched,
    //         }
    //     }))
    // })
    // .await
    // .unwrap()
}
