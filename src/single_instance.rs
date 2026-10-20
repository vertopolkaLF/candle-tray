//! Named-mutex ownership and a dedicated message-only activation window.
//! Activation callbacks run on `candle-activation`, never on the GPUI thread;
//! marshal work to the UI yourself. Early activations are retained until a hook
//! is installed. Dropping the guard stops the window thread and releases ownership.
use anyhow::Result;
#[cfg(windows)]
mod platform {
    use super::*;
    use anyhow::{Context, ensure};
    use std::{
        ptr,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND,
            LPARAM, LRESULT, WPARAM,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::CreateMutexW},
        UI::WindowsAndMessaging::*,
    };
    const CLASS: &str = "CandleTray.Activation";
    type Callback = Arc<dyn Fn() + Send + Sync>;
    struct ContextData {
        callback: Mutex<Option<Callback>>,
        pending: AtomicBool,
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
    pub fn acquire() -> Result<Option<SingleInstance>> {
        acquire_named("Local\\CandleTray.SingleInstance", CLASS)
    }
    fn acquire_named(mutex_name: &str, class_name: &str) -> Result<Option<SingleInstance>> {
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
                        ensure!(
                            unsafe { PostMessageW(window, message, 0, 0) } != 0,
                            "post activation: {}",
                            std::io::Error::last_os_error()
                        );
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
            pending: AtomicBool::new(false),
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
                SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
            }
        }
        let data = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const ContextData;
        if !data.is_null() && message == unsafe { (*data).message } {
            let data = unsafe { &*data };
            if wp == 0 {
                data.pending.store(true, Ordering::Release);
            }
            let callback = data
                .callback
                .lock()
                .expect("activation callback lock")
                .clone();
            if let Some(callback) = callback
                && data.pending.swap(false, Ordering::AcqRel)
            {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback()));
                if result.is_err() {
                    crate::logger::error("Activation callback panicked");
                }
            }
            return 0;
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
    impl SingleInstance {
        pub fn acquire() -> Result<Option<Self>> {
            acquire()
        }
        pub fn set_activation_callback(&self, callback: impl Fn() + Send + Sync + 'static) {
            *self
                .context
                .callback
                .lock()
                .expect("activation callback lock") = Some(Arc::new(callback));
            if self.context.pending.load(Ordering::Acquire) {
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
        fn secondary_activates_the_window_thread_and_guard_releases_ownership() {
            let namespace = format!(
                "CandleTray.Test.{}.{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            );
            let mutex = format!("Local\\{namespace}");
            let primary = acquire_named(&mutex, &namespace).unwrap().unwrap();
            // The primary window exists before acquire returns. Activation before
            // hook registration is retained until the hook can deliver it.
            assert!(acquire_named(&mutex, &namespace).unwrap().is_none());
            let (tx, rx) = mpsc::channel();
            primary.set_activation_callback(move || {
                tx.send(thread::current().name().unwrap().to_owned())
                    .unwrap();
            });
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(3)).unwrap(),
                "candle-activation"
            );
            assert!(acquire_named(&mutex, &namespace).unwrap().is_none());
            rx.recv_timeout(Duration::from_secs(3)).unwrap();
            drop(primary);
            assert!(acquire_named(&mutex, &namespace).unwrap().is_some());
        }
    }
}
#[cfg(windows)]
pub use platform::{SingleInstance, acquire};
#[cfg(not(windows))]
pub struct SingleInstance;
#[cfg(not(windows))]
pub fn acquire() -> Result<Option<SingleInstance>> {
    anyhow::bail!("single instance requires Windows")
}
