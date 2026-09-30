//! Hands a line about what is playing to the Windows share sheet. A desktop
//! app reaches the sheet through `IDataTransferManagerInterop`, keyed on its
//! window: the manager for that window asks for the data once the person has
//! picked where it goes.

use std::cell::RefCell;

use windows::ApplicationModel::DataTransfer::DataTransferManager;
use windows::Win32::shobjidl_core::IDataTransferManagerInterop;
use windows::Win32::winuser::GetActiveWindow;
use windows::core::{EventRevoker, HSTRING, Result, factory};

thread_local! {
    /// The handler for the sheet open now. Dropping a revoker unhooks its
    /// handler, so it is kept until the next share replaces it.
    static REQUESTED: RefCell<Option<EventRevoker>> = const { RefCell::new(None) };
}

/// Opens the share sheet over the active window with `text`, titled `title`.
/// Called from a click, so the active window is Ryuuji's.
pub(crate) fn share(title: &str, text: &str) -> Result<()> {
    let interop = factory::<DataTransferManager, IDataTransferManagerInterop>()?;
    // SAFETY: GetActiveWindow reads the calling thread's own window state.
    let window = unsafe { GetActiveWindow() };
    // SAFETY: `window` is the live top-level window of this UI thread.
    let manager: DataTransferManager = unsafe { interop.GetForWindow(window)? };
    let (title, text) = (HSTRING::from(title), HSTRING::from(text));
    let revoker = manager.DataRequested(move |_, args| {
        let Some(args) = args.as_ref() else {
            return;
        };
        let filled = args.Request().and_then(|request| {
            let data = request.Data()?;
            data.Properties()?.SetTitle(&title)?;
            data.SetText(&text)
        });
        if let Err(err) = filled {
            tracing::warn!(error = %err, "share sheet asked for data and got none");
        }
    })?;
    REQUESTED.with(|slot| slot.replace(Some(revoker)));
    // SAFETY: as above; the handler is hooked before the sheet can ask.
    unsafe { interop.ShowShareUIForWindow(window).ok() }
}
