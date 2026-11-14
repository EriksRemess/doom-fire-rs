use super::INTERRUPTED;
use crate::terminal::TermSize;
use std::sync::atomic::Ordering;
use std::{cmp, ffi::c_void, io, ptr};

type Bool = i32;
type Dword = u32;
type Handle = *mut c_void;
type Short = i16;
type Uint = u32;
type Word = u16;

pub(crate) struct Console {
    handle: Handle,
    original_mode: Dword,
    original_code_page: Uint,
}

pub(crate) struct Keyboard {
    handle: Handle,
    original_mode: Dword,
    worker: Option<super::InputWorker>,
}

impl Keyboard {
    pub(crate) fn finish(mut self) -> io::Result<()> {
        self.worker
            .take()
            .map_or(Ok(()), |mut worker| worker.finish())
    }

    pub(crate) fn start() -> io::Result<Self> {
        let handle = unsafe { GetStdHandle(-10_i32 as Dword) };
        let mut original_mode = 0;
        if unsafe { GetConsoleMode(handle, &mut original_mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // Retain processed input (Ctrl-C), disable line input and echo.
        if unsafe { SetConsoleMode(handle, original_mode & !(0x0002 | 0x0004)) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut keyboard = Self {
            handle,
            original_mode,
            worker: None,
        };
        let input = handle as usize;
        keyboard.worker = Some(super::InputWorker::start(move || {
            let handle = input as Handle;
            let mut count = 0;
            if unsafe { GetNumberOfConsoleInputEvents(handle, &mut count) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                return Ok(false);
            }
            let mut record = InputRecord {
                kind: 0,
                key: KeyEventRecord::default(),
            };
            if unsafe { ReadConsoleInputW(handle, &mut record, 1, &mut count) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(count == 1
                && record.kind == 1
                && record.key.down != 0
                && record.key.character == u16::from(b'q'))
        })?);
        Ok(keyboard)
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        drop(self.worker.take());
        unsafe {
            SetConsoleMode(self.handle, self.original_mode);
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct KeyEventRecord {
    down: Bool,
    repeat: Word,
    virtual_key: Word,
    scan_code: Word,
    character: Word,
    control_state: Dword,
}

// KEY_EVENT_RECORD is the largest member of the INPUT_RECORD event union.
#[repr(C)]
struct InputRecord {
    kind: Word,
    key: KeyEventRecord,
}

impl Drop for Console {
    fn drop(&mut self) {
        unsafe {
            SetConsoleOutputCP(self.original_code_page);
            SetConsoleMode(self.handle, self.original_mode);
        }
    }
}

const STD_OUTPUT_HANDLE: Dword = -11_i32 as Dword;
const ENABLE_PROCESSED_OUTPUT: Dword = 0x0001;
const ENABLE_VIRTUAL_TERMINAL_PROCESSING: Dword = 0x0004;
const DISABLE_NEWLINE_AUTO_RETURN: Dword = 0x0008;
const CP_UTF8: Uint = 65001;
const CTRL_C_EVENT: Dword = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct Coord {
    x: Short,
    y: Short,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmallRect {
    left: Short,
    top: Short,
    right: Short,
    bottom: Short,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ConsoleScreenBufferInfo {
    dw_size: Coord,
    dw_cursor_position: Coord,
    w_attributes: Word,
    sr_window: SmallRect,
    dw_maximum_window_size: Coord,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(nStdHandle: Dword) -> Handle;
    fn GetConsoleMode(hConsoleHandle: Handle, lpMode: *mut Dword) -> Bool;
    fn GetNumberOfConsoleInputEvents(handle: Handle, count: *mut Dword) -> Bool;
    fn ReadConsoleInputW(
        handle: Handle,
        record: *mut InputRecord,
        length: Dword,
        count: *mut Dword,
    ) -> Bool;
    fn SetConsoleMode(hConsoleHandle: Handle, dwMode: Dword) -> Bool;
    fn GetConsoleOutputCP() -> Uint;
    fn SetConsoleOutputCP(wCodePageID: Uint) -> Bool;
    fn GetConsoleScreenBufferInfo(
        hConsoleOutput: Handle,
        lpConsoleScreenBufferInfo: *mut ConsoleScreenBufferInfo,
    ) -> Bool;
    fn WriteConsoleA(
        hConsoleOutput: Handle,
        lpBuffer: *const c_void,
        nNumberOfCharsToWrite: Dword,
        lpNumberOfCharsWritten: *mut Dword,
        lpReserved: *mut c_void,
    ) -> Bool;
    fn SetConsoleCtrlHandler(
        HandlerRoutine: Option<unsafe extern "system" fn(Dword) -> Bool>,
        Add: Bool,
    ) -> Bool;
}

unsafe extern "system" fn handle_ctrl_c(ctrl_type: Dword) -> Bool {
    if ctrl_type != CTRL_C_EVENT {
        return 0;
    }

    INTERRUPTED.store(true, Ordering::Relaxed);
    1
}

pub(crate) fn install_exit_handlers() -> io::Result<()> {
    if unsafe { SetConsoleCtrlHandler(Some(handle_ctrl_c), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

pub(crate) fn init_console() -> io::Result<Console> {
    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if handle.is_null() || handle == (-1_isize as Handle) {
        return Err(io::Error::last_os_error());
    }

    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let original_code_page = unsafe { GetConsoleOutputCP() };
    if original_code_page == 0 {
        return Err(io::Error::last_os_error());
    }
    // Create the guard before the first mutation so setup failures also restore state.
    let console = Console {
        handle,
        original_mode: mode,
        original_code_page,
    };
    mode |=
        ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        return Err(io::Error::last_os_error());
    }

    if unsafe { SetConsoleOutputCP(CP_UTF8) } == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(console)
}

pub(crate) fn term_size(console: &Console) -> io::Result<TermSize> {
    let mut info = ConsoleScreenBufferInfo {
        dw_size: Coord { x: 0, y: 0 },
        dw_cursor_position: Coord { x: 0, y: 0 },
        w_attributes: 0,
        sr_window: SmallRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        dw_maximum_window_size: Coord { x: 0, y: 0 },
    };

    if unsafe { GetConsoleScreenBufferInfo(console.handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(TermSize {
        height: (i32::from(info.sr_window.bottom) - i32::from(info.sr_window.top) + 1) as usize,
        width: (i32::from(info.sr_window.right) - i32::from(info.sr_window.left) + 1) as usize,
    })
}

pub(crate) fn write_console(console: &Console, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let chunk_len = cmp::min(bytes.len(), Dword::MAX as usize);
        let mut written = 0;
        let ok = unsafe {
            WriteConsoleA(
                console.handle,
                bytes.as_ptr().cast(),
                chunk_len as Dword,
                &mut written,
                ptr::null_mut(),
            )
        };

        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "WriteConsoleA wrote zero bytes",
            ));
        }

        bytes = &bytes[written as usize..];
    }

    Ok(())
}
