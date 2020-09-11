mod patch;
mod keyframes;

use structopt::StructOpt;
use warp::{path::FullPath, Filter};
use std::{path::PathBuf, sync::Arc};
use tokio::{fs::File, task, prelude::*};
use urlencoding::decode;
use flv_codec::{FileDecoder, Tag, ScriptDataTag, VideoTag, FrameType, TagEncoder, Timestamp, StreamId};
use bytecodec::{Encode, Decode, io::{ReadBuf, IoDecodeExt, IoEncodeExt}};
use amf::amf0;
use keyframes::Keyframes;
use patch::{reader_stream, Patch};
use std::io::{SeekFrom, Cursor};
use anyhow::Result;
use bytes::BufMut;

#[derive(StructOpt)]
struct Args {
    /// root path to serve, default to "./"
    #[structopt(short, long, parse(from_os_str))]
    root_path: Option<PathBuf>,
}

fn map_not_fount<T: std::fmt::Debug>(e: T) ->warp::Rejection {
    println!("map_not_fount {:?}", e);
    warp::reject::not_found()
}

fn with_args(args: Arc<Args>) -> impl Filter<Extract = (Arc<Args>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || args.clone())
}

fn has_keyframes(v: amf0::Value) -> bool {
    match v.try_into_pairs() {
        Ok(mut iter) => iter.any(|i| i.0 == "keyframes"),
        Err(_) => false,
    }
}

fn insert_keyframes(metadata: amf0::Value, keyframes: Keyframes) -> amf0::Value {
    fn map_amf0((key, value): (String, amf::Value)) -> (String, amf0::Value) {
        (key, match value {
            amf::Value::Amf0(v) => v,
            _ => unreachable!()
        })
    }
    let keyframes = std::iter::once(keyframes.into_amf0());
    let value = metadata
        .try_into_pairs()
        .map(|i| {
            amf0::object(i.map(map_amf0).chain(keyframes))
        });
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
    encoder.start_encoding(Tag::ScriptData(ScriptDataTag {
        timestamp: Timestamp::new(0),
        stream_id: StreamId::new(0).unwrap(),
        data: buf.into_inner(),
    })).unwrap();
    encoder.encode_all(&mut out).unwrap();
    let prev_size = out.len();
    let mut tail = Vec::new();
    tail.put_u32(prev_size as u32);
    
    out.append(&mut tail);
    out
}

async fn generate_patch(file: File) -> Result<Option<Patch>> {
    let mut decoder = FileDecoder::new();
    let mut file = file.into_std().await;
    let mut keyframes = Keyframes::new();
    let mut offset: u64 = 13; // flv header
    let mut metadata_offset: u64 = 0;
    let mut metadata_size: u64 = 0;
    let mut metadata: Option<amf0::Value> = None;
    task::spawn_blocking(move || {
        let mut buf = ReadBuf::new(vec![0; 4096]);
        while !buf.stream_state().is_eos() {
            buf.fill(&mut file)?;
            decoder.decode_from_read_buf(&mut buf)?;
            if decoder.is_idle() {
                let tag = decoder.finish_decoding()?;
                let tag_size = tag.tag_size() as u64;
                match tag {
                    Tag::Audio(_) => {},
                    Tag::Video(VideoTag { timestamp, frame_type, .. }) => {
                        if frame_type == FrameType::KeyFrame {
                            keyframes.add(offset, (timestamp.value() as f64) / 1000f64);
                        }
                    },
                    Tag::ScriptData(tag @ ScriptDataTag { .. }) => {
                        let data = &mut &tag.data[..];
                        let mut amf_decoder = amf0::Decoder::new(data);
                        let data = match amf_decoder.decode()? {
                            amf0::Value::String(name) if name == "onMetaData" => {
                                amf_decoder.decode()?
                            },
                            _ => return Err(anyhow::anyhow!("InvalidData")),
                        };
                        metadata_offset = offset;
                        metadata_size = tag.tag_size() as u64 + 4;
                        let has_keyframes = has_keyframes(data.clone());
                        if has_keyframes {
                            return Ok(None)
                        }
                        metadata = Some(data);
                    },
                };
                // data + pre tag size
                offset += tag_size + 4;
            }
        }
        Result::Ok(metadata
            .map(|m| {
                let patched_len = make_patched(
                    insert_keyframes(
                        m.clone(),
                        keyframes.clone()
                    )
                ).len() as i64;
                keyframes.offset = (patched_len - metadata_size as i64) as f64;
                let patched = make_patched(
                    insert_keyframes(
                        m,
                        keyframes
                    )
                );
                Patch {
                    origin_pos: metadata_offset,
                    origin_size: metadata_size,
                    patched,
                }
            })
        )
    }).await.unwrap()
}

async fn generate_keyframes(path: PathBuf, patch_path: PathBuf) -> Result<Option<File>> {
    let file = File::open(path.clone()).await?;
    let patch = generate_patch(file).await?;
    if let Some(patch) = patch {
        let patch = bincode::serialize(&patch)?;
        let mut patch_file = File::create(patch_path.clone()).await?;
        patch_file.write_all(&patch).await?;
        return Ok(Some(File::open(patch_path).await?))
    }
    return Ok(None)
}

async fn reply_with_patch(path: PathBuf, patch_file: Option<File>) -> Result<warp::hyper::Response<warp::hyper::Body>> {
    let patch: Patch = match patch_file {
        Some(mut patch_file) => {
            let mut buf = vec![];
            patch_file.read_to_end(&mut buf).await?;
            bincode::deserialize(&buf[..])?
        },
        None => Patch {
            origin_pos: 0,
            origin_size: 0,
            patched: vec![],
        }
    };

    let file = File::open(path).await?;
    let reader = patch.patch_reader(file).await?;
    let content_length = reader.len();
    let stream = reader_stream(reader);
    Ok(warp::http::Response::builder()
        .header("Content-Length", content_length)
        .body(
            warp::hyper::Body::wrap_stream(stream)
        )?)
}

async fn handle_get(args: Arc<Args>, path: FullPath) -> Result<impl warp::Reply, warp::Rejection>  {
    let root_path = args.root_path.clone().unwrap_or_default();
    let p = decode(&path.as_str()[1..]).map_err(map_not_fount)?;
    let path = root_path.join(PathBuf::from(p));
    let mut patch_path = path.clone();
    patch_path.set_extension("v0.binpatch");
    let patch = File::open(&patch_path).await;

    let patch_file = match patch {
        Ok(pf) => Some(pf),
        Err(_) => generate_keyframes(path.clone(), patch_path).await.map_err(map_not_fount)?,
    };

    let reply = reply_with_patch(path, patch_file).await.map_err(map_not_fount)?;
    Ok(reply)
}

#[paw::main]
#[tokio::main]
async fn main(args: Args) {
    let cors = warp::cors()
        .allow_any_origin();
    let args = Arc::new(args);
    let routes = warp::get()
        .and(with_args(args))
        .and(warp::path::full())
        .and_then(handle_get)
        .with(cors);
    warp::serve(routes)
        .run(([0, 0, 0, 0], 3040))
        .await;
}
