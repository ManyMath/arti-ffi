#[macro_use]
mod error;

use arti::proxy;
use arti_client::{DormantMode, TorClient, TorClientConfig};
use arti_client::config::CfgPath;
use lazy_static::lazy_static;
use std::ffi::{c_char, c_void, CStr, CString};
use std::{io, ptr};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tor_rtcompat::tokio::TokioNativeTlsRuntime;
use tor_config::Reconfigure;
use tor_rtcompat::ToplevelBlockOn;

lazy_static! {
    // Initialize a Tokio runtime once and reuse it throughout the application.
    static ref RUNTIME: io::Result<Runtime> = Builder::new_multi_thread().enable_all().build();
}

#[repr(C)]
pub struct Tor {
    client: *mut c_void,
    proxy: *mut c_void,
    progress_sender: *mut c_void,
    progress_receiver: *mut c_void,
}

/// Fraction + message. Caller frees `message` via `darti_free_string`.
#[repr(C)]
pub struct BootstrapStatus {
    pub progress: f32,
    pub message: *mut c_char,
}

fn start_proxy(
    port: u16,
    client: TorClient<TokioNativeTlsRuntime>,
    progress_sender: &mpsc::Sender<String>, // Borrow sender to avoid unnecessary cloning.
) -> JoinHandle<anyhow::Result<()>> {
    println!("Starting proxy!");
    let rt = RUNTIME.as_ref().unwrap(); // Assume runtime is initialized successfully.
    let progress_sender = progress_sender.clone(); // Clone inside async block to avoid multiple mutable borrows.
    rt.spawn(async move {
        progress_sender.send("Proxy started".to_string()).await.unwrap();
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
        let listener = tor_rtcompat::NetStreamProvider::listen(&client.runtime().clone(), &addr).await?;
        proxy::run_proxy_with_listeners(
            client.clone(),
            vec![listener],
            None,
        ).await
    })
}

#[no_mangle]
pub unsafe extern "C" fn arti_start(
    socks_port: u16,
    state_dir: *const c_char,
    cache_dir: *const c_char,
) -> Tor {
    let (progress_sender, progress_receiver) = mpsc::channel::<String>(100); // Create channel for progress updates.

    // Convert channels to raw pointers for FFI.
    let progress_sender_ptr = Box::into_raw(Box::new(progress_sender)) as *mut c_void;
    let progress_receiver_ptr = Box::into_raw(Box::new(progress_receiver)) as *mut c_void;

    // Error return value with initialized raw pointers.
    let err_ret = Tor {
        client: ptr::null_mut(),
        proxy: ptr::null_mut(),
        progress_sender: progress_sender_ptr,
        progress_receiver: progress_receiver_ptr,
    };

    // Convert C strings to Rust strings and handle errors.
    let state_dir = unwrap_or_return!(CStr::from_ptr(state_dir).to_str(), err_ret);
    let cache_dir = unwrap_or_return!(CStr::from_ptr(cache_dir).to_str(), err_ret);

    // Create a Tokio runtime to handle asynchronous tasks.
    let runtime = unwrap_or_return!(TokioNativeTlsRuntime::create(), err_ret);

    // Configure the Tor client.
    let mut cfg_builder = TorClientConfig::builder();
    cfg_builder
        .storage()
        .state_dir(CfgPath::new(state_dir.to_string()))
        .cache_dir(CfgPath::new(cache_dir.to_string()));
    cfg_builder.address_filter().allow_onion_addrs(true);

    // Build configuration or return an error.
    let cfg = unwrap_or_return!(cfg_builder.build(), err_ret);

    // Create and bootstrap the Tor client.
    let client = unwrap_or_return!(
        runtime.block_on(async {
            TorClient::with_runtime(runtime.clone())
                .config(cfg)
                .create_bootstrapped()
                .await
        }),
        err_ret
    );

    // Convert the raw sender pointer back to its original type.
    let progress_sender_ref = &*(progress_sender_ptr as *mut mpsc::Sender<String>);
    let proxy_handle_box = Box::new(start_proxy(socks_port, client.clone(), progress_sender_ref));
    let client_box = Box::new(client.clone());

    // Return initialized Tor struct with raw pointers.
    Tor {
        client: Box::into_raw(client_box) as *mut c_void,
        proxy: Box::into_raw(proxy_handle_box) as *mut c_void,
        progress_sender: progress_sender_ptr,
        progress_receiver: progress_receiver_ptr,
    }
}

#[no_mangle]
pub unsafe extern "C" fn arti_client_bootstrap(client: *mut c_void) -> bool {
    let client = &*(client as *mut TorClient<TokioNativeTlsRuntime>);
    unwrap_or_return!(client.runtime().block_on(client.bootstrap()), false);
    true
}

#[no_mangle]
pub unsafe extern "C" fn arti_client_set_dormant(client: *mut c_void, soft_mode: bool) {
    let client = &*(client as *mut TorClient<TokioNativeTlsRuntime>);
    let dormant_mode = if soft_mode {
        DormantMode::Soft
    } else {
        DormantMode::Normal
    };
    client.set_dormant(dormant_mode);
}

/// Free the TorClient. Pointer is invalid after this call.
#[no_mangle]
pub unsafe extern "C" fn arti_client_free(client: *mut c_void) {
    if client.is_null() { return; }
    let _ = Box::from_raw(client as *mut TorClient<TokioNativeTlsRuntime>);
}

/// Non-blocking bootstrap status. Caller frees `message` via `darti_free_string`.
#[no_mangle]
pub unsafe extern "C" fn arti_bootstrap_status(client: *mut c_void) -> BootstrapStatus {
    if client.is_null() {
        return BootstrapStatus {
            progress: 0.0,
            message: CString::new("Client is null").unwrap().into_raw(),
        };
    }
    let client = &*(client as *mut TorClient<TokioNativeTlsRuntime>);
    let status = client.bootstrap_status();
    let progress = status.as_frac();
    let message = CString::new(status.to_string()).unwrap().into_raw();
    BootstrapStatus { progress, message }
}

/// Apply new state/cache dirs. Returns false on error (check `arti_last_error_message`).
#[no_mangle]
pub unsafe extern "C" fn arti_reconfigure(
    client: *mut c_void,
    state_dir: *const c_char,
    cache_dir: *const c_char,
) -> bool {
    if client.is_null() { return false; }
    let client = &*(client as *mut TorClient<TokioNativeTlsRuntime>);
    let state_dir = unwrap_or_return!(CStr::from_ptr(state_dir).to_str(), false);
    let cache_dir = unwrap_or_return!(CStr::from_ptr(cache_dir).to_str(), false);

    let mut cfg_builder = TorClientConfig::builder();
    cfg_builder
        .storage()
        .state_dir(CfgPath::new(state_dir.to_string()))
        .cache_dir(CfgPath::new(cache_dir.to_string()));
    cfg_builder.address_filter().allow_onion_addrs(true);

    let cfg = unwrap_or_return!(cfg_builder.build(), false);
    unwrap_or_return!(client.reconfigure(&cfg, Reconfigure::AllOrNothing), false);
    true
}

/// Circuit-isolated client. Caller owns result, free with `arti_client_free`.
#[no_mangle]
pub unsafe extern "C" fn arti_isolated_client(client: *mut c_void) -> *mut c_void {
    if client.is_null() { return ptr::null_mut(); }
    let client = &*(client as *mut TorClient<TokioNativeTlsRuntime>);
    let isolated = client.isolated_client();
    Box::into_raw(Box::new(isolated)) as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn arti_proxy_stop(proxy: *mut c_void) {
    // Convert raw pointer back to the join handle.
    let proxy = unsafe {
        Box::from_raw(proxy as *mut JoinHandle<anyhow::Result<()>>)
    };

    proxy.abort(); // Stop the proxy.
}

#[no_mangle]
pub unsafe extern "C" fn arti_progress_next(tor: *mut Tor) -> *const c_char {
    let tor = &mut *tor; // Convert raw pointer back to reference.

    // Convert the raw pointer back to a receiver.
    let progress_receiver: &mut mpsc::Receiver<String> =
        &mut *(tor.progress_receiver as *mut mpsc::Receiver<String>);

    match progress_receiver.blocking_recv() {
        Some(progress) => CString::new(progress).unwrap().into_raw(),
        None => CString::new("No progress").unwrap().into_raw(),
    }
}

// Return a string to test FFI.
//
// Make sure to free the memory allocated by CString (use darti_free_string).
#[no_mangle]
pub extern "C" fn darti_hello() -> *mut c_char {
    let c_str = CString::new("Hello there").unwrap();
    c_str.into_raw() // Return raw pointer to the CString
}

// A helper function to free the memory allocated by CString.
//
// Make sure to call this function to free the memory allocated by CString.
#[no_mangle]
pub extern "C" fn darti_free_string(s: *mut c_char) {
    if s.is_null() { return; }
    unsafe {
        let _ = CString::from_raw(s); // This reclaims the CString and drops it, freeing the memory
    }
}
