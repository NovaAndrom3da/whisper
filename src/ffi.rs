use std::{
    ffi::{c_char, c_int, c_ushort, CStr, CString},
    net::SocketAddr,
    ptr,
};

use hyper::Uri;
use tokio::{
    runtime::Runtime,
    sync::{
        mpsc::{unbounded_channel, UnboundedSender},
        Mutex,
    },
};
use tun2::{create_as_async, AsyncDevice, Configuration};

use crate::{
    start_whisper,
    util::{connect_to_wisp, WhisperError, WhisperMux},
    WhisperEvent, WispServer,
};

struct WhisperInitState {
    mux: WhisperMux,
    tun: AsyncDevice,
    mtu: u16,
    socketaddr: SocketAddr,
}

struct WhisperRunningState {
    socketaddr: SocketAddr,
    channel: UnboundedSender<WhisperEvent>,
}

static WHISPER: Mutex<(Option<WhisperInitState>, Option<WhisperRunningState>)> =
    Mutex::const_new((None, None));

#[no_mangle]
pub extern "C" fn whisper_init(fd: c_int, ws: *const c_char, mtu: c_ushort) -> bool {
    let ws = unsafe {
        if ws.is_null() {
            return false;
        }
        CStr::from_ptr(ws).to_string_lossy().to_string()
    };
    if let Ok(rt) = Runtime::new() {
        rt.block_on(async {
            let mut whisper = WHISPER.lock().await;

            if whisper.0.is_some() || whisper.1.is_some() {
                return Err(WhisperError::AlreadyInitialized);
            }

            let (mux, socketaddr) = connect_to_wisp(&WispServer {
                pty: None,
                url: Some(Uri::try_from(ws).map_err(WhisperError::other)?),
            })
            .await
            .map_err(WhisperError::Other)?;

            let mut cfg = Configuration::default();
            cfg.raw_fd(fd);
            let tun = create_as_async(&cfg).map_err(WhisperError::other)?;

            whisper.0.replace(WhisperInitState {
                mux,
                tun,
                mtu,
                socketaddr: socketaddr.ok_or(WhisperError::NoSocketAddr)?,
            });
            Ok(())
        })
        .is_ok()
    } else {
        false
    }
}

#[no_mangle]
pub extern "C" fn whisper_get_ws_ip() -> *mut c_char {
    if let Ok(rt) = Runtime::new() {
        let ip = rt.block_on(async {
            let whisper = WHISPER.lock().await;
            if let Some(init) = &whisper.0 {
                CString::new(init.socketaddr.to_string()).map_err(WhisperError::other)
            } else if let Some(running) = &whisper.1 {
                CString::new(running.socketaddr.to_string()).map_err(WhisperError::other)
            } else {
                Err(WhisperError::NotInitialized)
            }
        });
        match ip {
            Ok(ptr) => ptr.into_raw(),
            Err(_) => ptr::null_mut(),
        }
    } else {
        ptr::null_mut()
    }
}

#[no_mangle]
pub extern "C" fn whisper_free(s: *mut c_char) {
    unsafe {
        if s.is_null() {
            return;
        }
        let _ = CString::from_raw(s);
    };
}

#[no_mangle]
pub extern "C" fn whisper_start() -> bool {
    if let Ok(rt) = Runtime::new() {
        rt.block_on(async {
            let mut whisper = WHISPER.lock().await;
            if whisper.1.is_some() {
                return Err(WhisperError::AlreadyStarted);
            }
            let WhisperInitState {
                mux,
                tun,
                mtu,
                socketaddr,
            } = whisper.0.take().ok_or(WhisperError::NotInitialized)?;
            let (channel, rx) = unbounded_channel();
            whisper.1.replace(WhisperRunningState {
                channel,
                socketaddr,
            });
            start_whisper(mux, tun, mtu, rx)
                .await
                .map_err(WhisperError::Other)
        })
        .is_ok()
    } else {
        false
    }
}

#[no_mangle]
pub extern "C" fn whisper_stop() -> bool {
    if let Ok(rt) = Runtime::new() {
        rt.block_on(async {
            let mut whisper = WHISPER.lock().await;
            if whisper.1.is_none() {
                return Err(WhisperError::NotStarted);
            }
            let WhisperRunningState { channel, .. } =
                whisper.1.take().ok_or(WhisperError::NotInitialized)?;
            channel
                .send(WhisperEvent::EndFut)
                .map_err(WhisperError::other)?;
            Ok(())
        })
        .is_ok()
    } else {
        false
    }
}