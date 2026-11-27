//! Webcam capture for the overlay. One thread per opened camera keeps the newest picture; the UI
//! takes it when it draws. See docs/DEVELOPMENT.md, stage 8.

pub mod align;
pub mod blend;
mod mf;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

pub use blend::Picture;

/// A video capture device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    /// Windows device path; what the settings store.
    pub link: String,
}

/// Video capture devices Windows knows. Blocks for a moment: call it off the UI thread.
pub fn list_devices() -> anyhow::Result<Vec<Device>> {
    mf::enumerate()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Opening,
    Running {
        width: usize,
        height: usize,
    },
    /// The thread has ended; the text is for the user.
    Failed(String),
}

struct Shared {
    latest: Mutex<Option<Arc<Picture>>>,
    state: Mutex<State>,
    stop: AtomicBool,
}

/// Lock even if a thread panicked while holding the lock; the data is replaced whole.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An open webcam. Dropping it stops the camera.
pub struct Webcam {
    link: String,
    shared: Arc<Shared>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

/// How long dropping a [`Webcam`] waits for its thread (one frame normally takes 33 ms).
const STOP_WAIT: Duration = Duration::from_secs(2);

impl Webcam {
    /// Start the camera with this device path. `wake` is called after every new picture and when
    /// the state changes.
    pub fn open(link: &str, wake: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            latest: Mutex::new(None),
            state: Mutex::new(State::Opening),
            stop: AtomicBool::new(false),
        });
        let (done_tx, done) = mpsc::channel();
        let (path, sh) = (link.to_owned(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("webcam".into())
            .spawn(move || {
                if let Err(e) = run(&path, &sh, &wake) {
                    log::warn!("webcam: {e:#}");
                    *lock(&sh.state) = State::Failed(describe(&e));
                    wake();
                }
                let _ = done_tx.send(());
            });
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                *lock(&shared.state) = State::Failed(format!("스레드를 만들 수 없습니다: {e}"));
                None
            }
        };
        Self {
            link: link.to_owned(),
            shared,
            done,
            thread,
        }
    }

    /// A camera that "runs" and always shows `picture`, without a thread or device.
    #[cfg(test)]
    pub(crate) fn fake(link: &str, picture: Picture) -> Self {
        let (width, height) = (picture.width, picture.height);
        let (_, done) = mpsc::channel();
        Self {
            link: link.to_owned(),
            shared: Arc::new(Shared {
                latest: Mutex::new(Some(Arc::new(picture))),
                state: Mutex::new(State::Running { width, height }),
                stop: AtomicBool::new(false),
            }),
            done,
            thread: None,
        }
    }

    pub fn link(&self) -> &str {
        &self.link
    }

    pub fn state(&self) -> State {
        lock(&self.shared.state).clone()
    }

    /// The newest picture, if any arrived.
    pub fn latest(&self) -> Option<Arc<Picture>> {
        lock(&self.shared.latest).clone()
    }
}

impl Drop for Webcam {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let Some(t) = self.thread.take() else {
            return;
        };
        match self.done.recv_timeout(STOP_WAIT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = t.join();
            }
            // A camera that stopped answering: leave the thread, do not freeze the UI.
            Err(RecvTimeoutError::Timeout) => log::warn!("webcam thread did not stop in time"),
        }
    }
}

fn run(link: &str, shared: &Shared, wake: &dyn Fn()) -> anyhow::Result<()> {
    let mut reader = mf::Reader::open(link)?;
    let (width, height) = reader.size();
    *lock(&shared.state) = State::Running { width, height };
    log::info!("webcam: {width}×{height}");
    wake();
    let mut seq = 0u64;
    while !shared.stop.load(Ordering::Relaxed) {
        let Some((w, h, rgba)) = reader.read()? else {
            continue;
        };
        seq += 1;
        let picture = Arc::new(Picture::new(seq, w, h, rgba));
        *lock(&shared.latest) = Some(picture);
        if (w, h) != (width, height) {
            *lock(&shared.state) = State::Running {
                width: w,
                height: h,
            };
        }
        wake();
    }
    Ok(())
}

/// Message for the user. Access denied means Windows' camera privacy setting blocks desktop apps.
fn describe(e: &anyhow::Error) -> String {
    let denied = e.chain().any(|c| {
        c.downcast_ref::<windows::core::Error>()
            .is_some_and(|w| w.code() == windows::Win32::Foundation::E_ACCESSDENIED)
    });
    if denied {
        "Windows가 카메라 사용을 막았습니다. 설정 > 개인 정보 및 보안 > 카메라에서 \
         '데스크톱 앱이 카메라에 액세스하도록 허용'을 켜 주세요."
            .into()
    } else {
        format!("웹캠 오류: {e:#}")
    }
}
