use crate::flv_reader::{read_flv, FlvTag, FlvTagData, VideoFrameType};
use crate::keyframes::Keyframes;
use crate::patch::Patch;
use amf::amf0;
use anyhow::Result;
use bytes::BufMut;
use deku::prelude::*;
use futures::{pin_mut, stream::TryStreamExt};
use std::io::Cursor;
use tokio::fs::File;

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
    let mut buf = buf.into_inner();

    let tag = FlvTag {
        tag_type: 0x12,
        data_size: buf.len() as u32,
        timestamp: 0,
        stream_id: 0,
        ..Default::default()
    };
    let mut out = tag.to_bytes().unwrap();
    out.append(&mut buf);
    let prev_size = out.len();
    let mut tail = Vec::new();
    tail.put_u32(prev_size as u32);

    out.append(&mut tail);
    out
}

pub async fn generate_patch(mut file: File) -> Result<Option<Patch>> {
    let mut keyframes = Keyframes::new();
    let mut metadata_offset: u64 = 0;
    let mut metadata_size: u64 = 0;
    let mut metadata: Option<amf0::Value> = None;

    let (header, stream) = read_flv(&mut file).await?;
    let offset = (header.data_offset + 4) as u64;
    pin_mut!(stream);

    while let Some(FlvTag {
        timestamp,
        data,
        data_size,
        ..
    }) = stream.try_next().await?
    {
        match data {
            FlvTagData::Video { frame_type } => {
                if frame_type == VideoFrameType::KeyFrame {
                    keyframes.add(offset, (timestamp as f64) / 1000f64);
                }
            }
            FlvTagData::Script { data } => {
                let data = Cursor::new(&data[..]);
                let mut amf_decoder = amf0::Decoder::new(data);
                let data = match amf_decoder.decode()? {
                    amf0::Value::String(name) if name == "onMetaData" => amf_decoder.decode()?,
                    _ => return Err(anyhow::anyhow!("InvalidData")),
                };
                metadata_offset = offset;
                metadata_size = data_size as u64 + 4;
                let has_keyframes = has_keyframes(data.clone());
                if has_keyframes {
                    return Ok(None);
                }
                metadata = Some(data);
            }
            FlvTagData::Audio | FlvTagData::Other => {}
        };
    }
    Ok(metadata.map(|m| {
        let patched_len = make_patched(insert_keyframes(m.clone(), keyframes.clone())).len() as i64;
        keyframes.offset = (patched_len - metadata_size as i64) as f64;
        let patched = make_patched(insert_keyframes(m, keyframes));
        Patch {
            origin_pos: metadata_offset,
            origin_size: metadata_size,
            patched,
        }
    }))
}
