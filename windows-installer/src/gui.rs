//! The installer window.
//!
//! Plain Win32 controls, declared by hand like the rest of this crate's FFI:
//! no runtime to install, nothing to download before the window opens, and
//! it looks like the Windows it runs on (the manifest built into the .exe by
//! build.rs turns on the current common controls and per-monitor DPI).
//!
//! Five pages: what this does; where it goes; what exactly will happen; the
//! work; what happens next. All of the work runs on a second thread through
//! `winstall`, the same functions the command line uses, so the window holds
//! no install logic of its own and cannot drift from what is tested.

#![cfg(windows)]
#![allow(clippy::upper_case_acronyms)]

use crate::plan;
use crate::winstall::{self, Candidate, Event};
use std::cell::RefCell;
use std::ffi::c_void;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, OnceLock};

type HWND = *mut c_void;
type HANDLE = *mut c_void;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;

#[repr(C)]
struct WNDCLASSEXW {
    size: u32,
    style: u32,
    proc_: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    cls_extra: i32,
    wnd_extra: i32,
    instance: HANDLE,
    icon: HANDLE,
    cursor: HANDLE,
    background: HANDLE,
    menu: *const u16,
    class_name: *const u16,
    icon_sm: HANDLE,
}
#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    time: u32,
    pt: [i32; 2],
    private: u32,
}
#[repr(C)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[repr(C)]
struct INITCOMMONCONTROLSEX {
    size: u32,
    icc: u32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassExW(c: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(ex: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: HANDLE, inst: HANDLE, param: *mut c_void) -> HWND;
    fn DefWindowProcW(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    fn GetMessageW(m: *mut MSG, h: HWND, a: u32, b: u32) -> i32;
    fn TranslateMessage(m: *const MSG) -> i32;
    fn DispatchMessageW(m: *const MSG) -> LRESULT;
    fn IsDialogMessageW(h: HWND, m: *mut MSG) -> i32;
    fn PostQuitMessage(code: i32);
    fn PostMessageW(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> i32;
    fn SendMessageW(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    fn SetWindowTextW(h: HWND, s: *const u16) -> i32;
    fn ShowWindow(h: HWND, cmd: i32) -> i32;
    fn EnableWindow(h: HWND, enable: i32) -> i32;
    fn MoveWindow(h: HWND, x: i32, y: i32, w: i32, h_: i32, repaint: i32) -> i32;
    fn MessageBoxW(h: HWND, text: *const u16, caption: *const u16, flags: u32) -> i32;
    fn LoadCursorW(inst: HANDLE, name: *const u16) -> HANDLE;
    fn GetDpiForWindow(h: HWND) -> u32;
    fn GetClientRect(h: HWND, r: *mut RECT) -> i32;
    fn SetWindowPos(h: HWND, after: HWND, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    fn GetSysColorBrush(i: i32) -> HANDLE;
    fn DestroyWindow(h: HWND) -> i32;
    fn SetFocus(h: HWND) -> HWND;
    fn IsWindowVisible(h: HWND) -> i32;
    fn IsWindowEnabled(h: HWND) -> i32;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateFontW(h: i32, w: i32, esc: i32, orient: i32, weight: i32, italic: u32, underline: u32, strike: u32, charset: u32, outp: u32, clip: u32, quality: u32, pitch: u32, face: *const u16) -> HANDLE;
    fn DeleteObject(h: HANDLE) -> i32;
    fn SetBkMode(dc: HANDLE, mode: i32) -> i32;
}
#[link(name = "comctl32")]
unsafe extern "system" {
    fn InitCommonControlsEx(i: *const INITCOMMONCONTROLSEX) -> i32;
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(h: HWND, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> HANDLE;
    fn IsUserAnAdmin() -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HANDLE;
    fn AttachConsole(pid: u32) -> i32;
    fn GetFirmwareType(t: *mut u32) -> i32;
}

const WS_CHILD: u32 = 0x4000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_MAIN: u32 = 0x00C0_0000 | 0x0008_0000 | 0x0002_0000; // caption, sysmenu, minimize
const WS_EX_CLIENTEDGE: u32 = 0x200;
const BS_DEFPUSHBUTTON: u32 = 1;
const BS_AUTOCHECKBOX: u32 = 3;
const ES_MULTILINE: u32 = 4;
const ES_AUTOVSCROLL: u32 = 0x40;
const ES_READONLY: u32 = 0x800;
const LBS_NOTIFY: u32 = 1;
const LBS_NOINTEGRALHEIGHT: u32 = 0x100;
const PBS_SMOOTH: u32 = 1;

const WM_DESTROY: u32 = 2;
const WM_CLOSE: u32 = 0x10;
const WM_SETFONT: u32 = 0x30;
const WM_COMMAND: u32 = 0x111;
const WM_CTLCOLORSTATIC: u32 = 0x138;
const WM_DPICHANGED: u32 = 0x2E0;
const DM_GETDEFID: u32 = 0x400;
const WM_APP_CANDIDATES: u32 = 0x8001;
const WM_APP_PROGRESS: u32 = 0x8002;
const WM_APP_DONE: u32 = 0x8003;
const LB_ADDSTRING: u32 = 0x180;
const LB_RESETCONTENT: u32 = 0x184;
const LB_SETCURSEL: u32 = 0x186;
const LB_GETCURSEL: u32 = 0x188;
const LBN_SELCHANGE: u16 = 1;
const BM_GETCHECK: u32 = 0xF0;
const BM_SETCHECK: u32 = 0xF1;
const PBM_SETPOS: u32 = 0x402;
const PBM_SETRANGE32: u32 = 0x406;
const EM_SETSEL: u32 = 0xB1;
const EM_REPLACESEL: u32 = 0xC2;
const MB_ICONERROR: u32 = 0x10;
const MB_ICONWARNING: u32 = 0x30;
const MB_YESNO: u32 = 4;
const IDYES: i32 = 6;

// Control ids.
const ID_BACK: u16 = 10;
const ID_NEXT: u16 = 11;
const ID_LIST: u16 = 12;
const ID_REFRESH: u16 = 13;
const ID_DISKMGMT: u16 = 14;
const ID_CHECK: u16 = 15;
const ID_UNDO: u16 = 16;
const ID_CLOSE: u16 = 17;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Welcome,
    Choose,
    Confirm,
    Work,
    Done,
}

/// What the worker thread hands back to the window.
#[derive(Default)]
struct Shared {
    candidates: Option<(Vec<Candidate>, Vec<String>)>,
    step: String,
    log: Vec<String>,
    progress: Option<(u64, u64)>,
    result: Option<Result<String, String>>,
}

static SHARED: OnceLock<Arc<Mutex<Shared>>> = OnceLock::new();
fn shared() -> Arc<Mutex<Shared>> {
    SHARED.get_or_init(|| Arc::new(Mutex::new(Shared::default()))).clone()
}

#[derive(Clone, Copy)]
struct SendHwnd(usize);
unsafe impl Send for SendHwnd {}

struct Ui {
    main: HWND,
    title: HWND,
    body: HWND,
    list: HWND,
    detail: HWND,
    text: HWND,
    check: HWND,
    progress: HWND,
    log: HWND,
    back: HWND,
    next: HWND,
    refresh: HWND,
    diskmgmt: HWND,
    undo: HWND,
    close: HWND,
    font: HANDLE,
    title_font: HANDLE,
    mono: HANDLE,
    dpi: u32,
    page: Page,
    candidates: Vec<Candidate>,
    usable: Vec<usize>,
    chosen: Option<Candidate>,
    busy: bool,
    staged: bool,
    blocking: Option<String>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
/// Static controls want CRLF; this program's text uses \n.
fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}
fn set_text(h: HWND, s: &str) {
    unsafe { SetWindowTextW(h, wide(&crlf(s)).as_ptr()) };
}
fn show(h: HWND, on: bool) {
    unsafe { ShowWindow(h, if on { 5 } else { 0 }) };
}
fn enable(h: HWND, on: bool) {
    unsafe { EnableWindow(h, on as i32) };
}
fn message(owner: HWND, text: &str, flags: u32) -> i32 {
    unsafe { MessageBoxW(owner, wide(&crlf(text)).as_ptr(), wide("Rime OS installer").as_ptr(), flags) }
}

/// ATTACH_PARENT_PROCESS: print into the console the command was typed in.
pub fn attach_parent_console() {
    unsafe { AttachConsole(u32::MAX) };
}

fn child(ex: u32, class: &str, text: &str, style: u32, id: u16, parent: HWND) -> HWND {
    unsafe {
        CreateWindowExW(
            ex,
            wide(class).as_ptr(),
            wide(&crlf(text)).as_ptr(),
            WS_CHILD | style,
            0,
            0,
            10,
            10,
            parent,
            id as usize as HANDLE,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null_mut(),
        )
    }
}

fn fonts(dpi: u32) -> (HANDLE, HANDLE, HANDLE) {
    let px = |pt: i32| -(pt * dpi as i32 / 72);
    unsafe {
        let body = CreateFontW(px(10), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, wide("Segoe UI").as_ptr());
        let title = CreateFontW(px(17), 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, wide("Segoe UI").as_ptr());
        let mono = CreateFontW(px(9), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, wide("Consolas").as_ptr());
        (body, title, mono)
    }
}

pub fn run() -> ExitCode {
    // The release build aborts on panic. A window that vanishes without a word
    // is the worst way for an installer to fail, so say what happened first.
    std::panic::set_hook(Box::new(|info| {
        let text = format!("The Rime OS installer hit an internal error and has to close:\n\n{info}\n\nNothing after the last step it reported was done.");
        unsafe { MessageBoxW(std::ptr::null_mut(), wide(&crlf(&text)).as_ptr(), wide("Rime OS installer").as_ptr(), MB_ICONERROR) };
    }));
    unsafe {
        let icc = INITCOMMONCONTROLSEX { size: 8, icc: 0x20 | 0x4000 }; // progress, standard
        InitCommonControlsEx(&icc);
        let inst = GetModuleHandleW(std::ptr::null());
        let class = wide("RimeOsInstaller");
        let wc = WNDCLASSEXW {
            size: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            proc_: wndproc,
            cls_extra: 0,
            wnd_extra: 0,
            instance: inst,
            icon: std::ptr::null_mut(),
            cursor: LoadCursorW(std::ptr::null_mut(), 32512 as *const u16),
            background: (5 + 1) as HANDLE, // COLOR_WINDOW + 1
            menu: std::ptr::null(),
            class_name: class.as_ptr(),
            icon_sm: std::ptr::null_mut(),
        };
        if RegisterClassExW(&wc) == 0 {
            return ExitCode::FAILURE;
        }
        let main = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Install Rime OS").as_ptr(),
            WS_MAIN,
            0x8000_0000u32 as i32,
            0x8000_0000u32 as i32,
            860,
            640,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            inst,
            std::ptr::null_mut(),
        );
        if main.is_null() {
            return ExitCode::FAILURE;
        }
        let dpi = GetDpiForWindow(main).max(96);
        SetWindowPos(main, std::ptr::null_mut(), 0, 0, 860 * dpi as i32 / 96, 640 * dpi as i32 / 96, 0x2 | 0x4);
        let (font, title_font, mono) = fonts(dpi);
        let ui = Ui {
            main,
            title: child(0, "STATIC", "", 0, 0, main),
            body: child(0, "STATIC", "", 0, 0, main),
            list: child(WS_EX_CLIENTEDGE, "LISTBOX", "", WS_TABSTOP | WS_VSCROLL | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT, ID_LIST, main),
            detail: child(0, "STATIC", "", 0, 0, main),
            text: child(WS_EX_CLIENTEDGE, "EDIT", "", WS_TABSTOP | WS_VSCROLL | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL, 0, main),
            check: child(0, "BUTTON", "I have read this, and my important files are backed up", WS_TABSTOP | BS_AUTOCHECKBOX, ID_CHECK, main),
            progress: child(0, "msctls_progress32", "", PBS_SMOOTH | WS_BORDER, 0, main),
            log: child(WS_EX_CLIENTEDGE, "EDIT", "", WS_VSCROLL | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL, 0, main),
            back: child(0, "BUTTON", "Back", WS_TABSTOP, ID_BACK, main),
            next: child(0, "BUTTON", "Next", WS_TABSTOP | BS_DEFPUSHBUTTON, ID_NEXT, main),
            refresh: child(0, "BUTTON", "Refresh", WS_TABSTOP, ID_REFRESH, main),
            diskmgmt: child(0, "BUTTON", "Open Disk Management", WS_TABSTOP, ID_DISKMGMT, main),
            undo: child(0, "BUTTON", "Remove the prepared setup", WS_TABSTOP, ID_UNDO, main),
            close: child(0, "BUTTON", "Close", WS_TABSTOP, ID_CLOSE, main),
            font,
            title_font,
            mono,
            dpi,
            page: Page::Welcome,
            candidates: Vec::new(),
            usable: Vec::new(),
            chosen: None,
            busy: false,
            staged: matches!(winstall::journal().ok().as_deref().and_then(|j| j.iter().find(|(k, _)| k == "state").map(|(_, v)| v.clone())), Some(s) if s == "ready" || s == "table-written"),
            blocking: preflight(),
        };
        UI.with(|u| *u.borrow_mut() = Some(ui));
        apply_fonts();
        goto(Page::Welcome);
        ShowWindow(main, 5);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(main, &mut msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    ExitCode::SUCCESS
}

/// Reasons the install cannot start on this machine at all, checked before
/// anything is offered.
fn preflight() -> Option<String> {
    let mut fw = 0u32;
    if unsafe { GetFirmwareType(&mut fw) } != 0 && fw != 2 {
        return Some(
            "This computer started Windows in legacy BIOS mode. Rime OS installs only on UEFI computers, \
             and switching a running Windows from BIOS to UEFI is not something this installer does."
                .into(),
        );
    }
    if unsafe { IsUserAnAdmin() } == 0 {
        return Some("The installer needs administrator rights. Right-click it and choose Run as administrator.".into());
    }
    if let Err(e) = crate::pin::pin() {
        return Some(format!("This copy of the installer is incomplete: {e}. Download it again from rimeos.com."));
    }
    None
}

fn with<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    UI.with(|u| f(u.borrow_mut().as_mut().expect("ui")))
}

/// For the window procedure, which Windows also calls from INSIDE other
/// calls: buttons ask their parent for its default button (DM_GETDEFID) while
/// the window is still being built, and an edit control reports EN_CHANGE
/// synchronously while a page is filling it. Neither may touch the state
/// then; `None` means "not now", and the message gets its default answer.
fn try_with<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn apply_fonts() {
    with(|u| unsafe {
        for h in [u.body, u.list, u.detail, u.check, u.back, u.next, u.refresh, u.diskmgmt, u.undo, u.close] {
            SendMessageW(h, WM_SETFONT, u.font as usize, 1);
        }
        SendMessageW(u.title, WM_SETFONT, u.title_font as usize, 1);
        SendMessageW(u.text, WM_SETFONT, u.mono as usize, 1);
        SendMessageW(u.log, WM_SETFONT, u.mono as usize, 1);
    });
}

/// Positions every control for the current page. Logical units at 96 DPI,
/// scaled; the window is a fixed size so the layout is a fixed grid.
fn layout() {
    with(|u| unsafe {
        let s = |v: i32| v * u.dpi as i32 / 96;
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(u.main, &mut r);
        let (w, h) = (r.right, r.bottom);
        let m = s(24);
        let bw = s(110);
        let bh = s(32);
        let by = h - m - bh;
        MoveWindow(u.title, m, m, w - 2 * m, s(36), 1);
        let top = m + s(44);
        let p = u.page;
        let body_h = match p {
            Page::Welcome => by - top - s(16),
            Page::Choose => s(64),
            Page::Confirm => s(24),
            Page::Work => s(24),
            Page::Done => by - top - s(16),
        };
        MoveWindow(u.body, m, top, w - 2 * m, body_h, 1);
        let below = top + body_h + s(8);
        MoveWindow(u.list, m, below, w - 2 * m, s(190), 1);
        MoveWindow(u.detail, m, below + s(198), w - 2 * m, by - (below + s(198)) - s(12), 1);
        MoveWindow(u.text, m, below, w - 2 * m, by - below - s(48), 1);
        MoveWindow(u.check, m, by - s(40), w - 2 * m, s(28), 1);
        MoveWindow(u.progress, m, below + s(4), w - 2 * m, s(22), 1);
        MoveWindow(u.log, m, below + s(40), w - 2 * m, by - below - s(52), 1);
        MoveWindow(u.next, w - m - bw, by, bw, bh, 1);
        MoveWindow(u.back, w - m - 2 * bw - s(8), by, bw, bh, 1);
        MoveWindow(u.close, w - m - bw, by, bw, bh, 1);
        MoveWindow(u.refresh, m, by, bw, bh, 1);
        MoveWindow(u.diskmgmt, m + bw + s(8), by, s(190), bh, 1);
        MoveWindow(u.undo, m, by, s(230), bh, 1);
    });
}

fn goto(p: Page) {
    with(|u| u.page = p);
    let (main, staged, blocking) = with(|u| (u.main, u.staged, u.blocking.clone()));
    with(|u| {
        for h in [u.list, u.detail, u.text, u.check, u.progress, u.log, u.back, u.next, u.refresh, u.diskmgmt, u.undo, u.close] {
            show(h, false);
        }
        show(u.title, true);
        show(u.body, true);
    });
    match p {
        Page::Welcome => with(|u| {
            set_text(u.title, "Install Rime OS next to Windows");
            let mut t = String::from(
                "This puts Rime OS on this computer beside Windows. Windows stays exactly as it is.\n\n\
                 What happens:\n\
                 1.  You choose free space on a disk (make some first by shrinking C: in Disk Management).\n\
                 2.  This program downloads the Rime OS installer (about 1.9 GB), checks it, and puts it in \
                 a new partition of its own in that space. Windows' own boot files are never written.\n\
                 3.  You restart. Rime OS's installer starts once, by itself, and you create your account \
                 there. It finishes the installation from the internet.\n\n\
                 Afterwards your computer starts Rime OS, and Windows is still there to choose.\n\n\
                 Back up anything important before changing a disk. That is good advice for any installer, \
                 and this one is no exception.",
            );
            if staged {
                t.push_str("\n\nRime OS's installer has already been prepared on this computer. Restart to continue it, or remove it with the button below.");
            }
            if let Some(b) = &blocking {
                t = format!("This computer cannot install Rime OS from Windows:\n\n{b}");
            }
            set_text(u.body, &t);
            set_text(u.next, "Next");
            show(u.next, blocking.is_none());
            enable(u.next, blocking.is_none());
            show(u.undo, staged && blocking.is_none());
        }),
        Page::Choose => {
            with(|u| {
                set_text(u.title, "Where should Rime OS go?");
                set_text(
                    u.body,
                    &format!(
                        "Choose free space or an empty partition. Rime OS needs at least {}; 50 GB or more is better. \
                         Partitions Windows uses are not listed.",
                        plan::human(winstall::needed_bytes())
                    ),
                );
                for h in [u.list, u.detail, u.back, u.next, u.refresh, u.diskmgmt] {
                    show(h, true);
                }
                set_text(u.next, "Next");
                enable(u.next, false);
            });
            refresh_candidates();
        }
        Page::Confirm => with(|u| {
            let c = u.chosen.clone().expect("chosen");
            set_text(u.title, "Check this before you continue");
            set_text(u.body, "Nothing has been changed yet.");
            set_text(u.text, &plan::install_confirmation(&c.disk, &c.what, c.bytes));
            unsafe { SendMessageW(u.check, BM_SETCHECK, 0, 0) };
            for h in [u.text, u.check, u.back, u.next] {
                show(h, true);
            }
            set_text(u.next, "Install");
            enable(u.next, false);
        }),
        Page::Work => with(|u| {
            set_text(u.title, "Preparing Rime OS");
            set_text(u.body, "Starting...");
            set_text(u.log, "");
            show(u.progress, true);
            show(u.log, true);
            unsafe { SendMessageW(u.progress, PBM_SETRANGE32, 0, 1000) };
        }),
        Page::Done => with(|u| {
            show(u.close, true);
            show(u.next, true);
            set_text(u.next, "Restart now");
            enable(u.next, true);
            // Close sits left of Restart on this page.
            let s = |v: i32| v * u.dpi as i32 / 96;
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            unsafe {
                GetClientRect(u.main, &mut r);
                MoveWindow(u.close, r.right - s(24) - 2 * s(110) - s(8), r.bottom - s(24) - s(32), s(110), s(32), 1);
            }
        }),
    }
    layout();
    if p == Page::Done {
        with(|u| unsafe {
            let s = |v: i32| v * u.dpi as i32 / 96;
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            GetClientRect(u.main, &mut r);
            MoveWindow(u.close, r.right - s(24) - 2 * s(110) - s(8), r.bottom - s(24) - s(32), s(110), s(32), 1);
        });
    }
    // Keyboard focus on the page's primary control, so Enter and Tab work
    // from the moment a page appears, without reaching for the mouse.
    with(|u| unsafe {
        let order = match u.page {
            Page::Choose => [u.list, u.next, u.back],
            Page::Confirm => [u.check, u.next, u.back],
            Page::Done => [u.next, u.close, u.close],
            _ => [u.next, u.undo, u.close],
        };
        if let Some(h) = order.into_iter().find(|h| IsWindowVisible(*h) != 0 && IsWindowEnabled(*h) != 0) {
            SetFocus(h);
        }
    });
    let _ = main;
}

fn refresh_candidates() {
    with(|u| unsafe {
        SendMessageW(u.list, LB_RESETCONTENT, 0, 0);
        SendMessageW(u.list, LB_ADDSTRING, 0, wide("Looking at the disks...").as_ptr() as isize);
        enable(u.refresh, false);
        set_text(u.detail, "");
    });
    let hwnd = SendHwnd(with(|u| u.main) as usize);
    std::thread::spawn(move || {
        let r = winstall::candidates();
        shared().lock().unwrap().candidates = Some(r);
        unsafe { PostMessageW(hwnd.0 as HWND, WM_APP_CANDIDATES, 0, 0) };
    });
}

fn fill_candidates() {
    let Some((list, problems)) = shared().lock().unwrap().candidates.take() else { return };
    with(|u| unsafe {
        u.candidates = list;
        u.usable.clear();
        SendMessageW(u.list, LB_RESETCONTENT, 0, 0);
        enable(u.refresh, true);
        for (i, c) in u.candidates.iter().enumerate() {
            let mark = if c.refusal.is_none() { "" } else { "(cannot be used)  " };
            let line = format!("{mark}{}  -  {}", plan::human(c.bytes), c.what);
            SendMessageW(u.list, LB_ADDSTRING, 0, wide(&line).as_ptr() as isize);
            u.usable.push(i);
        }
        let mut d = String::new();
        if !u.candidates.iter().any(|c| c.refusal.is_none()) {
            d.push_str(plan::NO_SPACE_HELP);
        } else {
            d.push_str("Select a line to see what would happen there.");
        }
        if !problems.is_empty() {
            d.push_str("\n\nSome disks could not be read: ");
            d.push_str(&problems.join("; "));
        }
        set_text(u.detail, &d);
        // Pre-select the only usable space, when there is exactly one.
        let ok: Vec<usize> = u.candidates.iter().enumerate().filter(|(_, c)| c.refusal.is_none()).map(|(i, _)| i).collect();
        if ok.len() == 1 {
            SendMessageW(u.list, LB_SETCURSEL, ok[0], 0);
        }
    });
    selection_changed();
}

fn selection_changed() {
    with(|u| unsafe {
        let i = SendMessageW(u.list, LB_GETCURSEL, 0, 0);
        if i < 0 || i as usize >= u.candidates.len() {
            enable(u.next, false);
            return;
        }
        let c = &u.candidates[i as usize];
        match &c.refusal {
            None => {
                set_text(u.detail, &format!("{}\n{} of space. Rime creates its two partitions here.", c.what, plan::human(c.bytes)));
                enable(u.next, true);
                u.chosen = Some(c.clone());
            }
            Some(r) => {
                set_text(u.detail, &format!("{}\n\nThis cannot be used: {r}.", c.what));
                enable(u.next, false);
                u.chosen = None;
            }
        }
    });
}

fn start_install() {
    let c = with(|u| u.chosen.clone()).expect("chosen");
    with(|u| u.busy = true);
    goto(Page::Work);
    let hwnd = SendHwnd(with(|u| u.main) as usize);
    std::thread::spawn(move || {
        let sh = shared();
        let post = |m| unsafe { PostMessageW(hwnd.0 as HWND, m, 0, 0) };
        let mut ev = |e: Event| {
            {
                let mut s = sh.lock().unwrap();
                match e {
                    Event::Step(t) => {
                        s.log.push(t.clone());
                        s.step = t;
                        s.progress = None;
                    }
                    Event::Note(t) => s.log.push(t),
                    Event::Progress { what, done, total } => {
                        s.progress = Some((done, total));
                        s.step = format!("{} ({what}: {} of {})", s.step.split(" (").next().unwrap_or(""), plan::human(done), plan::human(total));
                    }
                }
            }
            post(WM_APP_PROGRESS);
        };
        let r = winstall::install(&c, None, &mut ev);
        sh.lock().unwrap().result = Some(match r {
            Ok(o) => Ok(format!(
                "Rime OS's installer is ready.\n\n\
                 Restart the computer. It starts Rime OS's installer once, by itself: choose your keyboard, \
                 connect to the internet, create your account, and confirm. The installer then downloads \
                 and installs Rime OS into the space you chose. That takes a while, depending on your connection.\n\n\
                 If you restart and change your mind, just turn the computer off at the installer's first page: \
                 Windows starts as usual next time, and this program can remove what it prepared.{}\n\n\
                 What was prepared is recorded in {}.",
                if o.bitlocker_suspended {
                    "\n\nBitLocker on C: is suspended until Windows next starts, so the new partitions do not \
                     make it ask for your recovery key. It turns itself back on."
                } else {
                    ""
                },
                o.journal.display()
            )),
            Err(e) => Err(e.to_string()),
        });
        post(WM_APP_DONE);
    });
}

fn start_undo() {
    let preview = match winstall::undo_preview() {
        Ok(p) => p,
        Err(e) => {
            message(with(|u| u.main), &format!("There is nothing this program can remove: {e}"), MB_ICONERROR);
            return;
        }
    };
    if message(with(|u| u.main), &format!("Remove what this program prepared?\n\n{preview}"), MB_YESNO | MB_ICONWARNING) != IDYES {
        return;
    }
    with(|u| u.busy = true);
    goto(Page::Work);
    with(|u| set_text(u.title, "Removing the prepared setup"));
    let hwnd = SendHwnd(with(|u| u.main) as usize);
    std::thread::spawn(move || {
        let sh = shared();
        let post = |m| unsafe { PostMessageW(hwnd.0 as HWND, m, 0, 0) };
        let mut ev = |e: Event| {
            if let Event::Step(t) | Event::Note(t) = e {
                sh.lock().unwrap().log.push(t.clone());
                sh.lock().unwrap().step = t;
            }
            post(WM_APP_PROGRESS);
        };
        let r = winstall::undo(&mut ev);
        sh.lock().unwrap().result = Some(match r {
            Ok(()) => Ok("Done. Rime's partitions and boot entries are gone; the space is unallocated again.".into()),
            Err(e) => Err(e.to_string()),
        });
        post(WM_APP_DONE);
    });
}

fn update_progress() {
    let (step, log, prog) = {
        let sh = shared();
        let mut s = sh.lock().unwrap();
        let log: Vec<String> = std::mem::take(&mut s.log);
        (s.step.clone(), log, s.progress)
    };
    with(|u| unsafe {
        set_text(u.body, &step);
        for line in log {
            SendMessageW(u.log, EM_SETSEL, usize::MAX >> 1, (usize::MAX >> 1) as isize);
            SendMessageW(u.log, EM_REPLACESEL, 0, wide(&format!("{line}\r\n")).as_ptr() as isize);
        }
        if let Some((d, t)) = prog {
            SendMessageW(u.progress, PBM_SETPOS, (d.saturating_mul(1000) / t.max(1)) as usize, 0);
        }
    });
}

fn finished() {
    let r = shared().lock().unwrap().result.take();
    update_progress();
    with(|u| u.busy = false);
    match r {
        Some(Ok(text)) => {
            with(|u| {
                u.staged = true;
                set_text(u.title, "Ready. Restart to continue.");
                set_text(u.body, &text);
            });
            goto(Page::Done);
            with(|u| {
                set_text(u.title, "Ready. Restart to continue.");
            });
        }
        Some(Err(e)) => {
            let main = with(|u| u.main);
            message(main, &format!("The installer stopped:\n\n{e}\n\nNothing after this step was done."), MB_ICONERROR);
            with(|u| {
                set_text(u.title, "The installer stopped");
                set_text(u.body, &e);
                show(u.close, true);
            });
            layout();
        }
        None => {}
    }
}

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_COMMAND => {
            let id = (w & 0xffff) as u16;
            let code = ((w >> 16) & 0xffff) as u16;
            let Some(page) = try_with(|u| u.page) else { return 0 };
            match (id, code) {
                (ID_LIST, LBN_SELCHANGE) => selection_changed(),
                (ID_CHECK, _) if page == Page::Confirm => with(|u| unsafe {
                    let on = SendMessageW(u.check, BM_GETCHECK, 0, 0) == 1;
                    enable(u.next, on);
                }),
                (ID_REFRESH, _) => refresh_candidates(),
                (ID_DISKMGMT, _) => unsafe {
                    ShellExecuteW(h, wide("open").as_ptr(), wide("diskmgmt.msc").as_ptr(), std::ptr::null(), std::ptr::null(), 1);
                },
                (ID_UNDO, _) => start_undo(),
                (ID_CLOSE, _) => unsafe {
                    DestroyWindow(h);
                },
                (ID_BACK, _) => match page {
                    Page::Choose => goto(Page::Welcome),
                    Page::Confirm => goto(Page::Choose),
                    _ => {}
                },
                (ID_NEXT, _) => match page {
                    Page::Welcome => goto(Page::Choose),
                    Page::Choose if with(|u| u.chosen.is_some()) => goto(Page::Confirm),
                    Page::Confirm => {
                        let on = with(|u| unsafe { SendMessageW(u.check, BM_GETCHECK, 0, 0) == 1 });
                        if on {
                            start_install();
                        }
                    }
                    Page::Done => {
                        if let Err(e) = crate::winwrite::restart() {
                            message(h, &format!("Windows did not restart: {e}. Restart from the Start menu."), MB_ICONERROR);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
            0
        }
        // IsDialogMessage asks a window for its default button when Enter
        // is pressed; a plain window answers 0 and Enter does nothing.
        DM_GETDEFID => {
            let Some(next) = try_with(|u| u.next) else { return 0 };
            if unsafe { IsWindowVisible(next) != 0 && IsWindowEnabled(next) != 0 } {
                ((0x534B_isize) << 16) | ID_NEXT as isize
            } else {
                0
            }
        }
        WM_APP_CANDIDATES => {
            fill_candidates();
            0
        }
        WM_APP_PROGRESS => {
            update_progress();
            0
        }
        WM_APP_DONE => {
            finished();
            0
        }
        WM_CTLCOLORSTATIC => {
            unsafe { SetBkMode(w as HANDLE, 1) };
            unsafe { GetSysColorBrush(5) as LRESULT }
        }
        WM_DPICHANGED => {
            let dpi = (w & 0xffff) as u32;
            let r = unsafe { &*(l as *const RECT) };
            unsafe {
                SetWindowPos(h, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, 0x4 | 0x10);
            }
            if try_with(|_| ()).is_none() {
                return 0;
            }
            with(|u| {
                unsafe {
                    DeleteObject(u.font);
                    DeleteObject(u.title_font);
                    DeleteObject(u.mono);
                }
                u.dpi = dpi;
                let (a, b, c) = fonts(dpi);
                u.font = a;
                u.title_font = b;
                u.mono = c;
            });
            apply_fonts();
            layout();
            0
        }
        WM_CLOSE => {
            if try_with(|u| u.busy).unwrap_or(false) {
                message(h, "Please wait: the installer is writing to the disk. Closing now would leave the work half done.", MB_ICONWARNING);
                return 0;
            }
            unsafe { DestroyWindow(h) };
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(h, m, w, l) },
    }
}
