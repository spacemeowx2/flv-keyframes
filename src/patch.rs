use serde::{Serialize, Deserialize};
use futures::{stream::{self, Stream}, ready, pin_mut};
use bytes::{Bytes, buf::BufExt, Buf, BytesMut};
use std::{pin::Pin, task::{Context, Poll}, io::{self, SeekFrom}};
use tokio::io::{AsyncRead, AsyncSeek};
use tokio::prelude::*;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Patch {
    pub origin_pos: u64,
    pub origin_size: u64,
    pub patched: Vec<u8>,
}

pub struct PatchedReader<R>
{
    reader: R,
    patch: Patch,
    offset: u64,
    origin_length: u64,
}

impl<R> AsyncSeek for PatchedReader<R>
where
    R: AsyncSeek + Send + 'static + Unpin,
{
    fn start_seek(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        position: SeekFrom,
    ) -> Poll<io::Result<()>> {
        match position {
            SeekFrom::Start(i) => {
                self.offset = i;
            }
            SeekFrom::Current(i) => {
                self.offset = (self.offset as i64 + i) as u64;
            }
            SeekFrom::End(i) => {
                self.offset = (self.len() as i64 + i) as u64
            },
        }
        Poll::Ready(Ok(()))
    }

    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Ok(self.offset))
    }
}

impl<R> AsyncRead for PatchedReader<R>
where
    R: AsyncRead + AsyncSeek + Send + 'static + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let off = self.offset;
        let patch = &self.patch;
        let (read_from, readable) = if off < patch.origin_pos {
            // before patch
            (StartPoint::Origin(off), patch.origin_pos - off)
        } else if off > (patch.origin_pos + self.patched_len()) {
            // after patch
            let off = off - self.patched_len() + patch.origin_size;
            (StartPoint::Origin(off), 0)
        } else {
            // in patch
            let off = off - patch.origin_pos;
            (StartPoint::Patch(off), self.patched_len() - off)
        };
        let read_size = buf.len().min(readable as usize);
        let result = match read_from {
            StartPoint::Origin(off) => {
                ready!(Pin::new(&mut self.reader).start_seek(cx, SeekFrom::Start(off)))?;
                ready!(Pin::new(&mut self.reader).poll_complete(cx))?;
                ready!(Pin::new(&mut self.reader).poll_read(cx, &mut buf[..read_size]))?
            }
            StartPoint::Patch(off) => {
                &mut buf[..read_size].copy_from_slice(&patch.patched[off as usize..read_size]);
                read_size
            }
        };
        self.offset += result as u64;
        Poll::Ready(Ok(result))
    }
}

enum StartPoint {
    Origin(u64),
    Patch(u64),
}

impl<R> PatchedReader<R>
where
    R: AsyncSeek + Send + 'static + Unpin,
{
    pub async fn new(mut reader: R, patch: Patch) -> io::Result<PatchedReader<R>> {
        let origin_length = reader.seek(SeekFrom::End(0)).await?;
        Ok(PatchedReader {
            reader,
            patch,
            offset: 0,
            origin_length,
        })
    }
    pub fn len(&self) -> u64 {
        self.origin_length + (self.patched_len()) - self.patch.origin_size
    }
    pub fn patched_len(&self) -> u64 {
        self.patch.patched.len() as u64
    }
}

pub struct PatchedStream<S>
{
    stream: S,
    patch: Option<Patch>,
    offset: u64,
}

impl<S, O, E> Stream for PatchedStream<S>
where
    S: Stream<Item = Result<O, E>> + Send + 'static + Unpin,
    O: Into<Bytes> + 'static,
    E: Into<Box<dyn std::error::Error + Send + Sync>> + 'static, 
{
    type Item = Result<Bytes, E>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let chunk = ready!(Pin::new(&mut self.stream).poll_next(cx));
        let mut chunk: Bytes = match chunk {
            Some(Ok(c)) => c.into(),
            Some(Err(e)) => return Poll::Ready(Some(Err(e))),
            None => return Poll::Ready(None),
        };
        let end = self.offset + chunk.len() as u64;
        if let Some(patch) = &self.patch {
            if end >= patch.origin_pos {
                let patch = self.patch.take().unwrap();
                let right = chunk.split_off((patch.origin_pos - self.offset) as usize);
                let patched: Bytes = patch.patched.into();
                let chained = chunk.chain(patched).chain(right).to_bytes();
                return Poll::Ready(Some(Ok(chained)))
            }
        }
        self.offset = end;
        Poll::Ready(Some(Ok(chunk)))
    }
}

pub fn reader_stream<R>(mut reader: R) -> impl Stream<Item=Result<Bytes, std::io::Error>>
where
    R: AsyncRead + Send + 'static + Unpin,
{
    stream::poll_fn(move |cx| {
        let mut buf = BytesMut::new();
        buf.reserve(2048);
        let n = match ready!(Pin::new(&mut reader).poll_read_buf(cx, &mut buf)) {
            Ok(n) => n,
            Err(e) => return Poll::Ready(Some(Err(e))),
        };
        if n == 0 {
            return Poll::Ready(None)
        }
        Poll::Ready(Some(Ok(buf.to_bytes())))
    })
}

impl Patch {
    pub fn patch_reader<R>(&self, mut reader: R) -> PatchedStream<impl Stream<Item=Result<Bytes, std::io::Error>>>
    where
        R: AsyncRead + Send + 'static + Unpin,
    {
        let stream = stream::poll_fn(move |cx| {
            let mut buf = BytesMut::new();
            buf.reserve(2048);
            let n = match ready!(Pin::new(&mut reader).poll_read_buf(cx, &mut buf)) {
                Ok(n) => n,
                Err(e) => return Poll::Ready(Some(Err(e))),
            };
            if n == 0 {
                return Poll::Ready(None)
            }
            Poll::Ready(Some(Ok(buf.to_bytes())))
        });
        self.patch_stream(stream)
    }
    pub fn patch_stream<S, O, E>(&self, stream: S) -> PatchedStream<S>
    where
        S: Stream<Item = Result<O, E>> + Send + 'static,
        O: Into<Bytes> + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + 'static, 
    {
        PatchedStream {
            stream,
            patch: Some(self.clone()),
            offset: 0,
        }
    }
}
