use std::sync::atomic::{AtomicBool, Ordering};
use std::{io, sync::Arc, thread};

struct InputWorker {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<io::Result<()>>>,
}

impl InputWorker {
    fn start(mut read_quit: impl FnMut() -> io::Result<bool> + Send + 'static) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = thread::Builder::new()
            .name("keyboard".into())
            .spawn(move || {
                while !stopped.load(Ordering::Relaxed) && !interrupted() {
                    match read_quit() {
                        Ok(false) => {}
                        result @ (Ok(true) | Err(_)) => {
                            request_exit();
                            #[cfg(unix)]
                            resume_output();
                            return result.map(|_| ());
                        }
                    }
                }
                Ok(())
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    fn finish(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().map_or(Ok(()), |thread| {
            thread
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("keyboard worker panicked")))
        })
    }
}

impl Drop for InputWorker {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

pub(crate) fn reset_interrupt() {
    INTERRUPTED.store(false, Ordering::Relaxed);
}

pub(crate) fn request_exit() {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_worker_preserves_read_errors_and_normal_quit() {
        let _reset = std::mem::DropGuard::new((), |()| reset_interrupt());
        for fail in [true, false] {
            reset_interrupt();
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut worker = InputWorker::start(move || {
                sender.send(()).unwrap();
                if fail {
                    Err(io::Error::other("test input failure"))
                } else {
                    Ok(true)
                }
            })
            .unwrap();
            receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            let result = worker.finish();
            assert!(interrupted());
            if fail {
                let error = result.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::Other);
                assert_eq!(error.to_string(), "test input failure");
            } else {
                result.unwrap();
            }
            worker.finish().unwrap();
        }
    }
}
