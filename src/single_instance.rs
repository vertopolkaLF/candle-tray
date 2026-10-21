//! Named-mutex ownership and a dedicated message-only activation window.
//! Activation callbacks run on `candle-activation`, never on the GPUI thread;
//! marshal work to the UI yourself. Early activations are retained until a hook
//! is installed. Dropping the guard stops the window thread and releases ownership.
use anyhow::Result;
#[cfg(windows)]
mod platform {
    use super::*;
    use crate::cli::Command;
    use anyhow::{Context, ensure};
    use std::{
        collections::VecDeque,
        ptr,
        sync::{Arc, Mutex, mpsc},
        thread,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND,
            LPARAM, LRESULT, WPARAM,
        },
        System::{
            DataExchange::COPYDATASTRUCT, LibraryLoader::GetModuleHandleW, Threading::CreateMutexW,
        },
        UI::WindowsAndMessaging::*,
    };
    const CLASS: &str = "CandleTray.Activation";
    const PAYLOAD_TAG: usize = 0x434e4431; // CND1, command protocol version 1.
    const MAX_PAYLOAD: usize = 64 * 1024;
    type Callback = Arc<dyn Fn(Option<Command>) + Send + Sync>;
    struct ContextData {
        callback: Mutex<Option<Callback>>,
        pending: Mutex<VecDeque<Option<Command>>>,
        message: u32,
    }
    pub struct SingleInstance {
        mutex: isize,
        window: isize,
        context: Arc<ContextData>,
        thread: Option<thread::JoinHandle<()>>,
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    pub fn acquire(command: Option<&Command>) -> Result<Option<SingleInstance>> {
        acquire_named("Local\\CandleTray.SingleInstance", CLASS, command)
    }
    fn acquire_named(
        mutex_name: &str,
        class_name: &str,
        command: Option<&Command>,
    ) -> Result<Option<SingleInstance>> {
        let payload = command.map(serde_json::to_vec).transpose()?;
        ensure!(
            payload
                .as_ref()
                .is_none_or(|bytes| bytes.len() <= MAX_PAYLOAD),
            "CLI payload too large"
        );
        let message = unsafe { RegisterWindowMessageW(wide(class_name).as_ptr()) };
        ensure!(
            message != 0,
            "register activation message: {}",
            std::io::Error::last_os_error()
        );
        let handle = unsafe { CreateMutexW(ptr::null(), 0, wide(mutex_name).as_ptr()) };
        let status = unsafe { GetLastError() };
        ensure!(
            !handle.is_null(),
            "create instance mutex: {}",
            std::io::Error::last_os_error()
        );
        if status == ERROR_ALREADY_EXISTS {
            let result = (|| {
                for _ in 0..100 {
                    let window = unsafe {
                        FindWindowExW(
                            HWND_MESSAGE,
                            ptr::null_mut(),
                            wide(class_name).as_ptr(),
                            ptr::null(),
                        )
                    };
                    if !window.is_null() {
                        if let Some(payload) = payload.as_ref() {
                            let data = COPYDATASTRUCT {
                                dwData: PAYLOAD_TAG,
                                cbData: payload.len() as u32,
                                lpData: payload.as_ptr() as *mut _,
                            };
                            let mut accepted = 0;
                            // COPYDATA owns the cross-process copy only during
                            // this synchronous call; the receiver deserializes it.
                            let sent = unsafe {
                                SendMessageTimeoutW(
                                    window,
                                    WM_COPYDATA,
                                    0,
                                    &data as *const _ as isize,
                                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                                    3000,
                                    &mut accepted,
                                )
                            };
                            ensure!(
                                sent != 0 && accepted == 1,
                                "running instance did not accept CLI command"
                            );
                        } else {
                            ensure!(
                                unsafe { PostMessageW(window, message, 0, 0) } != 0,
                                "post activation: {}",
                                std::io::Error::last_os_error()
                            );
                        }
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                anyhow::bail!("primary activation window did not become ready")
            })();
            unsafe {
                CloseHandle(handle);
            }
            return result;
        }
        let context = Arc::new(ContextData {
            callback: Mutex::new(None),
            pending: Mutex::new(VecDeque::new()),
            message,
        });
        let data = context.clone();
        let (ready, receiver) = mpsc::sync_channel(1);
        let class_name = class_name.to_owned();
        let worker = thread::Builder::new()
            .name("candle-activation".into())
            .spawn(move || {
                let create = || -> Result<isize> {
                    let module = unsafe { GetModuleHandleW(ptr::null()) };
                    let class = wide(&class_name);
                    let wc = WNDCLASSW {
                        lpfnWndProc: Some(window_proc),
                        hInstance: module,
                        lpszClassName: class.as_ptr(),
                        ..unsafe { std::mem::zeroed() }
                    };
                    let atom = unsafe { RegisterClassW(&wc) };
                    ensure!(
                        atom != 0 || unsafe { GetLastError() } == ERROR_CLASS_ALREADY_EXISTS,
                        "register activation window class"
                    );
                    let window = unsafe {
                        CreateWindowExW(
                            0,
                            class.as_ptr(),
                            class.as_ptr(),
                            0,
                            0,
                            0,
                            0,
                            0,
                            HWND_MESSAGE,
                            ptr::null_mut(),
                            module,
                            Arc::as_ptr(&data).cast(),
                        )
                    };
                    ensure!(
                        !window.is_null(),
                        "create activation window: {}",
                        std::io::Error::last_os_error()
                    );
                    Ok(window as isize)
                };
                match create() {
                    Ok(window) => {
                        if ready.send(Ok(window)).is_err() {
                            unsafe {
                                DestroyWindow(window as HWND);
                            }
                            return;
                        }
                        let mut msg = unsafe { std::mem::zeroed() };
                        while unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) } > 0 {
                            unsafe {
                                TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(format!("{error:#}")));
                    }
                }
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                unsafe {
                    CloseHandle(handle);
                }
                return Err(error).context("spawn activation thread");
            }
        };
        match receiver.recv() {
            Ok(Ok(window)) => Ok(Some(SingleInstance {
                mutex: handle as isize,
                window,
                context,
                thread: Some(worker),
            })),
            result => {
                let _ = worker.join();
                unsafe {
                    CloseHandle(handle);
                }
                anyhow::bail!("activation window startup failed: {result:?}")
            }
        }
    }
    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wp: WPARAM,
        lp: LPARAM,
    ) -> LRESULT {
        if message == WM_NCCREATE {
            let create = unsafe { &*(lp as *const CREATESTRUCTW) };
            unsafe {
                SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as _);
            }
        }
        let data = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const ContextData;
        if !data.is_null() {
            let data = unsafe { &*data };
            if message == WM_COPYDATA {
                if lp == 0 {
                    return 0;
                }
                let payload = unsafe { &*(lp as *const COPYDATASTRUCT) };
                if payload.dwData != PAYLOAD_TAG
                    || payload.cbData == 0
                    || payload.cbData as usize > MAX_PAYLOAD
                    || payload.lpData.is_null()
                {
                    return 0;
                }
                let bytes = unsafe {
                    std::slice::from_raw_parts(payload.lpData.cast::<u8>(), payload.cbData as usize)
                };
                match serde_json::from_slice::<Command>(bytes) {
                    Ok(command) => {
                        data.pending
                            .lock()
                            .expect("activation queue lock")
                            .push_back(Some(command));
                        deliver_pending(data);
                        return 1;
                    }
                    Err(error) => {
                        crate::logger::error(format!("Invalid CLI payload: {error}"));
                        return 0;
                    }
                }
            }
            if message == data.message {
                if wp == 0 {
                    data.pending
                        .lock()
                        .expect("activation queue lock")
                        .push_back(None);
                }
                deliver_pending(data);
                return 0;
            }
        }
        if message == WM_CLOSE {
            unsafe {
                DestroyWindow(window);
            }
            return 0;
        }
        if message == WM_DESTROY {
            unsafe {
                PostQuitMessage(0);
            }
            return 0;
        }
        unsafe { DefWindowProcW(window, message, wp, lp) }
    }
    fn deliver_pending(data: &ContextData) {
        let callback = data
            .callback
            .lock()
            .expect("activation callback lock")
            .clone();
        if let Some(callback) = callback {
            let pending = std::mem::take(&mut *data.pending.lock().expect("activation queue lock"));
            for command in pending {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(command)));
                if result.is_err() {
                    crate::logger::error("Activation callback panicked");
                }
            }
        }
    }
    impl SingleInstance {
        pub fn acquire(command: Option<&Command>) -> Result<Option<Self>> {
            acquire(command)
        }
        pub fn set_activation_callback(
            &self,
            callback: impl Fn(Option<Command>) + Send + Sync + 'static,
        ) {
            *self
                .context
                .callback
                .lock()
                .expect("activation callback lock") = Some(Arc::new(callback));
            if !self
                .context
                .pending
                .lock()
                .expect("activation queue lock")
                .is_empty()
            {
                unsafe {
                    PostMessageW(self.window as HWND, self.context.message, 1, 0);
                }
            }
        }
    }
    impl Drop for SingleInstance {
        fn drop(&mut self) {
            unsafe {
                PostMessageW(self.window as HWND, WM_CLOSE, 0, 0);
            }
            if let Some(worker) = self.thread.take()
                && worker.thread().id() != thread::current().id()
            {
                let _ = worker.join();
            }
            unsafe {
                CloseHandle(self.mutex as _);
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn commands_queue_in_order_before_hook_and_forward_after_hook() {
            let namespace = format!(
                "CandleTray.PayloadTest.{}.{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            );
            let mutex = format!("Local\\{namespace}");
            let primary = acquire_named(&mutex, &namespace, None).unwrap().unwrap();
            let commands = [
                Command::Profile("????".into()),
                Command::Offset(-10, crate::cli::Selector::All),
            ];
            for command in &commands {
                assert!(
                    acquire_named(&mutex, &namespace, Some(command))
                        .unwrap()
                        .is_none()
                );
            }
            let (tx, rx) = mpsc::channel();
            primary.set_activation_callback(move |command| {
                tx.send(command).unwrap();
            });
            for command in &commands {
                assert_eq!(
                    rx.recv_timeout(Duration::from_secs(3)).unwrap(),
                    Some(command.clone())
                );
            }
            assert!(rx.try_recv().is_err());
            assert!(
                acquire_named(&mutex, &namespace, Some(&Command::Off))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(3)).unwrap(),
                Some(Command::Off)
            );
            // Invalid payloads are rejected without enqueueing a command.
            let bytes = b"not JSON";
            let payload = COPYDATASTRUCT {
                dwData: PAYLOAD_TAG,
                cbData: bytes.len() as u32,
                lpData: bytes.as_ptr() as *mut _,
            };
            let accepted = unsafe {
                SendMessageW(
                    primary.window as HWND,
                    WM_COPYDATA,
                    0,
                    &payload as *const _ as isize,
                )
            };
            assert_eq!(accepted, 0);
            assert!(rx.try_recv().is_err());
            assert!(
                acquire_named(
                    &mutex,
                    &namespace,
                    Some(&Command::Profile("x".repeat(MAX_PAYLOAD)))
                )
                .is_err()
            );
        }

        #[test]
        fn secondary_activates_the_window_thread_and_guard_releases_ownership() {
            let namespace = format!(
                "CandleTray.Test.{}.{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            );
            let mutex = format!("Local\\{namespace}");
            let primary = acquire_named(&mutex, &namespace, None).unwrap().unwrap();
            // The primary window exists before acquire returns. Activation before
            // hook registration is retained until the hook can deliver it.
            assert!(acquire_named(&mutex, &namespace, None).unwrap().is_none());
            let (tx, rx) = mpsc::channel();
            primary.set_activation_callback(move |_| {
                tx.send(thread::current().name().unwrap().to_owned())
                    .unwrap();
            });
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(3)).unwrap(),
                "candle-activation"
            );
            assert!(acquire_named(&mutex, &namespace, None).unwrap().is_none());
            rx.recv_timeout(Duration::from_secs(3)).unwrap();
            drop(primary);
            assert!(acquire_named(&mutex, &namespace, None).unwrap().is_some());
        }
    }
}
#[cfg(windows)]
pub use platform::{SingleInstance, acquire};
#[cfg(not(windows))]
pub struct SingleInstance;
#[cfg(not(windows))]
pub fn acquire(_command: Option<&crate::cli::Command>) -> Result<Option<SingleInstance>> {
    anyhow::bail!("single instance requires Windows")
}
