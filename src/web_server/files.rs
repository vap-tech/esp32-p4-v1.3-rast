//! Four bounded HTTP transfer states share one scratch and one serialized FS.
use super::*;
use esp32_p4::storage::files::{FileHandle, OpenMode, QUANTUM};
#[derive(Clone, Copy)]
pub(super) enum Transfer {
    Idle,
    Download { file: FileHandle, left: u32 },
    Upload { file: FileHandle, left: u32 },
    Drain,
}
impl Transfer {
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
    let writing = r.method == http::Method::Put;
    let mode = if writing {
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
        if writing {
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
            web::asyncfs::created(io)?;
            io.flush()?;
        }
        Ok(Transfer::Drain)
    } else if writing {
        Ok(Transfer::Upload {
            file: handle,
            left: len,
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
            files.abort(file).await.map_err(|_| web::Error::Storage)?
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
        Transfer::Upload { file, mut left } => {
            let want = scratch.len().min(QUANTUM).min(left as usize);
            let n = network.borrow_mut().tcp.read(slot, &mut scratch[..want]);
            if n == 0 {
                return if network.borrow().tcp.eof(slot) {
                    Err(web::Error::Transport)
                } else {
                    Ok(Step::Waiting)
                };
            }
            let wrote = files
                .write(file, &scratch[..n])
                .await
                .map_err(|_| web::Error::Storage)?;
            if wrote != n {
                return Err(web::Error::Storage);
            }
            left -= n as u32;
            if left == 0 {
                files.close(file).await.map_err(|_| web::Error::Storage)?;
                *state = Transfer::Drain;
                let mut io = Connection {
                    network,
                    slot,
                    buffer: [0; tcp::MSS],
                    used: 0,
                };
                web::asyncfs::created(&mut io)?;
                io.flush()?;
            } else {
                *state = Transfer::Upload { file, left };
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
