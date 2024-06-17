use overtls::{run_client, run_server, BoxError, CmdOpt, Config, Error, Result};

fn main() -> Result<(), BoxError> {
    let opt = CmdOpt::parse_cmd();

    if opt.c_api {
        if opt.is_server() {
            return Err("C API is not supported for server".into());
        }

        let join = ctrlc2::set_handler(|| {
            log::info!("Ctrl-C received, exiting...");
            unsafe { overtls::over_tls_client_stop() };
            true
        })?;

        unsafe extern "C" fn log_cb(_: overtls::ArgVerbosity, msg: *const std::os::raw::c_char, _ctx: *mut std::os::raw::c_void) {
            println!("{:?}", unsafe { std::ffi::CStr::from_ptr(msg).to_str() });
        }
        unsafe { overtls::overtls_set_log_callback(true, Some(log_cb), std::ptr::null_mut()) };

        unsafe extern "C" fn port_cb(port: i32, _ctx: *mut std::os::raw::c_void) {
            log::info!("Listening on {}", port);
        }

        if let Some(cfg) = opt.config.as_ref() {
            // Test the C API usage
            let config_path_str = cfg.as_path().to_string_lossy().into_owned();
            let c_string = std::ffi::CString::new(config_path_str)?;
            let config_path: *const std::os::raw::c_char = c_string.as_ptr();

            unsafe { overtls::over_tls_client_run(config_path, opt.verbosity, Some(port_cb), std::ptr::null_mut()) };

            join.join().expect("Couldn't join on the associated thread");
        } else if let Some(url) = opt.url_of_node.as_ref() {
            let url_str = std::ffi::CString::new(url.as_str())?;
            let url_ptr = url_str.as_ptr();

            let listen_addr = opt.listen_addr.unwrap_or(std::net::SocketAddr::from(([127, 0, 0, 1], 1080)));
            let listen_addr = std::ffi::CString::new(listen_addr.to_string())?;
            let listen_addr = listen_addr.as_ptr();

            unsafe { overtls::over_tls_client_run_with_ssr_url(url_ptr, listen_addr, opt.verbosity, Some(port_cb), std::ptr::null_mut()) };

            join.join().expect("Couldn't join on the associated thread");
        } else {
            return Err("Config file or node URL is required".into());
        }
        return Ok(());
    }

    dotenvy::dotenv().ok();

    let level = format!("{}={:?}", module_path!(), opt.verbosity);
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();

    let is_server = opt.is_server();

    let mut config = if let Some(file) = opt.config {
        Config::from_config_file(file)?
    } else if let Some(ref url_of_node) = opt.url_of_node {
        let mut cfg = Config::from_ssr_url(url_of_node)?;
        cfg.set_listen_addr(opt.listen_addr.unwrap_or(std::net::SocketAddr::from(([127, 0, 0, 1], 1080))));
        cfg
    } else {
        return Err("Config file or node URL is required".into());
    };
    config.set_cache_dns(opt.cache_dns);

    if opt.generate_url {
        let url = config.generate_ssr_url()?;
        println!("{}", url);
        return Ok(());
    }

    config.check_correctness(is_server)?;

    #[cfg(unix)]
    if opt.daemonize {
        let stdout = std::fs::File::create("/tmp/overtls.out")?;
        let stderr = std::fs::File::create("/tmp/overtls.err")?;
        let daemonize = daemonize::Daemonize::new()
            .working_directory("/tmp")
            .umask(0o777)
            .stdout(stdout)
            .stderr(stderr)
            .privileged_action(|| "Executed before drop privileges");
        let _ = daemonize.start()?;
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async_main(config))?;
    Ok(())
}

async fn async_main(config: Config) -> Result<()> {
    let shutdown_token = overtls::CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    let main_body = async {
        if config.is_server {
            if config.exist_server() {
                run_server(&config, shutdown_token_clone).await?;
            } else {
                return Err(Error::from("Config is not a server config"));
            }
        } else if config.exist_client() {
            let callback = |addr| {
                log::trace!("Listening on {}", addr);
            };
            run_client(&config, shutdown_token_clone, Some(callback)).await?;
        } else {
            return Err("Config is not a client config".into());
        }

        Ok(())
    };

    ctrlc2::set_async_handler(async move {
        log::info!("Ctrl-C received, exiting...");
        shutdown_token.cancel();
    })
    .await;

    if let Err(e) = main_body.await {
        log::error!("main_body error: \"{}\"", e);
    }

    Ok(())
}
