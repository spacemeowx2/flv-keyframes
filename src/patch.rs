use bytes::{Buf, Bytes, BytesMut};
use futures::{
    ready,
    stream::{self, Stream},
};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncSeek};
use tokio::prelude::*;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Patch {
    pub origin_pos: u64,
    pub origin_size: u64,
    pub patched: Vec<u8>,
}

pub struct PatchedReader<R> {
    reader: R,
    reader_pos: u64,
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
            SeekFrom::End(i) => self.offset = (self.len() as i64 + i) as u64,
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
        } else if off >= (patch.origin_pos + self.patched_len()) {
            // after patch
            let off = off - self.patched_len() + patch.origin_size;
            (StartPoint::Origin(off), self.len() - off)
        } else {
            // in patch
            let off = off - patch.origin_pos;
            (StartPoint::Patch(off as usize), self.patched_len() - off)
        };
        let read_size = buf.len().min(readable as usize);
        let result = match read_from {
            StartPoint::Origin(off) => {
                if self.reader_pos != off {
                    ready!(Pin::new(&mut self.reader).start_seek(cx, SeekFrom::Start(off)))?;
                    self.reader_pos = ready!(Pin::new(&mut self.reader).poll_complete(cx))?;
                }
                let read = ready!(Pin::new(&mut self.reader).poll_read(cx, &mut buf[..read_size]))?;
                self.reader_pos += read as u64;
                read
            }
            StartPoint::Patch(off) => {
                &mut buf[..read_size].copy_from_slice(&patch.patched[off..off + read_size]);
                read_size
            }
        };
        self.offset += result as u64;
        Poll::Ready(Ok(result))
    }
}

#[derive(Debug)]
enum StartPoint {
    Origin(u64),
    Patch(usize),
}

impl<R> PatchedReader<R>
where
    R: AsyncSeek + Send + 'static + Unpin,
{
    pub async fn new(mut reader: R, patch: Patch) -> io::Result<PatchedReader<R>> {
        let origin_length = reader.seek(SeekFrom::End(0)).await?;
        let reader_pos = reader.seek(SeekFrom::Start(0)).await?;
        Ok(PatchedReader {
            reader,
            reader_pos,
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

pub fn reader_stream<R>(mut reader: R) -> impl Stream<Item = Result<Bytes, std::io::Error>>
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
            return Poll::Ready(None);
        }
        Poll::Ready(Some(Ok(buf.to_bytes())))
    })
}

impl Patch {
    pub async fn patch_reader<R>(&self, reader: R) -> io::Result<PatchedReader<R>>
    where
        R: AsyncRead + AsyncSeek + Send + 'static + Unpin,
    {
        Ok(PatchedReader::new(reader, self.clone()).await?)
    }
}
