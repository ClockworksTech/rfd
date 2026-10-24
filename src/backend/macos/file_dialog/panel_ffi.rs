use core::str;
use std::path::Path;
use std::path::PathBuf;

use block2::Block;
use objc2::rc::Retained;
use objc2::{msg_send, ClassType, MainThreadMarker};
use objc2_app_kit::{NSModalResponse, NSOpenPanel, NSSavePanel, NSWindow, NSWindowLevel};
use objc2_foundation::{NSArray, NSString, NSURL};
use raw_window_handle::RawWindowHandle;

use super::super::{
    modal_future::{AsModal, InnerModal},
    utils::{FocusManager, PolicyManager},
};
use crate::backend::macos::utils::window_from_raw_window_handle;
use crate::FileDialog;

extern "C" {
    pub fn CGShieldingWindowLevel() -> i32;
}

/// `+[NSOpenPanel openPanel]`, or `None` when AppKit returns nil instead of a
/// panel.
///
/// The open and save panels are hosted by a system XPC service, and when that
/// service can't be reached (a broken or interrupted connection, a bundle the
/// service refuses) AppKit returns nil, although the header says it never
/// does. The generated `NSOpenPanel::openPanel` binding panics on that, which
/// takes the whole app down; asking through `msg_send!` with an `Option`
/// return lets the dialog end as if it was cancelled instead.
fn new_open_panel(_mtm: MainThreadMarker) -> Option<Retained<NSOpenPanel>> {
    let panel: Option<Retained<NSOpenPanel>> =
        unsafe { msg_send![NSOpenPanel::class(), openPanel] };
    if panel.is_none() {
        log::error!("+[NSOpenPanel openPanel] returned nil; the dialog can't be shown");
    }
    panel
}

/// `+[NSSavePanel savePanel]`, or `None` when AppKit returns nil. See
/// [`new_open_panel`].
fn new_save_panel(_mtm: MainThreadMarker) -> Option<Retained<NSSavePanel>> {
    let panel: Option<Retained<NSSavePanel>> =
        unsafe { msg_send![NSSavePanel::class(), savePanel] };
    if panel.is_none() {
        log::error!("+[NSSavePanel savePanel] returned nil; the dialog can't be shown");
    }
    panel
}

pub struct Panel {
    // Either `NSSavePanel` or the subclass `NSOpenPanel`
    pub(crate) panel: Retained<NSSavePanel>,
    parent: Option<Retained<NSWindow>>,
    _focus_manager: FocusManager,
    _policy_manager: PolicyManager,
}

impl AsModal for Panel {
    fn inner_modal(&self) -> &(impl InnerModal + 'static) {
        &*self.panel
    }
}

impl InnerModal for NSSavePanel {
    fn begin_modal(&self, window: &NSWindow, handler: &Block<dyn Fn(NSModalResponse)>) {
        unsafe { self.beginSheetModalForWindow_completionHandler(window, handler) }
    }

    fn run_modal(&self) -> NSModalResponse {
        unsafe { self.runModal() }
    }
}

impl Panel {
    fn as_open_panel(&self) -> Option<&NSOpenPanel> {
        self.panel.downcast_ref::<NSOpenPanel>()
    }

    pub fn new(panel: Retained<NSSavePanel>, parent: Option<&RawWindowHandle>) -> Self {
        let _policy_manager = PolicyManager::new(MainThreadMarker::from(&*panel));

        let _focus_manager = FocusManager::new(MainThreadMarker::from(&*panel));

        panel.setLevel(unsafe { CGShieldingWindowLevel() } as NSWindowLevel);
        Self {
            panel,
            parent: parent.map(window_from_raw_window_handle),
            _focus_manager,
            _policy_manager,
        }
    }

    pub fn run_modal(&self) -> NSModalResponse {
        if let Some(parent) = self.parent.clone() {
            let completion = block2::StackBlock::new(|_: isize| {});

            unsafe {
                self.panel
                    .beginSheetModalForWindow_completionHandler(&parent, &completion)
            }
        }

        unsafe { self.panel.runModal() }
    }

    pub fn get_result(&self) -> PathBuf {
        unsafe {
            let url = self.panel.URL().unwrap();
            url.path().unwrap().to_string().into()
        }
    }

    pub fn get_results(&self) -> Vec<PathBuf> {
        unsafe {
            let urls = self.as_open_panel().unwrap().URLs();

            let mut res = Vec::new();
            for url in urls {
                res.push(url.path().unwrap().to_string().into());
            }

            res
        }
    }
}

trait PanelExt {
    fn panel(&self) -> &NSSavePanel;

    fn set_can_create_directories(&self, can: bool) {
        unsafe { self.panel().setCanCreateDirectories(can) }
    }

    fn add_filters(&self, opt: &FileDialog) {
        let mut exts: Vec<String> = Vec::new();

        for filter in opt.filters.iter() {
            exts.append(&mut filter.extensions.to_vec());
        }

        let f_raw: Vec<_> = exts.iter().map(|ext| NSString::from_str(ext)).collect();
        let array = NSArray::from_retained_slice(&f_raw);

        unsafe {
            #[allow(deprecated)]
            self.panel().setAllowedFileTypes(Some(&array));
        }
    }

    fn set_path(&self, path: &Path, file_name: Option<&str>) {
        // if file_name is some, and path is a dir
        let path = if let (Some(name), true) = (file_name, path.is_dir()) {
            let mut path = path.to_owned();
            // add a name to the end of path
            path.push(name);
            path
        } else {
            path.to_owned()
        };

        if let Some(path) = path.to_str() {
            unsafe {
                let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), true);
                self.panel().setDirectoryURL(Some(&url));
            }
        }
    }

    fn set_file_name(&self, file_name: &str) {
        unsafe {
            self.panel()
                .setNameFieldStringValue(&NSString::from_str(file_name))
        }
    }

    fn set_title(&self, title: &str) {
        unsafe { self.panel().setMessage(Some(&NSString::from_str(title))) }
    }
}

impl PanelExt for Retained<NSSavePanel> {
    fn panel(&self) -> &NSSavePanel {
        &self
    }
}

impl PanelExt for Retained<NSOpenPanel> {
    fn panel(&self) -> &NSSavePanel {
        &self
    }
}

impl Panel {
    pub fn build_pick_file(opt: &FileDialog, mtm: MainThreadMarker) -> Option<Self> {
        let panel = new_open_panel(mtm)?;

        if !opt.filters.is_empty() {
            panel.add_filters(opt);
        }

        if let Some(path) = &opt.starting_directory {
            panel.set_path(path, opt.file_name.as_deref());
        }

        if let Some(file_name) = &opt.file_name {
            panel.set_file_name(file_name);
        }

        if let Some(title) = &opt.title {
            panel.set_title(title);
        }

        if let Some(can) = opt.can_create_directories {
            panel.set_can_create_directories(can);
        }

        unsafe { panel.setCanChooseDirectories(false) };
        unsafe { panel.setCanChooseFiles(true) };

        Some(Self::new(Retained::into_super(panel), opt.parent.as_ref()))
    }

    pub fn build_save_file(opt: &FileDialog, mtm: MainThreadMarker) -> Option<Self> {
        let panel = new_save_panel(mtm)?;

        if !opt.filters.is_empty() {
            panel.add_filters(opt);
        }

        if let Some(path) = &opt.starting_directory {
            panel.set_path(path, opt.file_name.as_deref());
        }

        if let Some(file_name) = &opt.file_name {
            panel.set_file_name(file_name);
        }

        if let Some(title) = &opt.title {
            panel.set_title(title);
        }

        if let Some(can) = opt.can_create_directories {
            panel.set_can_create_directories(can);
        }

        Some(Self::new(panel, opt.parent.as_ref()))
    }

    pub fn build_pick_folder(opt: &FileDialog, mtm: MainThreadMarker) -> Option<Self> {
        let panel = new_open_panel(mtm)?;

        if let Some(path) = &opt.starting_directory {
            panel.set_path(path, opt.file_name.as_deref());
        }

        if let Some(title) = &opt.title {
            panel.set_title(title);
        }

        let can = opt.can_create_directories.unwrap_or(true);
        panel.set_can_create_directories(can);

        unsafe { panel.setCanChooseDirectories(true) };
        unsafe { panel.setCanChooseFiles(false) };

        Some(Self::new(Retained::into_super(panel), opt.parent.as_ref()))
    }

    pub fn build_pick_folders(opt: &FileDialog, mtm: MainThreadMarker) -> Option<Self> {
        let panel = new_open_panel(mtm)?;

        if let Some(path) = &opt.starting_directory {
            panel.set_path(path, opt.file_name.as_deref());
        }

        if let Some(title) = &opt.title {
            panel.set_title(title);
        }

        let can = opt.can_create_directories.unwrap_or(true);
        panel.set_can_create_directories(can);

        unsafe { panel.setCanChooseDirectories(true) };
        unsafe { panel.setCanChooseFiles(false) };
        unsafe { panel.setAllowsMultipleSelection(true) };

        Some(Self::new(Retained::into_super(panel), opt.parent.as_ref()))
    }

    pub fn build_pick_files(opt: &FileDialog, mtm: MainThreadMarker) -> Option<Self> {
        let panel = new_open_panel(mtm)?;

        if !opt.filters.is_empty() {
            panel.add_filters(opt);
        }

        if let Some(path) = &opt.starting_directory {
            panel.set_path(path, opt.file_name.as_deref());
        }

        if let Some(title) = &opt.title {
            panel.set_title(title);
        }

        if let Some(can) = opt.can_create_directories {
            panel.set_can_create_directories(can);
        }

        unsafe { panel.setCanChooseDirectories(false) };
        unsafe { panel.setCanChooseFiles(true) };
        unsafe { panel.setAllowsMultipleSelection(true) };

        Some(Self::new(Retained::into_super(panel), opt.parent.as_ref()))
    }

    pub fn build_pick_file_or_folder(opt: &FileDialog, mtm: MainThreadMarker) -> Option<Self> {
        let panel = new_open_panel(mtm)?;

        if !opt.filters.is_empty() {
            panel.add_filters(opt);
        }

        if let Some(path) = &opt.starting_directory {
            panel.set_path(path, opt.file_name.as_deref());
        }

        if let Some(title) = &opt.title {
            panel.set_title(title);
        }

        let can = opt.can_create_directories.unwrap_or(true);
        panel.set_can_create_directories(can);

        unsafe { panel.setCanChooseDirectories(true) };
        unsafe { panel.setCanChooseFiles(true) };
        unsafe { panel.setAllowsMultipleSelection(false) };

        Some(Self::new(Retained::into_super(panel), opt.parent.as_ref()))
    }

    pub fn build_pick_files_or_folders(opt: &FileDialog, mtm: MainThreadMarker) -> Option<Self> {
        let panel = new_open_panel(mtm)?;

        if !opt.filters.is_empty() {
            panel.add_filters(opt);
        }

        if let Some(path) = &opt.starting_directory {
            panel.set_path(path, opt.file_name.as_deref());
        }

        if let Some(title) = &opt.title {
            panel.set_title(title);
        }

        let can = opt.can_create_directories.unwrap_or(true);
        panel.set_can_create_directories(can);

        unsafe { panel.setCanChooseDirectories(true) };
        unsafe { panel.setCanChooseFiles(true) };
        unsafe { panel.setAllowsMultipleSelection(true) };

        Some(Self::new(Retained::into_super(panel), opt.parent.as_ref()))
    }
}
