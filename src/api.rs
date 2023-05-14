#![cfg(not(target_os = "android"))]

use crate::error::{Error, Result};
use crate::LOCAL_HOST_V4;
use std::os::raw::{c_char, c_int, c_void};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[derive(Clone)]
struct CCallback(Option<unsafe extern "C" fn(c_int, *mut c_void)>, *mut c_void);

impl CCallback {
    unsafe fn call(self, arg: c_int) {
        if let Some(cb) = self.0 {
            cb(arg, self.1);
        }
    }
}

unsafe impl Send for CCallback {}
unsafe impl Sync for CCallback {}

lazy_static::lazy_static! {
    static ref EXITING_FLAG: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    static ref LISTEN_ADDR: Arc<Mutex<SocketAddr>> = Arc::new(Mutex::new(format!("{}:0", LOCAL_HOST_V4).parse::<SocketAddr>().unwrap()));
}

/// # Safety
///
/// Run the overtls client with config file.
#[no_mangle]
pub unsafe extern "C" fn over_tls_client_run(
    config_path: *const c_char,
    verbose: c_char,
    callback: Option<unsafe extern "C" fn(c_int, *mut c_void)>,
    ctx: *mut c_void,
) -> c_int {
    let ccb = CCallback(callback, ctx);

    let block = || -> Result<()> {
        let config_path = std::ffi::CStr::from_ptr(config_path).to_str()?;
        let log_level = if verbose != 0 { "trace" } else { "info" };
        let root = module_path!().split("::").next().ok_or("module path error")?;
        let default = format!("off,{}={}", root, log_level);
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default)).init();

        let cb = |addr: SocketAddr| {
            log::trace!("Listening on {}", addr);
            let port = addr.port();
            let addr = format!("{}:{}", LOCAL_HOST_V4, port).parse::<SocketAddr>().unwrap();
            *LISTEN_ADDR.lock().unwrap() = addr;
            unsafe {
                ccb.call(port as c_int);
            }
        };

        let mut config = crate::config::Config::from_config_file(config_path)?;
        config.check_correctness(false)?;
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        rt.block_on(async {
            EXITING_FLAG.store(false, Ordering::SeqCst);
            crate::client::run_client(&config, Some(EXITING_FLAG.clone()), Some(cb)).await?;
            Ok::<(), Error>(())
        })
    };
    if let Err(error) = block() {
        log::error!("failed to run client, error={:?}", error);
        return -1;
    }
    0
}

/// # Safety
///
/// Shutdown the client.
#[no_mangle]
pub unsafe extern "C" fn over_tls_client_stop() -> c_int {
    EXITING_FLAG.store(true, Ordering::SeqCst);

    let l_addr = *LISTEN_ADDR.lock().unwrap();
    let addr = if l_addr.is_ipv6() { "::1" } else { LOCAL_HOST_V4 };
    let _ = std::net::TcpStream::connect((addr, l_addr.port()));
    log::trace!("Client stop on listen address {}", l_addr);
    0
}
