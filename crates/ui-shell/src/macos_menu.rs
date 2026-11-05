use crate::messages::AppMenuCommand;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSString};
use std::sync::{Once, OnceLock};
use tokio::sync::broadcast;

const APP_NAME: &str = "Iris";

#[derive(Clone, Copy)]
struct KeyEquivalent<'a> {
    key: &'a NSString,
    masks: Option<NSEventModifierFlags>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct MenuActionHandler;

    unsafe impl NSObjectProtocol for MenuActionHandler {}

    impl MenuActionHandler {
        #[unsafe(method(irisOpenSelected:))]
        fn iris_open_selected(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::OpenSelected);
        }

        #[unsafe(method(irisCreateFolder:))]
        fn iris_create_folder(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::CreateFolder);
        }

        #[unsafe(method(irisEditPath:))]
        fn iris_edit_path(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::EditPath);
        }

        #[unsafe(method(irisNavigateBack:))]
        fn iris_navigate_back(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::NavigateBack);
        }

        #[unsafe(method(irisNavigateForward:))]
        fn iris_navigate_forward(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::NavigateForward);
        }

        #[unsafe(method(irisNavigateUp:))]
        fn iris_navigate_up(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::NavigateUp);
        }

        #[unsafe(method(irisCutSelected:))]
        fn iris_cut_selected(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::CutSelected);
        }

        #[unsafe(method(irisCopySelected:))]
        fn iris_copy_selected(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::CopySelected);
        }

        #[unsafe(method(irisPaste:))]
        fn iris_paste(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::Paste);
        }

        #[unsafe(method(irisUndo:))]
        fn iris_undo(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::Undo);
        }

        #[unsafe(method(irisRedo:))]
        fn iris_redo(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::Redo);
        }

        #[unsafe(method(irisDeleteSelected:))]
        fn iris_delete_selected(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::DeleteSelected);
        }

        #[unsafe(method(irisSelectAll:))]
        fn iris_select_all(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::SelectAll);
        }

        #[unsafe(method(irisOpenSettings:))]
        fn iris_open_settings(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::OpenSettings);
        }

        #[unsafe(method(irisRefresh:))]
        fn iris_refresh(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::Refresh);
        }

        #[unsafe(method(irisToggleSidebar:))]
        fn iris_toggle_sidebar(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::ToggleSidebar);
        }

        #[unsafe(method(irisTogglePreviewPanel:))]
        fn iris_toggle_preview_panel(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::TogglePreviewPanel);
        }

        #[unsafe(method(irisTogglePropertiesPanel:))]
        fn iris_toggle_properties_panel(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::TogglePropertiesPanel);
        }

        #[unsafe(method(irisToggleHiddenFiles:))]
        fn iris_toggle_hidden_files(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::ToggleHiddenFiles);
        }

        #[unsafe(method(irisOpenHelp:))]
        fn iris_open_help(&self, _sender: Option<&AnyObject>) {
            publish(AppMenuCommand::OpenHelp);
        }
    }
);

impl MenuActionHandler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send![super(allocated), init] }
    }
}

static MENU_INSTALL: Once = Once::new();
static MENU_TARGET: OnceLock<usize> = OnceLock::new();
static MENU_COMMANDS: OnceLock<broadcast::Sender<AppMenuCommand>> = OnceLock::new();

pub fn install_if_needed() {
    MENU_INSTALL.call_once(install);
}

pub fn subscribe() -> broadcast::Receiver<AppMenuCommand> {
    command_sender().subscribe()
}

fn install() {
    let mtm = MainThreadMarker::new().expect("macOS menu installation must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let target = action_target(mtm);

    let menubar = NSMenu::new(mtm);

    let app_menu = NSMenu::new(mtm);
    let services_menu = NSMenu::new(mtm);
    app_menu.addItem(&menu_item(
        mtm,
        ns_string!("About Iris"),
        Some(objc2::sel!(orderFrontStandardAboutPanel:)),
        None,
        None,
    ));
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    app_menu.addItem(&menu_item(
        mtm,
        ns_string!("Settings..."),
        Some(objc2::sel!(irisOpenSettings:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!(","),
            masks: None,
        }),
    ));
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    let services_item = menu_item(mtm, ns_string!("Services"), None, None, None);
    services_item.setSubmenu(Some(&services_menu));
    app_menu.addItem(&services_item);
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    app_menu.addItem(&menu_item(
        mtm,
        ns_string!("Hide Iris"),
        Some(objc2::sel!(hide:)),
        None,
        Some(KeyEquivalent {
            key: ns_string!("h"),
            masks: None,
        }),
    ));
    app_menu.addItem(&menu_item(
        mtm,
        ns_string!("Hide Others"),
        Some(objc2::sel!(hideOtherApplications:)),
        None,
        Some(KeyEquivalent {
            key: ns_string!("h"),
            masks: Some(NSEventModifierFlags::Option | NSEventModifierFlags::Command),
        }),
    ));
    app_menu.addItem(&menu_item(
        mtm,
        ns_string!("Show All"),
        Some(objc2::sel!(unhideAllApplications:)),
        None,
        None,
    ));
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    app_menu.addItem(&menu_item(
        mtm,
        ns_string!("Quit Iris"),
        Some(objc2::sel!(terminate:)),
        None,
        Some(KeyEquivalent {
            key: ns_string!("q"),
            masks: None,
        }),
    ));
    menubar.addItem(&submenu_root(mtm, ns_string!(APP_NAME), &app_menu));

    let file_menu = NSMenu::new(mtm);
    file_menu.addItem(&menu_item(
        mtm,
        ns_string!("Open"),
        Some(objc2::sel!(irisOpenSelected:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("o"),
            masks: None,
        }),
    ));
    file_menu.addItem(&menu_item(
        mtm,
        ns_string!("New Folder"),
        Some(objc2::sel!(irisCreateFolder:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("N"),
            masks: Some(NSEventModifierFlags::Shift | NSEventModifierFlags::Command),
        }),
    ));
    file_menu.addItem(&NSMenuItem::separatorItem(mtm));
    file_menu.addItem(&menu_item(
        mtm,
        ns_string!("Close Window"),
        Some(objc2::sel!(performClose:)),
        None,
        Some(KeyEquivalent {
            key: ns_string!("w"),
            masks: None,
        }),
    ));
    menubar.addItem(&submenu_root(mtm, ns_string!("File"), &file_menu));

    let edit_menu = NSMenu::new(mtm);
    edit_menu.addItem(&menu_item(
        mtm,
        ns_string!("Undo"),
        Some(objc2::sel!(irisUndo:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("z"),
            masks: None,
        }),
    ));
    edit_menu.addItem(&menu_item(
        mtm,
        ns_string!("Redo"),
        Some(objc2::sel!(irisRedo:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("z"),
            masks: Some(NSEventModifierFlags::Shift | NSEventModifierFlags::Command),
        }),
    ));
    edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
    edit_menu.addItem(&menu_item(
        mtm,
        ns_string!("Cut"),
        Some(objc2::sel!(irisCutSelected:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("x"),
            masks: None,
        }),
    ));
    edit_menu.addItem(&menu_item(
        mtm,
        ns_string!("Copy"),
        Some(objc2::sel!(irisCopySelected:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("c"),
            masks: None,
        }),
    ));
    edit_menu.addItem(&menu_item(
        mtm,
        ns_string!("Paste"),
        Some(objc2::sel!(irisPaste:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("v"),
            masks: None,
        }),
    ));
    edit_menu.addItem(&menu_item(
        mtm,
        ns_string!("Delete"),
        Some(objc2::sel!(irisDeleteSelected:)),
        Some(target),
        None,
    ));
    edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
    edit_menu.addItem(&menu_item(
        mtm,
        ns_string!("Select All"),
        Some(objc2::sel!(irisSelectAll:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("a"),
            masks: None,
        }),
    ));
    menubar.addItem(&submenu_root(mtm, ns_string!("Edit"), &edit_menu));

    let view_menu = NSMenu::new(mtm);
    view_menu.addItem(&menu_item(
        mtm,
        ns_string!("Refresh"),
        Some(objc2::sel!(irisRefresh:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("r"),
            masks: None,
        }),
    ));
    view_menu.addItem(&menu_item(
        mtm,
        ns_string!("Toggle Sidebar"),
        Some(objc2::sel!(irisToggleSidebar:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("s"),
            masks: Some(NSEventModifierFlags::Option | NSEventModifierFlags::Command),
        }),
    ));
    view_menu.addItem(&menu_item(
        mtm,
        ns_string!("Toggle Preview"),
        Some(objc2::sel!(irisTogglePreviewPanel:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("p"),
            masks: Some(NSEventModifierFlags::Option | NSEventModifierFlags::Command),
        }),
    ));
    view_menu.addItem(&menu_item(
        mtm,
        ns_string!("Toggle Properties"),
        Some(objc2::sel!(irisTogglePropertiesPanel:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("i"),
            masks: Some(NSEventModifierFlags::Option | NSEventModifierFlags::Command),
        }),
    ));
    view_menu.addItem(&menu_item(
        mtm,
        ns_string!("Toggle Hidden Files"),
        Some(objc2::sel!(irisToggleHiddenFiles:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("."),
            masks: Some(NSEventModifierFlags::Shift | NSEventModifierFlags::Command),
        }),
    ));
    view_menu.addItem(&NSMenuItem::separatorItem(mtm));
    view_menu.addItem(&menu_item(
        mtm,
        ns_string!("Enter Full Screen"),
        Some(objc2::sel!(toggleFullScreen:)),
        None,
        Some(KeyEquivalent {
            key: ns_string!("f"),
            masks: Some(NSEventModifierFlags::Control | NSEventModifierFlags::Command),
        }),
    ));
    menubar.addItem(&submenu_root(mtm, ns_string!("View"), &view_menu));

    let go_menu = NSMenu::new(mtm);
    go_menu.addItem(&menu_item(
        mtm,
        ns_string!("Back"),
        Some(objc2::sel!(irisNavigateBack:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("["),
            masks: None,
        }),
    ));
    go_menu.addItem(&menu_item(
        mtm,
        ns_string!("Forward"),
        Some(objc2::sel!(irisNavigateForward:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("]"),
            masks: None,
        }),
    ));
    go_menu.addItem(&NSMenuItem::separatorItem(mtm));
    go_menu.addItem(&menu_item(
        mtm,
        ns_string!("Enclosing Folder"),
        Some(objc2::sel!(irisNavigateUp:)),
        Some(target),
        None,
    ));
    go_menu.addItem(&menu_item(
        mtm,
        ns_string!("Edit Path..."),
        Some(objc2::sel!(irisEditPath:)),
        Some(target),
        Some(KeyEquivalent {
            key: ns_string!("l"),
            masks: None,
        }),
    ));
    menubar.addItem(&submenu_root(mtm, ns_string!("Go"), &go_menu));

    let window_menu = NSMenu::new(mtm);
    window_menu.addItem(&menu_item(
        mtm,
        ns_string!("Minimize"),
        Some(objc2::sel!(performMiniaturize:)),
        None,
        Some(KeyEquivalent {
            key: ns_string!("m"),
            masks: None,
        }),
    ));
    window_menu.addItem(&menu_item(
        mtm,
        ns_string!("Zoom"),
        Some(objc2::sel!(performZoom:)),
        None,
        None,
    ));
    window_menu.addItem(&NSMenuItem::separatorItem(mtm));
    window_menu.addItem(&menu_item(
        mtm,
        ns_string!("Bring All to Front"),
        Some(objc2::sel!(arrangeInFront:)),
        None,
        None,
    ));
    menubar.addItem(&submenu_root(mtm, ns_string!("Window"), &window_menu));

    let help_menu = NSMenu::new(mtm);
    help_menu.addItem(&menu_item(
        mtm,
        ns_string!("Iris Help"),
        Some(objc2::sel!(irisOpenHelp:)),
        Some(target),
        None,
    ));
    menubar.addItem(&submenu_root(mtm, ns_string!("Help"), &help_menu));

    app.setServicesMenu(Some(&services_menu));
    app.setWindowsMenu(Some(&window_menu));
    app.setHelpMenu(Some(&help_menu));
    app.setMainMenu(Some(&menubar));
}

fn command_sender() -> &'static broadcast::Sender<AppMenuCommand> {
    MENU_COMMANDS.get_or_init(|| {
        let (sender, _) = broadcast::channel(32);
        sender
    })
}

fn publish(command: AppMenuCommand) {
    let _ = command_sender().send(command);
}

fn action_target(mtm: MainThreadMarker) -> &'static AnyObject {
    let raw = *MENU_TARGET.get_or_init(|| Retained::into_raw(MenuActionHandler::new(mtm)) as usize);
    unsafe { &*(raw as *const AnyObject) }
}

fn submenu_root(mtm: MainThreadMarker, title: &NSString, submenu: &NSMenu) -> Retained<NSMenuItem> {
    let item = menu_item(mtm, title, None, None, None);
    item.setSubmenu(Some(submenu));
    item
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &NSString,
    selector: Option<objc2::runtime::Sel>,
    target: Option<&AnyObject>,
    key_equivalent: Option<KeyEquivalent<'_>>,
) -> Retained<NSMenuItem> {
    let (key, masks) = match key_equivalent {
        Some(ke) => (ke.key, ke.masks),
        None => (ns_string!(""), None),
    };

    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(mtm.alloc(), title, selector, key)
    };

    if let Some(masks) = masks {
        item.setKeyEquivalentModifierMask(masks);
    }

    unsafe {
        item.setTarget(target);
    }

    item
}
