//! Four bounded HTTP transfers share one SRAM scratch and a serialized filesystem.
use super::*;
use esp32_p4::{
    storage::files::{FileHandle, OpenMode, QUANTUM},
    web::upload::{UploadPool, CHUNK},
};
#[derive(Clone, Copy)]
pub(super) struct Upload {
    file: FileHandle,
    left: u32,
    total: u32,
    queued: usize,
    head: usize,
    overlap_bytes: u32,
    peak_queued: usize,
    flushes: u32,
    started: u64,
    #[cfg(feature = "diagnostics")]
    trace: super::storage_lab::HttpTrace,
}
// Diagnostic counters deliberately live in the four bounded states, not a heap.
#[cfg_attr(feature = "diagnostics", allow(clippy::large_enum_variant))]
#[derive(Clone, Copy)]
pub(super) enum Transfer {
    Idle,
    Download { file: FileHandle, left: u32 },
    Upload(Upload),
    Drain,
}
impl Transfer {
    #[cfg(feature = "diagnostics")]
    pub fn set_trace(&mut self, mut value: super::storage_lab::HttpTrace, now: u64) {
        value.opened(now);
        if let Self::Upload(upload) = self {
            upload.trace = value;
        }
    }
    #[cfg(feature = "diagnostics")]
    pub fn mark_shared(&mut self) {
        if let Self::Upload(upload) = self {
            upload.trace.shared();
        }
    }
    pub fn idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
}
pub(super) enum Step {
    Progress,
    Waiting,
    Done,
}
pub(super) async fn start<D: disk::BlockDevice, const N: usize>(
    r: &http::Request,
    files: &mut FileTable<'_, '_, D, N>,
    io: &mut impl Transport,
    staging: &mut UploadPool<'_, '_, { tcp::SLOTS }>,
    slot: usize,
    now: u64,
) -> Result<Transfer, web::Error> {
    let writing = matches!(r.method, http::Method::Put | http::Method::Post);
    let append = cfg!(feature = "diagnostics") && r.method == http::Method::Post;
    let mode = if append {
        OpenMode::Append
    } else if writing {
        OpenMode::CreateNew
    } else {
        OpenMode::Read
    };
    let handle = match files.open(r.path(), mode).await {
        Ok(h) => h,
        Err(e) => {
            web::error(io, web::asyncfs::status(&e), r.method == http::Method::Head)?;
            io.flush()?;
            return Ok(Transfer::Drain);
        }
    };
    let len = if writing {
        r.length
    } else {
        files.info(handle).map_err(|_| web::Error::Storage)?.len as u32
    };
    if writing && len != 0 && staging.enabled() && staging.acquire(slot).is_err() {
        if append {
            files.close(handle).await
        } else {
            files.abort(handle).await
        }
        .map_err(|_| web::Error::Storage)?;
        web::error(io, 503, false)?;
        io.flush()?;
        return Ok(Transfer::Drain);
    }
    let result = if writing {
        if r.expect {
            io.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .and_then(|_| io.flush())
        } else {
            Ok(())
        }
    } else {
        web::asyncfs::file_headers(r, len, io).and_then(|_| io.flush())
    };
    if let Err(e) = result {
        if writing && !append {
            files.abort(handle).await
        } else {
            files.close(handle).await
        }
        .map_err(|_| web::Error::Storage)?;
        if writing && staging.enabled() && !staging.release(slot) {
            return Err(web::Error::Storage);
        }
        return Err(e);
    }
    if r.method == http::Method::Head || len == 0 {
        files.close(handle).await.map_err(|_| web::Error::Storage)?;
        if writing {
            #[cfg(feature = "diagnostics")]
            if append {
                web::asyncfs::appended(io)?;
                io.flush()?;
                return Ok(Transfer::Drain);
            }
            web::asyncfs::created(io)?;
            io.flush()?;
        }
        Ok(Transfer::Drain)
    } else if writing {
        Ok(Transfer::Upload(Upload {
            file: handle,
            left: len,
            total: len,
            queued: 0,
            head: 0,
            overlap_bytes: 0,
            peak_queued: 0,
            flushes: 0,
            started: now,
            #[cfg(feature = "diagnostics")]
            trace: super::storage_lab::HttpTrace::EMPTY,
        }))
    } else {
        Ok(Transfer::Download {
            file: handle,
            left: len,
        })
    }
}
pub(super) async fn cancel<D: disk::BlockDevice, const N: usize>(
    state: &mut Transfer,
    files: &mut FileTable<'_, '_, D, N>,
    staging: &mut UploadPool<'_, '_, { tcp::SLOTS }>,
    slot: usize,
) -> Result<(), web::Error> {
    match *state {
        Transfer::Upload(upload) => {
            if files
                .info(upload.file)
                .map_err(|_| web::Error::Storage)?
                .mode
                == OpenMode::CreateNew
            {
                files
                    .abort(upload.file)
                    .await
                    .map_err(|_| web::Error::Storage)?;
            } else {
                files
                    .close(upload.file)
                    .await
                    .map_err(|_| web::Error::Storage)?;
            }
            if staging.enabled() && !staging.release(slot) {
                return Err(web::Error::Storage);
            }
        }
        Transfer::Download { file, .. } => {
            files.close(file).await.map_err(|_| web::Error::Storage)?;
        }
        _ => {}
    }
    *state = Transfer::Idle;
    Ok(())
}
pub(super) async fn step<P: Port, D: disk::BlockDevice, const N: usize>(
    state: &mut Transfer,
    slot: usize,
    files: &mut FileTable<'_, '_, D, N>,
    network: &RefCell<Network<'_, '_, '_, P>>,
    scratch: &mut [u8],
    staging: &mut UploadPool<'_, '_, { tcp::SLOTS }>,
) -> Result<Step, web::Error> {
    if !network.borrow().tcp.alive(slot) {
        return Err(web::Error::Transport);
    }
    match *state {
        Transfer::Download { file, mut left } => {
            let want = scratch
                .len()
                .min(QUANTUM)
                .min(left as usize)
                .min(network.borrow().tcp.write_capacity(slot));
            if want == 0 {
                return Ok(Step::Waiting);
            }
            let n = files
                .read(file, &mut scratch[..want])
                .await
                .map_err(|_| web::Error::Storage)?;
            if n == 0 || network.borrow_mut().tcp.write(slot, &scratch[..n]) != n {
                return Err(web::Error::Transport);
            }
            left -= n as u32;
            if left == 0 {
                files.close(file).await.map_err(|_| web::Error::Storage)?;
                *state = Transfer::Drain;
            } else {
                *state = Transfer::Download { file, left };
            }
            Ok(Step::Progress)
        }
        Transfer::Upload(mut upload) => {
            let available = network.borrow().tcp.read_available(slot);
            #[cfg(feature = "diagnostics")]
            if available == 0 && upload.left != 0 {
                upload
                    .trace
                    .empty(network.borrow().port.borrow_mut().now_us());
            }
            if network.borrow().tcp.input_finished(slot) && available < upload.left as usize {
                return Err(web::Error::Transport);
            }
            if staging.enabled() {
                let tail = (upload.head + upload.queued) % CHUNK;
                let want = available
                    .min(upload.left as usize)
                    .min(scratch.len())
                    .min(CHUNK - upload.queued)
                    .min(CHUNK - tail);
                if want != 0 {
                    #[cfg(feature = "diagnostics")]
                    let read_start = network.borrow().port.borrow_mut().now_us();
                    let n = network.borrow_mut().tcp.read(slot, &mut scratch[..want]);
                    #[cfg(feature = "diagnostics")]
                    let copy_start = {
                        let now = network.borrow().port.borrow_mut().now_us();
                        upload.trace.read(now - read_start);
                        upload.trace.available(now);
                        now
                    };
                    staging
                        .append(slot, tail, &scratch[..n])
                        .map_err(|_| web::Error::Storage)?;
                    #[cfg(feature = "diagnostics")]
                    upload
                        .trace
                        .copied(network.borrow().port.borrow_mut().now_us() - copy_start, n);
                    upload.left -= n as u32;
                    upload.queued += n;
                    upload.peak_queued = upload.peak_queued.max(upload.queued);
                }
                if upload.queued < esp32_p4::web::upload::BLOCK && upload.left != 0 {
                    *state = Transfer::Upload(upload);
                    return Ok(if want == 0 {
                        Step::Waiting
                    } else {
                        Step::Progress
                    });
                }
                let length = upload.queued.min(QUANTUM).min(CHUNK - upload.head);
                let (source, before, after) = staging.parts(slot, upload.head, length);
                let source_end = upload.head + length;
                // The filesystem future owns only the immutable in-flight range.
                // While SD is pending, receive into other, disjoint ring regions.
                #[cfg(feature = "diagnostics")]
                let write_start = network.borrow().port.borrow_mut().now_us();
                let write = files.write(upload.file, source);
                let mut write = core::pin::pin!(write);
                let wrote = core::future::poll_fn(|cx| {
                    use core::{future::Future, task::Poll};
                    if let Poll::Ready(result) = write.as_mut().poll(cx) {
                        return Poll::Ready(result);
                    }
                    let tail = (upload.head + upload.queued) % CHUNK;
                    let available = network.borrow().tcp.read_available(slot);
                    let want = available
                        .min(upload.left as usize)
                        .min(scratch.len())
                        .min(CHUNK - upload.queued)
                        .min(CHUNK - tail);
                    if want != 0 {
                        #[cfg(feature = "diagnostics")]
                        let read_start = network.borrow().port.borrow_mut().now_us();
                        let n = network.borrow_mut().tcp.read(slot, &mut scratch[..want]);
                        #[cfg(feature = "diagnostics")]
                        let copy_start = {
                            let now = network.borrow().port.borrow_mut().now_us();
                            upload.trace.read(now - read_start);
                            upload.trace.available(now);
                            now
                        };
                        let dst = if tail < upload.head {
                            &mut before[tail..tail + n]
                        } else {
                            &mut after[tail - source_end..tail - source_end + n]
                        };
                        esp32_p4::web::upload::copy_paced(dst, &scratch[..n]);
                        #[cfg(feature = "diagnostics")]
                        upload
                            .trace
                            .copied(network.borrow().port.borrow_mut().now_us() - copy_start, n);
                        upload.queued += n;
                        upload.overlap_bytes += n as u32;
                        upload.peak_queued = upload.peak_queued.max(upload.queued);
                        upload.left -= n as u32;
                        if n != 0 {
                            cx.waker().wake_by_ref();
                        }
                    }
                    Poll::Pending
                })
                .await
                .map_err(|_| web::Error::Storage)?;
                #[cfg(feature = "diagnostics")]
                upload
                    .trace
                    .wrote(network.borrow().port.borrow_mut().now_us() - write_start);
                if wrote != length {
                    return Err(web::Error::Storage);
                }
                upload.head = (upload.head + wrote) % CHUNK;
                upload.queued -= wrote;
            } else {
                let want =
                    web::asyncfs::upload_chunk(available, upload.left as usize, scratch.len());
                if want == 0 {
                    return Ok(Step::Waiting);
                }
                let n = network.borrow_mut().tcp.read(slot, &mut scratch[..want]);
                let wrote = files
                    .write(upload.file, &scratch[..n])
                    .await
                    .map_err(|_| web::Error::Storage)?;
                if wrote != n {
                    return Err(web::Error::Storage);
                }
                upload.left -= n as u32;
            }
            upload.flushes += 1;
            if upload.flushes.is_multiple_of(2048) {
                let _ = writeln!(
                    network.borrow_mut().console,
                    "WEB UPLOAD PROGRESS slot={} received={} total={}",
                    slot,
                    upload.total - upload.left,
                    upload.total
                );
            }
            if upload.left == 0 && upload.queued == 0 {
                #[cfg(feature = "diagnostics")]
                let append = files
                    .info(upload.file)
                    .map_err(|_| web::Error::Storage)?
                    .mode
                    == OpenMode::Append;
                #[cfg(feature = "diagnostics")]
                let begun = network.borrow().port.borrow_mut().now_us();
                files
                    .close(upload.file)
                    .await
                    .map_err(|_| web::Error::Storage)?;
                if staging.enabled() && !staging.release(slot) {
                    return Err(web::Error::Storage);
                }
                let now = network.borrow().port.borrow_mut().now_us();
                let _ = writeln!(
                    network.borrow_mut().console,
                    "WEB UPLOAD DONE slot={} bytes={} flushes={} us={} staging={}",
                    slot,
                    upload.total,
                    upload.flushes,
                    now - upload.started,
                    if staging.enabled() {
                        "PSRAM-ring"
                    } else {
                        "SRAM"
                    }
                );
                let _ = writeln!(
                    network.borrow_mut().console,
                    "WEB RING USE slot={} received_during_sd={} peak_queued={} capacity={}",
                    slot,
                    upload.overlap_bytes,
                    upload.peak_queued,
                    if staging.enabled() { CHUNK } else { 0 }
                );
                let stats = network.borrow().writes.get();
                let _ = writeln!(network.borrow_mut().console, "SD WRITE TOTALS {:?}", stats);
                #[cfg(feature = "diagnostics")]
                {
                    upload.trace.closed(now - begun);
                    upload.trace.report(now, slot, &mut network.borrow_mut());
                }
                *state = Transfer::Drain;
                let mut io = Connection {
                    network,
                    slot,
                    buffer: [0; tcp::MSS],
                    used: 0,
                };
                #[cfg(feature = "diagnostics")]
                if append {
                    web::asyncfs::appended(&mut io)?;
                } else {
                    web::asyncfs::created(&mut io)?;
                }
                #[cfg(not(feature = "diagnostics"))]
                web::asyncfs::created(&mut io)?;
                io.flush()?;
            } else {
                *state = Transfer::Upload(upload);
            }
            Ok(Step::Progress)
        }
        Transfer::Drain => {
            if !network.borrow().tcp.flushed(slot) {
                return Ok(Step::Waiting);
            }
            network.borrow_mut().tcp.release(slot);
            *state = Transfer::Idle;
            Ok(Step::Done)
        }
        Transfer::Idle => Ok(Step::Waiting),
    }
}
