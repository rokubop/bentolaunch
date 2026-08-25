//! Shell pickers for adding tiles without hand-editing config.
//!
//! `IFileOpenDialog` pointed at `shell:AppsFolder` is a real installed-app
//! browser: everything on the Start menu, Store apps included. And it returns a
//! shell item, whose parsing name is exactly the string bentolaunch's target model
//! already stores. So "pick an app" costs no bespoke UI, and Store apps come
//! along for free.
//!
//! These are modal and run on the UI thread, which is the STA the dialog needs.
//! Safe because they are only reachable from the tray menu, with the panel
//! hidden — the watchdog only watches while the panel is visible, and the
//! dialog's own message loop keeps dispatching to our window anyway.

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    FILEOPENDIALOGOPTIONS, FOS_ALLNONSTORAGEITEMS, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_NODEREFERENCELINKS, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    IShellItem, SHCreateItemFromParsingName, SIGDN_DESKTOPABSOLUTEPARSING,
    SIGDN_PARENTRELATIVEPARSING,
};
use windows::Win32::Foundation::HWND;
use windows::core::HSTRING;

use crate::{log_info, log_warn};

/// The virtual folder holding every installed app, Store apps included.
const APPS_FOLDER: &str = "shell:AppsFolder";

/// Browse installed apps. Returns a launchable parsing name.
pub fn pick_app(owner: HWND) -> Option<String> {
    // Apps are not filesystem items, so FORCEFILESYSTEM must be off or the
    // dialog refuses to return them.
    let options = FOS_ALLNONSTORAGEITEMS | FOS_NODEREFERENCELINKS;
    let picked = show(owner, "Add an app", options, Some(APPS_FOLDER))?;
    log_info!("picked app: {picked}");
    Some(picked)
}

pub fn pick_folder(owner: HWND) -> Option<String> {
    let options = FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST;
    let picked = show(owner, "Add a folder", options, None)?;
    log_info!("picked folder: {picked}");
    Some(picked)
}

pub fn pick_file(owner: HWND) -> Option<String> {
    // NODEREFERENCELINKS keeps a .lnk as itself: it launches fine and carries
    // the target's icon, and the shortcut is what the user actually chose.
    let options = FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST | FOS_NODEREFERENCELINKS;
    let picked = show(owner, "Add a file or shortcut", options, None)?;
    log_info!("picked file: {picked}");
    Some(picked)
}

fn show(
    owner: HWND,
    title: &str,
    options: FILEOPENDIALOGOPTIONS,
    start_folder: Option<&str>,
) -> Option<String> {
    // SAFETY: every COM object is scoped to this call. The UI thread is already
    // an STA, which is what the dialog requires.
    unsafe {
        let dialog: IFileOpenDialog =
            match CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) {
                Ok(dialog) => dialog,
                Err(e) => {
                    log_warn!("could not create the file dialog: {e}");
                    return None;
                }
            };

        let _ = dialog.SetTitle(&HSTRING::from(title));
        // Replace rather than merge: the defaults include FOS_FORCEFILESYSTEM,
        // which would silently exclude Store apps.
        let _ = dialog.SetOptions(options);

        if let Some(folder) = start_folder
            && let Ok(item) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(folder), None)
        {
            let _ = dialog.SetFolder(&item);
        }

        // Cancelling returns an error, which is not worth logging.
        dialog.Show(Some(owner)).ok()?;
        let item = dialog.GetResult().ok()?;
        parsing_name(&item)
    }
}

/// The string that both `ShellExecuteW` and `IShellItemImageFactory` accept.
///
/// Filesystem items give a plain path. Apps give a shell namespace path
/// beginning `::{GUID}`, which `ShellExecuteW` does not accept, so those are
/// rebuilt as `shell:AppsFolder\<AppUserModelID>` from the item's own relative
/// name.
unsafe fn parsing_name(item: &IShellItem) -> Option<String> {
    unsafe {
        let absolute = display_name(item, SIGDN_DESKTOPABSOLUTEPARSING)?;
        if !absolute.starts_with("::") {
            return Some(absolute);
        }
        let aumid = display_name(item, SIGDN_PARENTRELATIVEPARSING)?;
        Some(format!("{APPS_FOLDER}\\{aumid}"))
    }
}

unsafe fn display_name(
    item: &IShellItem,
    kind: windows::Win32::UI::Shell::SIGDN,
) -> Option<String> {
    // SAFETY: GetDisplayName allocates with CoTaskMemAlloc; freed below on both
    // paths.
    unsafe {
        let raw = item.GetDisplayName(kind).ok()?;
        if raw.is_null() {
            return None;
        }
        let text = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
        text
    }
}
