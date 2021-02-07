use bytes::{Bytes, BytesMut};
use futures::{
    ready,
    stream::{self, Stream},
};
use serde_derive::{Deserialize, Serialize};
use std::{
    io::{self, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncSeek, AsyncSeekExt, ReadBuf};

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
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        match position {
            SeekFrom::Start(i) => {
                self.offset = i;
            }
            SeekFrom::Current(i) => {
                self.offset = (self.offset as i64 + i) as u64;
            }
            SeekFrom::End(i) => self.offset = (self.len() as i64 + i) as u64,
        }
        Ok(())
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
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
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
        let read_size = buf.remaining().min(readable as usize);
        match read_from {
            StartPoint::Origin(off) => {
                if self.reader_pos != off {
                    Pin::new(&mut self.reader).start_seek(SeekFrom::Start(off))?;
                    self.reader_pos = ready!(Pin::new(&mut self.reader).poll_complete(cx))?;
                }
                let mut read_buf = buf.take(read_size);
                ready!(Pin::new(&mut self.reader).poll_read(cx, &mut read_buf))?;
                self.reader_pos += read_buf.filled().len() as u64;
            }
            StartPoint::Patch(off) => {
                buf.put_slice(&patch.patched[off..off + read_size]);
            }
        };
        self.offset += buf.filled().len() as u64;
        Poll::Ready(Ok(()))
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
        let mut read_buf = ReadBuf::new(&mut buf);
        match ready!(Pin::new(&mut reader).poll_read(cx, &mut read_buf)) {
            Ok(n) => {}
            Err(e) => return Poll::Ready(Some(Err(e))),
        };
        if read_buf.filled().is_empty() {
            return Poll::Ready(None);
        }
        Poll::Ready(Some(Ok(buf.freeze())))
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
