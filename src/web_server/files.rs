//! Four bounded HTTP transfer states share one scratch and one serialized FS.
use super::*;
use esp32_p4::storage::files::{FileHandle, OpenMode, QUANTUM};
#[derive(Clone, Copy)]
pub(super) enum Transfer {
    Idle,
    Download {
        file: FileHandle,
        left: u32,
    },
    Upload {
        file: FileHandle,
        left: u32,
        #[cfg(feature = "diagnostics")]
        trace: super::storage_lab::HttpTrace,
    },
    Drain,
}
impl Transfer {
    #[cfg(feature = "diagnostics")]
    pub fn set_trace(&mut self, mut value: super::storage_lab::HttpTrace, now: u64) {
        value.opened(now);
        if let Self::Upload { trace, .. } = self {
            *trace = value;
        }
    }
    #[cfg(feature = "diagnostics")]
    pub fn mark_shared(&mut self) {
        if let Self::Upload { trace, .. } = self {
            trace.shared();
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
    let result = if writing {
        if r.expect {
            io.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .and_then(|_| io.flush())
        } else {
            Ok(())
        }
    } else {
        let len = files.info(handle).map_err(|_| web::Error::Storage)?.len as u32;
        web::asyncfs::file_headers(r, len, io).and_then(|_| io.flush())
    };
    if let Err(e) = result {
        if writing && !append {
            files.abort(handle).await.map_err(|_| web::Error::Storage)?;
        } else {
            files.close(handle).await.map_err(|_| web::Error::Storage)?;
        }
        return Err(e);
    }
    let len = if writing {
        r.length
    } else {
        files.info(handle).map_err(|_| web::Error::Storage)?.len as u32
    };
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
        Ok(Transfer::Upload {
            file: handle,
            left: len,
            #[cfg(feature = "diagnostics")]
            trace: super::storage_lab::HttpTrace::EMPTY,
        })
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
) -> Result<(), web::Error> {
    match *state {
        Transfer::Upload { file, .. } => {
            if files.info(file).map_err(|_| web::Error::Storage)?.mode == OpenMode::CreateNew {
                files.abort(file).await.map_err(|_| web::Error::Storage)?;
            } else {
                files.close(file).await.map_err(|_| web::Error::Storage)?;
            }
        }
        Transfer::Download { file, .. } => {
            files.close(file).await.map_err(|_| web::Error::Storage)?
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
    network: &RefCell<Network<'_, '_, P>>,
    scratch: &mut [u8],
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
        Transfer::Upload {
            file,
            mut left,
            #[cfg(feature = "diagnostics")]
            mut trace,
        } => {
            let available = network.borrow().tcp.read_available(slot);
            if network.borrow().tcp.input_finished(slot) && available < left as usize {
                return Err(web::Error::Transport);
            }
            let want =
                web::asyncfs::upload_chunk(available, left as usize, scratch.len().min(QUANTUM));
            if want == 0 {
                #[cfg(feature = "diagnostics")]
                {
                    trace.empty(network.borrow().port.borrow_mut().now_us());
                    *state = Transfer::Upload { file, left, trace };
                }
                return if network.borrow().tcp.eof(slot) {
                    Err(web::Error::Transport)
                } else {
                    Ok(Step::Waiting)
                };
            }
            // A ring wrap can split a sector even when enough bytes are queued.
            // Join only the already-available bytes; this loop performs no IO wait.
            let mut n = 0;
            while n < want {
                let got = network.borrow_mut().tcp.read(slot, &mut scratch[n..want]);
                if got == 0 {
                    return Err(web::Error::Transport);
                }
                n += got;
            }
            #[cfg(feature = "diagnostics")]
            let begun = network.borrow().port.borrow_mut().now_us();
            #[cfg(feature = "diagnostics")]
            trace.available(begun);
            let wrote = files
                .write(file, &scratch[..n])
                .await
                .map_err(|_| web::Error::Storage)?;
            #[cfg(feature = "diagnostics")]
            trace.wrote(network.borrow().port.borrow_mut().now_us() - begun);
            if wrote != n {
                return Err(web::Error::Storage);
            }
            left -= n as u32;
            if left == 0 {
                #[cfg(feature = "diagnostics")]
                let append =
                    files.info(file).map_err(|_| web::Error::Storage)?.mode == OpenMode::Append;
                #[cfg(feature = "diagnostics")]
                let begun = network.borrow().port.borrow_mut().now_us();
                files.close(file).await.map_err(|_| web::Error::Storage)?;
                #[cfg(feature = "diagnostics")]
                {
                    let now = network.borrow().port.borrow_mut().now_us();
                    trace.closed(now - begun);
                    trace.report(now, slot, network.borrow_mut().console);
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
                *state = Transfer::Upload {
                    file,
                    left,
                    #[cfg(feature = "diagnostics")]
                    trace,
                };
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
