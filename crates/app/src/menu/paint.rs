//! Painting the Windows menu bar (File, Edit, ...) in the theme's colours.
//!
//! Windows draws a window's menu bar itself, in the system's colours, and
//! has no public setting to change them. Before it paints, though, it sends
//! the window two undocumented messages: `WM_UAHDRAWMENU` for the empty bar
//! and `WM_UAHDRAWMENUITEM` for each title. A window that answers them has
//! painted the bar itself. Notepad++, muda's own dark mode and many other
//! programs rely on this, and it has not changed since Windows Vista.
//!
//! To answer them we *subclass* the window: Windows lets a program put a
//! function in front of a window's message handler, and that function
//! passes on whatever it does not handle with `DefSubclassProc`. muda
//! subclasses the window too, for its dark menu bar. The latest subclass
//! sees each message first, so ours, installed after muda attaches the
//! menu, paints over muda's.
//!
//! The drop-down menus are separate popup windows and keep the system's
//! colours.
#![expect(
    unsafe_code,
    reason = "subclassing a window and painting with GDI are Win32 calls"
)]

use std::cell::Cell;
use std::ptr;

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateSolidBrush, DT_CENTER, DT_HIDEPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
    FillRect, GetWindowDC, HBRUSH, HDC, MapWindowPoints, OffsetRect, RDW_FRAME, RDW_INVALIDATE,
    RedrawWindow, ReleaseDC, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ODS_DISABLED, ODS_GRAYED, ODS_HOTLIGHT, ODS_NOACCEL, ODS_SELECTED,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DrawMenuBar, GetClientRect, GetMenu, GetMenuBarInfo, GetMenuItemInfoW, GetWindowRect, HMENU,
    InSendMessage, MENUBARINFO, MENUITEMINFOW, MIIM_STRING, OBJID_MENU, SPI_GETHIGHCONTRAST,
    SystemParametersInfoW, WM_NCACTIVATE, WM_NCDESTROY, WM_NCPAINT,
};

use crate::theme::{Color, Ui};

/// Sent to paint the empty menu bar; `lparam` points to a [`UahMenu`].
const WM_UAHDRAWMENU: u32 = 0x0091;
/// Sent to paint one menu title; `lparam` points to a [`UahDrawMenuItem`].
const WM_UAHDRAWMENUITEM: u32 = 0x0092;

/// Tells our subclass apart from muda's on the same window.
const SUBCLASS_ID: usize = 0x4e49_4d42;

/// What `WM_UAHDRAWMENU` points to (`UAHMENU`). The layout must match
/// Windows' exactly, padding included: it is embedded in the item below.
#[repr(C)]
struct UahMenu {
    menu: HMENU,
    dc: HDC,
    _flags: u32,
}

/// The start of what `WM_UAHDRAWMENUITEM` points to (`UAHDRAWMENUITEM`):
/// the usual owner-draw description, the menu, then the item's position in
/// the bar. Windows passes item sizes after these fields; we only read this
/// prefix, which is sound through a pointer to the larger object.
#[repr(C)]
struct UahDrawMenuItem {
    draw: DRAWITEMSTRUCT,
    menu: UahMenu,
    position: u32,
}

/// The bar's colours as Windows wants them (`0x00BBGGRR`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colours {
    bar: COLORREF,
    hot: COLORREF,
    text: COLORREF,
    disabled: COLORREF,
}

impl Colours {
    /// The tab bar's colour for the bar, so it runs on into the title bar
    /// above and the tabs below; the picker's selected row behind a title
    /// under the pointer or open.
    /// Theme colours may be see-through (a light theme's selected row is
    /// a faint tint); GDI has no alpha, so each is mixed over the bar first.
    pub fn from_theme(ui: &Ui) -> Self {
        let bar = over(ui.tab_bar, ui.background);
        Self {
            bar: colorref(bar),
            hot: colorref(over(ui.picker_selected, bar)),
            text: colorref(over(ui.text, bar)),
            disabled: colorref(over(ui.gutter, bar)),
        }
    }
}

/// `top` painted over the opaque colour `below`.
fn over(top: Color, below: Color) -> Color {
    let a = top[3].clamp(0.0, 1.0);
    let mix = |i: usize| top[i] * a + below[i] * (1.0 - a);
    [mix(0), mix(1), mix(2), 1.0]
}

/// An opaque colour as a `COLORREF`.
fn colorref(c: Color) -> COLORREF {
    let byte = |v: f32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a colour channel clamped to 0-255"
        )]
        let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        u32::from(b)
    };
    byte(c[0]) | (byte(c[1]) << 8) | (byte(c[2]) << 16)
}

thread_local! {
    /// The colours to paint with. Windows calls the subclass on the thread
    /// that owns the window, the same UI thread that sets them, so a
    /// thread-local needs no locking and the subclass holds no pointer.
    static COLOURS: Cell<Option<Colours>> = const { Cell::new(None) };
}

/// Put our painter in front of the window's message handler. Call after
/// muda has attached the menu, so ours runs first. `false` if Windows
/// refused, and the bar keeps muda's colours.
pub fn install(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` is a live window created on this thread (the caller
    // passes the winit window it holds). `subclass_proc` is a plain function
    // that lives for the whole program and takes no reference data (0), so
    // nothing it uses can be freed while the subclass is installed; it
    // removes itself when the window is destroyed (`WM_NCDESTROY`).
    unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0) != 0 }
}

/// Paint with these colours from now on, and repaint the bar now.
pub fn set_colours(hwnd: HWND, colours: Colours) {
    COLOURS.with(|c| c.set(Some(colours)));
    // SAFETY: `hwnd` is the live window the menu is attached to. Both calls
    // only ask Windows to repaint the bar and the frame around the client
    // area; null rectangle and region mean "all of it".
    unsafe {
        DrawMenuBar(hwnd);
        RedrawWindow(
            hwnd,
            ptr::null(),
            ptr::null_mut(),
            RDW_FRAME | RDW_INVALIDATE,
        );
    }
}

/// Whether Windows' high contrast mode is on; its colours then win over
/// the theme's, as for every other part of the window frame.
fn high_contrast() -> bool {
    let mut hc = HIGHCONTRASTW {
        cbSize: u32::try_from(size_of::<HIGHCONTRASTW>()).unwrap_or(0),
        ..HIGHCONTRASTW::default()
    };
    // SAFETY: writes into the local struct, whose size is passed in both
    // `cbSize` and `uiParam` as `SPI_GETHIGHCONTRAST` requires.
    let ok =
        unsafe { SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, (&raw mut hc).cast(), 0) };
    ok != 0 && hc.dwFlags & HCF_HIGHCONTRASTON != 0
}

/// The subclass: paint the bar ourselves, pass everything else on.
unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let pass_on = || {
        // SAFETY: called from inside the subclass procedure with the
        // message Windows gave it, which is what `DefSubclassProc` requires.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    };
    if msg == WM_NCDESTROY {
        // SAFETY: removes the subclass this module installed on `hwnd`,
        // with the same procedure and id, while the window still exists.
        unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
        return pass_on();
    }
    let ours = matches!(
        msg,
        WM_UAHDRAWMENU | WM_UAHDRAWMENUITEM | WM_NCPAINT | WM_NCACTIVATE
    );
    // The paint messages come from Windows' own frame painting on this
    // thread; one sent from elsewhere (`InSendMessage`) is not trusted.
    // SAFETY: `InSendMessage` only reports how the current message arrived.
    let from_elsewhere = ours && unsafe { InSendMessage() } != 0;
    let Some(colours) = COLOURS.with(Cell::get).filter(|_| ours && !from_elsewhere) else {
        return pass_on();
    };
    if high_contrast() {
        return pass_on();
    }
    match msg {
        WM_UAHDRAWMENU => {
            // SAFETY: for this message, sent by Windows on this thread (the
            // `InSendMessage` check above), `lparam` is null or points to a
            // `UAHMENU` that stays valid for the length of the call, and
            // `UahMenu` matches its layout.
            let Some(menu) = (unsafe { (lparam as *const UahMenu).as_ref() }) else {
                return pass_on();
            };
            let Some(rect) = bar_rect(hwnd) else {
                return pass_on();
            };
            fill(menu.dc, &rect, colours.bar);
            0
        }
        WM_UAHDRAWMENUITEM => {
            // SAFETY: as above, for `UAHDRAWMENUITEM`, whose first fields
            // `UahDrawMenuItem` matches.
            let Some(item) = (unsafe { (lparam as *const UahDrawMenuItem).as_ref() }) else {
                return pass_on();
            };
            paint_item(item, colours);
            0
        }
        WM_NCPAINT | WM_NCACTIVATE => {
            // Windows (and muda) paint the frame first, including a light
            // line under the menu bar; then we paint over that line.
            let result = pass_on();
            // SAFETY: `GetMenu` only reads the window's menu handle. With no
            // menu (muda detaches it on quit) there is no line to cover.
            if !unsafe { GetMenu(hwnd) }.is_null() {
                paint_line(hwnd, colours);
            }
            result
        }
        _ => pass_on(),
    }
}

/// Fills a rectangle with one colour, creating and freeing the brush.
fn fill(dc: HDC, rect: &RECT, colour: COLORREF) {
    // SAFETY: `dc` is a device context Windows handed us for this paint (or
    // one we got from `GetWindowDC`). The brush is created here, used once
    // and deleted; a null brush (creation failed) makes `FillRect` fail
    // without drawing.
    unsafe {
        let brush: HBRUSH = CreateSolidBrush(colour);
        FillRect(dc, rect, brush);
        DeleteObject(brush);
    }
}

/// The menu bar's rectangle in window coordinates, one pixel taller at the
/// top to cover the frame's edge; `None` if Windows can't say.
fn bar_rect(hwnd: HWND) -> Option<RECT> {
    let mut info = MENUBARINFO {
        cbSize: u32::try_from(size_of::<MENUBARINFO>()).unwrap_or(0),
        ..MENUBARINFO::default()
    };
    let mut window = RECT::default();
    // SAFETY: both calls write into the local structs passed by pointer;
    // `cbSize` is set as `GetMenuBarInfo` requires.
    let ok = unsafe {
        GetMenuBarInfo(hwnd, OBJID_MENU, 0, &raw mut info) != 0
            && GetWindowRect(hwnd, &raw mut window) != 0
    };
    if !ok {
        return None;
    }
    let mut bar = info.rcBar;
    // SAFETY: only adjusts the local rectangle.
    unsafe { OffsetRect(&raw mut bar, -window.left, -window.top) };
    bar.top -= 1;
    Some(bar)
}

/// One title: its background, then its label.
fn paint_item(item: &UahDrawMenuItem, colours: Colours) {
    let dc = item.menu.dc;
    let state = item.draw.itemState;
    let hot = state & (ODS_HOTLIGHT | ODS_SELECTED) != 0;
    let disabled = state & (ODS_GRAYED | ODS_DISABLED) != 0;
    fill(
        dc,
        &item.draw.rcItem,
        if hot { colours.hot } else { colours.bar },
    );

    // The label, read into a stack buffer: titles are a few letters.
    let mut label = [0u16; 128];
    let mut info = MENUITEMINFOW {
        cbSize: u32::try_from(size_of::<MENUITEMINFOW>()).unwrap_or(0),
        fMask: MIIM_STRING,
        dwTypeData: label.as_mut_ptr(),
        cch: u32::try_from(label.len()).unwrap_or(0),
        ..MENUITEMINFOW::default()
    };
    // SAFETY: `info` points at `label` and gives its length in `cch`, so
    // Windows writes at most that many characters, the terminating zero
    // included.
    let ok = unsafe { GetMenuItemInfoW(item.menu.menu, item.position, 1, &raw mut info) };
    if ok == 0 {
        return;
    }
    // Windows sets `cch` to the text's length, which a cut-off title could
    // put past the buffer; read no more than was written.
    let written = u32::try_from(label.len() - 1).unwrap_or(0);
    let len = i32::try_from(info.cch.min(written)).unwrap_or(0);
    let mut flags = DT_CENTER | DT_SINGLELINE | DT_VCENTER;
    if state & ODS_NOACCEL != 0 {
        // Underline the access key (the F in File) only once Alt is held.
        flags |= DT_HIDEPREFIX;
    }
    let mut rect = item.draw.rcItem;
    // SAFETY: draws `len` characters of `label`, at most the ones Windows
    // just filled,
    // on the device context it handed us for this paint.
    unsafe {
        SetBkMode(dc, TRANSPARENT.cast_signed());
        SetTextColor(
            dc,
            if disabled {
                colours.disabled
            } else {
                colours.text
            },
        );
        DrawTextW(dc, label.as_ptr(), len, &raw mut rect, flags);
    }
}

/// The one-pixel line Windows draws between the menu bar and the client
/// area, which it always draws light.
fn paint_line(hwnd: HWND, colours: Colours) {
    let mut client = RECT::default();
    let mut window = RECT::default();
    // SAFETY: the calls write into the local rectangles. `MapWindowPoints`
    // treats a `RECT` as two `POINT`s, its documented use, and converts
    // client coordinates to the screen (null target window).
    unsafe {
        GetClientRect(hwnd, &raw mut client);
        MapWindowPoints(hwnd, ptr::null_mut(), (&raw mut client).cast::<POINT>(), 2);
        GetWindowRect(hwnd, &raw mut window);
        OffsetRect(&raw mut client, -window.left, -window.top);
    }
    let line = RECT {
        top: client.top - 1,
        bottom: client.top,
        ..client
    };
    // SAFETY: `GetWindowDC` gives a context for the whole window, frame
    // included, which is released right after the fill.
    unsafe {
        let dc = GetWindowDC(hwnd);
        if dc.is_null() {
            return;
        }
        fill(dc, &line, colours.bar);
        ReleaseDC(hwnd, dc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_red_in_the_low_byte() {
        assert_eq!(colorref([1.0, 0.0, 0.0, 1.0]), 0x0000_00ff);
        assert_eq!(colorref([0.0, 0.5, 1.0, 0.3]), 0x00ff_8000);
    }

    #[test]
    fn see_through_colours_are_mixed_over_the_bar() {
        let tint = over([0.0, 0.0, 0.0, 0.25], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(colorref(tint), 0x00bf_bfbf);
    }

    #[test]
    #[cfg(target_pointer_width = "32")]
    fn item_layout_matches_windows() {
        assert_eq!(size_of::<UahMenu>(), 12);
        assert_eq!(size_of::<DRAWITEMSTRUCT>(), 48);
        assert_eq!(std::mem::offset_of!(UahDrawMenuItem, position), 60);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn item_layout_matches_windows() {
        // UAHMENU is two pointers and a DWORD, padded to 24 bytes; the
        // item's position follows it.
        assert_eq!(size_of::<UahMenu>(), 24);
        assert_eq!(
            std::mem::offset_of!(UahDrawMenuItem, position),
            size_of::<DRAWITEMSTRUCT>() + 24
        );
        assert_eq!(size_of::<DRAWITEMSTRUCT>(), 64);
    }
}
