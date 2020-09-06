use serde::{Serialize, Deserialize};
use futures::{stream::{self, Stream}, ready};
use bytes::{Bytes, buf::BufExt, Buf, BytesMut};
use std::{pin::Pin, task::{Context, Poll}};
use tokio::io::AsyncRead;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Patch {
    pub origin_pos: usize,
    pub origin_size: usize,
    pub patched: Vec<u8>,
}

pub struct PatchedReader<R>
{
    reader: R,
    patch: Patch,
}



pub struct PatchedStream<S>
{
    stream: S,
    patch: Option<Patch>,
    offset: usize,
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
        let end = self.offset + chunk.len();
        if let Some(patch) = &self.patch {
            if end >= patch.origin_pos {
                let patch = self.patch.take().unwrap();
                let right = chunk.split_off(patch.origin_pos - self.offset);
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
