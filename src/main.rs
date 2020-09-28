mod flv;
mod keyframes;
mod patch;

use anyhow::Result;
use flv::generate_patch;
use headers::{HeaderMap, HeaderMapExt, Range};
use patch::{reader_stream, Patch};
use std::{io::SeekFrom, path::PathBuf, sync::Arc};
use structopt::StructOpt;
use tokio::{fs::File, prelude::*};
use urlencoding::decode;
use warp::{path::FullPath, Filter};

#[derive(StructOpt)]
struct Args {
    /// root path to serve, default to "./"
    #[structopt(short, long, parse(from_os_str))]
    root_path: Option<PathBuf>,
}

fn map_not_found<T: std::fmt::Debug>(e: T) -> warp::Rejection {
    println!("map_not_found {:?}", e);
    warp::reject::not_found()
}

fn with_args(
    args: Arc<Args>,
) -> impl Filter<Extract = (Arc<Args>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || args.clone())
}

async fn generate_keyframes(path: PathBuf, patch_path: PathBuf) -> Result<Option<File>> {
    let file = File::open(path.clone()).await?;
    let patch = generate_patch(file).await?;
    if let Some(patch) = patch {
        let patch = bincode::serialize(&patch)?;
        let mut patch_file = File::create(patch_path.clone()).await?;
        patch_file.write_all(&patch).await?;
        return Ok(Some(File::open(patch_path).await?));
    }
    return Ok(None);
}

async fn reply_with_patch(
    path: PathBuf,
    patch_file: Option<File>,
    range: Option<Range>,
) -> Result<warp::hyper::Response<warp::hyper::Body>> {
    use std::ops::Bound;

    let patch: Patch = match patch_file {
        Some(mut patch_file) => {
            let mut buf = vec![];
            patch_file.read_to_end(&mut buf).await?;
            bincode::deserialize(&buf[..])?
        }
        None => Patch {
            origin_pos: 0,
            origin_size: 0,
            patched: vec![],
        },
    };

    let file = File::open(path).await?;
    let mut reader = patch.patch_reader(file).await?;
    let max_len = reader.len();
    let range = if let Some(range) = range {
        range
            .iter()
            .map(|(start, end)| {
                let start = match start {
                    Bound::Unbounded => 0,
                    Bound::Included(s) => s,
                    Bound::Excluded(s) => s + 1,
                };
                let end = match end {
                    Bound::Unbounded => max_len,
                    Bound::Included(s) => s,
                    Bound::Excluded(s) => s + 1,
                };
                if start < end && end <= max_len {
                    io::Result::Ok((start, end))
                } else {
                    Err(io::ErrorKind::InvalidData.into())
                }
            })
            .next()
            .unwrap_or(Ok((0, max_len)))
    } else {
        io::Result::Ok((0, max_len))
    }?;
    reader.seek(SeekFrom::Start(range.0)).await?;
    let reader = reader.take(range.1 - range.0);
    let stream = reader_stream(reader);
    let mut builder = warp::http::Response::builder();
    builder = builder.header("Content-Length", range.1 - range.0);
    builder = builder.header(
        "Content-Range",
        format!("bytes {}-{}/{}", range.0, range.1, max_len),
    );

    Ok(builder.body(warp::hyper::Body::wrap_stream(stream))?)
}

async fn handle_get(
    args: Arc<Args>,
    path: FullPath,
    headers: HeaderMap,
) -> Result<impl warp::Reply, warp::Rejection> {
    let range: Option<Range> = headers.typed_get();
    let root_path = args.root_path.clone().unwrap_or_default();
    let p = decode(&path.as_str()[1..]).map_err(map_not_found)?;
    let path = root_path.join(PathBuf::from(p));
    let mut patch_path = path.clone();
    let filename = patch_path.file_name().unwrap_or_default().to_os_string();
    patch_path.set_file_name(format!(".{}", filename.to_string_lossy()));
    patch_path.set_extension("v0.binpatch");
    let patch = File::open(&patch_path).await;

    let patch_file = match patch {
        Ok(pf) => Some(pf),
        Err(_) => generate_keyframes(path.clone(), patch_path)
            .await
            .map_err(map_not_found)?,
    };

    let reply = reply_with_patch(path, patch_file, range)
        .await
        .map_err(map_not_found)?;
    Ok(reply)
}

#[paw::main]
#[tokio::main]
async fn main(args: Args) {
    let cors = warp::cors()
        .allow_any_origin()
        .allow_method("GET")
        .allow_header("range");
    let args = Arc::new(args);
    let routes = warp::get()
        .and(with_args(args))
        .and(warp::path::full())
        .and(warp::header::headers_cloned())
        .and_then(handle_get)
        .with(cors);
    warp::serve(routes).run(([0, 0, 0, 0], 3040)).await;
}
