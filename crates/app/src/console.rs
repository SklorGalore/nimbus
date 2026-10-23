//! The Windows console: why Nimbus has none of its own, and how terminal
//! output still works.
//!
//! Every Windows program is marked, in its file header, as either a
//! *console* program or a *GUI* program (its "subsystem"). Windows gives a
//! console program a console window as it starts unless it was started from
//! one. So a console-subsystem Nimbus opened from the Start menu, Explorer
//! or Open With came with an empty black window beside the editor. `main.rs`
//! marks Nimbus as a GUI program (`windows_subsystem = "windows"`), and no
//! console is made.
//!
//! A GUI program started from a terminal is not connected to that terminal
//! either, though, so `nimbus --version` would print nothing. [`attach`],
//! the first thing `main` does, connects to the terminal that started
//! Nimbus, if there is one (`AttachConsole`). Output the caller already sent
//! somewhere (`nimbus --help > file`, or a pipe from git or a test) has its
//! own handles and is left alone.
//!
//! A run that goes on to open the editor window lets go of the terminal
//! again ([`release`]). A terminal does not wait for a GUI program, so its
//! prompt is already back; staying connected would let a Ctrl+C typed there
//! later, or closing the terminal, end the editor and lose unsaved work.
//!
//! Two things differ from a console program, and both are how Windows
//! treats every GUI program: Command Prompt and `PowerShell` show their
//! prompt again without waiting, so output such as `--version` appears
//! after it, and `nimbus --wait` typed at a prompt does not hold it (git and
//! other programs that start an editor do wait, as they wait for the
//! process itself).
//!
//! On macOS and Linux both functions do nothing: neither system makes a
//! terminal window for a program, however it is started.

#[cfg(windows)]
pub use windows::{attach, release};

/// Connect to the terminal that started Nimbus, if any, so `print_out`,
/// `print_err` and logging reach it. Nothing to do on this system.
#[cfg(not(windows))]
pub fn attach() {}

/// Disconnect from the terminal [`attach`] connected to, for a run that
/// becomes the editor window. Nothing to do on this system.
#[cfg(not(windows))]
pub fn release() {}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "kernel32's console functions; no dependency wraps them"
)]
mod windows {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// `AttachConsole`'s "the console of the process that started us".
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;

    /// `STD_INPUT_HANDLE`, `STD_OUTPUT_HANDLE` and `STD_ERROR_HANDLE`: -10,
    /// -11 and -12 as the unsigned `DWORD` the functions take.
    const STD_HANDLES: [u32; 3] = [0xFFFF_FFF6, 0xFFFF_FFF5, 0xFFFF_FFF4];

    /// Which of [`STD_HANDLES`] (bit 0, 1, 2) had no handle until
    /// [`attach`] connected the terminal, so [`release`] clears exactly
    /// those and never one the caller gave us. Zero when not attached.
    static FILLED: AtomicU8 = AtomicU8::new(0);

    // SAFETY: these match kernel32's declarations in the Windows SDK (BOOL
    // is i32, DWORD is u32, HANDLE is a pointer-sized value). The three
    // marked `safe` take no pointer they read or write through, so any
    // argument is sound: a bad one is an error code, not memory misuse.
    // `SetStdHandle` stays unsafe because it changes the handle that
    // Rust's `std::io::stdout` and friends borrow.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        safe fn AttachConsole(process: u32) -> i32;
        safe fn FreeConsole() -> i32;
        safe fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
    }

    /// Connect to the terminal that started Nimbus, if any, so `print_out`,
    /// `print_err` and logging reach it. Started from Explorer or the Start
    /// menu there is none, and this does nothing. Rust's `std::io` looks
    /// up the handle on every write, so later output finds the terminal.
    pub fn attach() {
        let mut empty = 0;
        for (bit, which) in STD_HANDLES.into_iter().enumerate() {
            let handle = GetStdHandle(which);
            // Null: none given. All ones: INVALID_HANDLE_VALUE.
            if handle.is_null() || handle.addr() == usize::MAX {
                empty |= 1 << bit;
            }
        }
        // Everything already goes somewhere the caller chose.
        if empty == 0 {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            FILLED.store(empty, Ordering::Relaxed);
        }
    }

    /// Disconnect from the terminal [`attach`] connected to, for a run that
    /// becomes the editor window, so the terminal's Ctrl+C and close no
    /// longer reach it. Its handles are cleared first: a closed handle's
    /// number can be reused by a file Nimbus opens later, and a stray log
    /// line must not land in it. With no handle, `std::io` drops writes.
    pub fn release() {
        let filled = FILLED.swap(0, Ordering::Relaxed);
        if filled == 0 {
            return;
        }
        for (bit, which) in STD_HANDLES.into_iter().enumerate() {
            if filled & (1 << bit) != 0 {
                // SAFETY: null is what a GUI program's standard handles
                // are when it has no console, and `std::io` treats it as
                // "no output" (writes succeed and go nowhere). Only
                // handles that `attach` filled in are cleared, so one the
                // caller passed (a pipe, a file) is never touched.
                unsafe {
                    SetStdHandle(which, std::ptr::null_mut());
                }
            }
        }
        FreeConsole();
    }
}
