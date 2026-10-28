use std::cell::{Cell, RefCell};

use crate::{
    config::ConnectionConfig,
    menubar_popover::{ROW_HEIGHT, WIDTH},
    session::{ConnectionState, FailureKind},
};
use anyhow::{Context, Result};
use objc2::{
    define_class, msg_send, rc::Retained, sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSBezelStyle, NSBox, NSBoxType, NSButton, NSCellImagePosition, NSColor,
    NSControlSize, NSFont, NSImage, NSImageSymbolConfiguration, NSMenu, NSPopUpArrowPosition,
    NSPopUpButton, NSPopUpButtonCell, NSProgressIndicator, NSProgressIndicatorStyle, NSTextField,
    NSTitlePosition, NSView, NSWorkspace,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use tao::event_loop::EventLoopProxy;
use tray_icon::menu::{ContextMenu, MenuEvent, MenuId, Submenu};

pub struct RowState {
    view: Retained<NSView>,
    header: Retained<NSView>,
    details: Retained<NSView>,
    disclosure: Retained<NSButton>,
    expanded: Cell<bool>,
    button: Retained<NSButton>,
    action: MenuId,
    recovery_action: MenuId,
    copy_action: MenuId,
    layout_action: MenuId,
    recovery: Retained<NSButton>,
    status: Retained<NSTextField>,
    dot: Retained<NSTextField>,
    spinner: Retained<NSProgressIndicator>,
    last_status: RefCell<Option<LastStatus>>,
    hint: Retained<NSTextField>,
    proxy: EventLoopProxy<MenuEvent>,
    name: String,
    interactive: bool,
}

struct LastStatus {
    state: ConnectionState,
    error: Option<String>,
    failure: Option<FailureKind>,
    retry_attempt: u8,
    reduce_motion: bool,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = RowState]
    pub struct ConnectionRow;
    unsafe impl NSObjectProtocol for ConnectionRow {}
    impl ConnectionRow {
        #[unsafe(method(recoverSession:))]
        fn recover_session(&self, _sender: &NSButton) {
            self.send(self.ivars().recovery_action.clone());
        }
        #[unsafe(method(toggleSession:))]
        fn toggle_session(&self, _sender: &NSButton) {
            self.send(self.ivars().action.clone());
        }
        #[unsafe(method(copyAddress:))]
        fn copy_address(&self, _sender: &NSButton) {
            self.send(self.ivars().copy_action.clone());
        }
        #[unsafe(method(toggleDetails:))]
        fn toggle_details(&self, _sender: &NSButton) {
            let row = self.ivars();
            row.expanded.set(!row.expanded.get());
            self.update_disclosure();
            self.layout();
        }
    }
);

impl ConnectionRow {
    pub fn new(
        connection: &ConnectionConfig,
        actions: &Submenu,
        action: MenuId,
        recovery_action: MenuId,
        copy_action: MenuId,
        layout_action: MenuId,
        proxy: EventLoopProxy<MenuEvent>,
    ) -> Result<Retained<Self>> {
        let mtm = MainThreadMarker::new().context("menu rows require the main thread")?;
        let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., WIDTH, ROW_HEIGHT));
        let header = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., WIDTH, ROW_HEIGHT));
        view.addSubview(&header);
        let title = if connection.pinned {
            format!("★ {}", connection.name)
        } else {
            connection.name.clone()
        };
        let name = label(&title, rect(40., 40., WIDTH - 160., 20.), mtm);
        name.setFont(Some(&NSFont::boldSystemFontOfSize(13.)));
        name.setToolTip(Some(&NSString::from_str(&connection.name)));
        header.addSubview(&name);
        let address = crate::menubar::local_address(connection);
        let route = label(
            address.as_deref().unwrap_or("No local address"),
            rect(58., 16., 200., 20.),
            mtm,
        );
        route.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(12., 0.)));
        route.setTextColor(Some(&NSColor::secondaryLabelColor()));
        route.sizeToFit();
        header.addSubview(&route);
        let copy = icon_button("doc.on.doc", "Copy Local Address", mtm);
        copy.setFrame(rect(60. + route.frame().size.width, 10., 28., 28.));
        copy.setHidden(address.is_none());
        copy.setAccessibilityLabel(Some(&NSString::from_str(&format!(
            "Copy local address for {}",
            connection.name
        ))));
        header.addSubview(&copy);
        let dot = label("○", rect(40., 16., 16., 18.), mtm);
        dot.setFont(Some(&NSFont::boldSystemFontOfSize(12.)));
        header.addSubview(&dot);
        let spinner = NSProgressIndicator::new(mtm);
        spinner.setStyle(NSProgressIndicatorStyle::Spinning);
        spinner.setControlSize(NSControlSize::Small);
        spinner.setIndeterminate(true);
        spinner.setDisplayedWhenStopped(false);
        spinner.setFrame(rect(40., 16., 16., 16.));
        spinner.setHidden(true);
        header.addSubview(&spinner);
        let status = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        status.setFont(Some(&NSFont::systemFontOfSize(12.)));
        status.setTextColor(Some(&NSColor::labelColor()));
        status.setHidden(true);
        view.addSubview(&status);
        let hint = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        hint.setFont(Some(&NSFont::systemFontOfSize(12.)));
        hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
        view.addSubview(&hint);
        let recovery = NSButton::new(mtm);
        recovery.setBezelStyle(NSBezelStyle::Push);
        recovery.setControlSize(NSControlSize::Small);
        recovery.setFont(Some(&NSFont::systemFontOfSize(12.)));
        recovery.setHidden(true);
        view.addSubview(&recovery);
        let button = NSButton::new(mtm);
        button.setFrame(rect(WIDTH - 112., 38., 72., 24.));
        button.setBezelStyle(NSBezelStyle::Push);
        button.setControlSize(NSControlSize::Small);
        button.setFont(Some(&NSFont::systemFontOfSize(12.)));
        header.addSubview(&button);
        let disclosure = icon_button("chevron.right", "Show Session Details", mtm);
        disclosure.setFrame(rect(8., 34., 28., 28.));
        header.addSubview(&disclosure);
        let more = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            rect(WIDTH - 36., 38., 24., 24.),
            true,
        );
        more.setBezelStyle(NSBezelStyle::Push);
        more.setControlSize(NSControlSize::Small);
        more.setFont(Some(&NSFont::systemFontOfSize(12.)));
        unsafe {
            more.setMenu(Some(&*actions.ns_menu().cast::<NSMenu>()));
        }
        if let Some(cell) = more.cell() {
            if let Some(cell) = cell.downcast_ref::<NSPopUpButtonCell>() {
                cell.setArrowPosition(NSPopUpArrowPosition::NoArrow);
            }
        }
        more.setTitle(&NSString::from_str("…"));
        more.setAccessibilityLabel(Some(&NSString::from_str(&format!(
            "Actions for {}",
            connection.name
        ))));
        more.setToolTip(Some(&NSString::from_str("Session Actions")));
        header.addSubview(&more);
        let details = session_details(connection, mtm);
        view.addSubview(&details);
        let separator = NSBox::new(mtm);
        separator.setBoxType(NSBoxType::Separator);
        separator.setFrame(rect(16., 0., WIDTH - 32., 1.));
        view.addSubview(&separator);
        let row = Self::alloc(mtm).set_ivars(RowState {
            view,
            header,
            details,
            disclosure,
            expanded: Cell::new(false),
            button,
            action,
            recovery_action,
            copy_action,
            layout_action,
            recovery,
            status,
            dot,
            spinner,
            last_status: RefCell::new(None),
            hint,
            proxy,
            name: connection.name.clone(),
            interactive: crate::ssm::is_interactive(connection),
        });
        let row: Retained<Self> = unsafe { msg_send![super(row), init] };
        unsafe {
            row.ivars().recovery.setTarget(Some(&row));
            row.ivars().recovery.setAction(Some(sel!(recoverSession:)));
            row.ivars().button.setTarget(Some(&row));
            row.ivars().button.setAction(Some(sel!(toggleSession:)));
            row.ivars().disclosure.setTarget(Some(&row));
            row.ivars().disclosure.setAction(Some(sel!(toggleDetails:)));
            copy.setTarget(Some(&row));
            copy.setAction(Some(sel!(copyAddress:)));
        }
        row.update_disclosure();
        row.update(ConnectionState::Stopped, None, None, 0);
        Ok(row)
    }

    fn send(&self, id: MenuId) {
        let _ = self.ivars().proxy.send_event(MenuEvent { id });
    }

    pub fn view(&self) -> &NSView {
        &self.ivars().view
    }

    fn layout(&self) {
        let row = self.ivars();
        let expanded = row.expanded.get();
        let details_height = if expanded {
            row.details.frame().size.height
        } else {
            0.
        };
        row.details.setHidden(!expanded);
        let status_height = if row.status.isHidden() {
            0.
        } else {
            text_height(&row.status, WIDTH - 56.) + 6.
        };
        let hint_width = WIDTH - if row.recovery.isHidden() { 56. } else { 152. };
        let hint_height = if row.hint.stringValue().is_empty() {
            0.
        } else {
            text_height(&row.hint, hint_width).max(24.) + 8.
        };
        row.hint.setFrame(rect(
            40.,
            details_height + 8.,
            hint_width,
            (hint_height - 8.).max(0.),
        ));
        row.hint.setHidden(hint_height == 0.);
        row.recovery
            .setFrame(rect(WIDTH - 106., details_height + 8., 90., 24.));
        row.status.setFrame(rect(
            40.,
            details_height + hint_height + 6.,
            WIDTH - 56.,
            (status_height - 6.).max(0.),
        ));
        let height = ROW_HEIGHT + status_height + hint_height + details_height;
        row.header
            .setFrame(rect(0., height - ROW_HEIGHT, WIDTH, ROW_HEIGHT));
        let mut frame = row.view.frame();
        let changed = frame.size.height != height;
        frame.size.height = height;
        row.view.setFrame(frame);
        if changed {
            self.send(row.layout_action.clone());
        }
    }

    fn update_disclosure(&self) {
        let row = self.ivars();
        let expanded = row.expanded.get();
        let description = format!(
            "{} details for {}",
            if expanded { "Hide" } else { "Show" },
            row.name
        );
        row.disclosure
            .setAccessibilityLabel(Some(&NSString::from_str(&description)));
        row.disclosure.setAccessibilityExpanded(expanded);
        row.disclosure
            .setToolTip(Some(&NSString::from_str(&description)));
        set_symbol(
            &row.disclosure,
            if expanded {
                "chevron.down"
            } else {
                "chevron.right"
            },
            &description,
        );
    }

    pub fn update(
        &self,
        state: ConnectionState,
        error: Option<&str>,
        failure: Option<FailureKind>,
        retry_attempt: u8,
    ) {
        let row = self.ivars();
        let reduce_motion = NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
        if row.last_status.borrow().as_ref().is_some_and(|last| {
            last.state == state
                && last.error.as_deref() == error
                && last.failure == failure
                && last.retry_attempt == retry_attempt
                && last.reduce_motion == reduce_motion
        }) {
            return;
        }
        *row.last_status.borrow_mut() = Some(LastStatus {
            state,
            error: error.map(str::to_owned),
            failure,
            retry_attempt,
            reduce_motion,
        });
        let (action, status, color) = match state {
            ConnectionState::Running => ("Disconnect", "Tunnel Ready", NSColor::systemGreenColor()),
            ConnectionState::Connecting => ("Cancel", "Connecting…", NSColor::systemOrangeColor()),
            ConnectionState::Reconnecting => (
                "Cancel",
                "Waiting to Reconnect…",
                NSColor::systemOrangeColor(),
            ),
            ConnectionState::Disconnected => (
                "Reconnect",
                "Connection Interrupted",
                NSColor::systemOrangeColor(),
            ),
            ConnectionState::Stopping => {
                ("Disconnect", "Disconnecting…", NSColor::systemOrangeColor())
            }
            ConnectionState::Stopped => ("Connect", "Disconnected", NSColor::secondaryLabelColor()),
            ConnectionState::Failed => ("Reconnect", "Failed", NSColor::systemRedColor()),
        };
        let problem = matches!(
            state,
            ConnectionState::Failed | ConnectionState::Disconnected
        );
        let status_text = if problem {
            failure.map(|kind| kind.summary()).unwrap_or(status)
        } else {
            status
        };
        row.status.setStringValue(&NSString::from_str(status_text));
        row.status.setHidden(!problem);
        row.status
            .setToolTip(Some(&NSString::from_str(error.unwrap_or(status_text))));
        let status_description = if state == ConnectionState::Running {
            "Tunnel Ready. Remote service availability is not verified."
        } else {
            status_text
        };
        row.dot.setStringValue(&NSString::from_str(match state {
            ConnectionState::Running => "✓",
            ConnectionState::Stopped => "○",
            ConnectionState::Connecting
            | ConnectionState::Reconnecting
            | ConnectionState::Stopping => "⌛︎",
            _ => "!",
        }));
        row.dot.setTextColor(Some(&color));
        row.dot
            .setAccessibilityLabel(Some(&NSString::from_str(status_description)));
        row.dot
            .setToolTip(Some(&NSString::from_str(status_description)));
        let pending = matches!(
            state,
            ConnectionState::Connecting | ConnectionState::Reconnecting | ConnectionState::Stopping
        );
        let animate = pending && !reduce_motion;
        row.dot.setHidden(animate);
        row.spinner
            .setAccessibilityLabel(Some(&NSString::from_str(status_text)));
        row.spinner
            .setToolTip(Some(&NSString::from_str(status_text)));
        if animate && row.spinner.isHidden() {
            row.spinner.setHidden(false);
            unsafe {
                row.spinner.startAnimation(None);
            }
        } else if !animate && !row.spinner.isHidden() {
            unsafe {
                row.spinner.stopAnimation(None);
            }
            row.spinner.setHidden(true);
        }
        let hint = status_hint(state, failure, retry_attempt, row.interactive);
        row.hint.setStringValue(&NSString::from_str(&hint));
        row.hint.setToolTip(Some(&NSString::from_str(&hint)));
        let recovery_title = match failure {
            Some(FailureKind::SsoExpired) => "Sign In…",
            Some(FailureKind::PortInUse) => "Change Port…",
            Some(FailureKind::MissingDependency) => "Check Setup",
            Some(FailureKind::Network | FailureKind::Timeout) => "Reconnect",
            _ => "Details…",
        };
        row.recovery.setTitle(&NSString::from_str(recovery_title));
        row.recovery.setHidden(!problem);
        row.recovery
            .setAccessibilityLabel(Some(&NSString::from_str(&format!(
                "{recovery_title} {}",
                row.name
            ))));
        let description = if row.interactive {
            "Edit to choose a forwarding document"
        } else {
            action
        };
        row.button.setTitle(&NSString::from_str(action));
        row.button
            .setEnabled(!row.interactive && state != ConnectionState::Stopping);
        row.button
            .setAccessibilityLabel(Some(&NSString::from_str(&format!(
                "{description} {}. {status}",
                row.name
            ))));
        row.button.setToolTip(Some(&NSString::from_str(&format!(
            "{status_description} — {description}"
        ))));
        self.layout();
    }
}

fn status_hint(
    state: ConnectionState,
    failure: Option<FailureKind>,
    retry_attempt: u8,
    interactive: bool,
) -> String {
    if interactive {
        "Choose a port-forwarding document in Edit Session.".to_owned()
    } else if matches!(
        state,
        ConnectionState::Failed | ConnectionState::Disconnected
    ) {
        failure
            .map(|kind| kind.suggestion().to_owned())
            .unwrap_or_else(|| "Open Details for the diagnostic output.".to_owned())
    } else if state == ConnectionState::Reconnecting {
        format!("Retry {retry_attempt} of 3 · Click Cancel to stop.")
    } else {
        String::new()
    }
}

fn destination(connection: &ConnectionConfig) -> String {
    let host = connection
        .parameters
        .get("host")
        .unwrap_or(&connection.target);
    match connection.parameters.get("portNumber") {
        Some(port) => format!("{host}:{port}"),
        None => host.clone(),
    }
}

fn session_details(connection: &ConnectionConfig, mtm: MainThreadMarker) -> Retained<NSView> {
    let width = WIDTH - 56.;
    let destination = detail_field("Destination", &destination(connection), width, mtm);
    let target = detail_field("SSM Target", &connection.target, width, mtm);
    let profile = detail_field(
        "Profile",
        connection.profile.as_deref().unwrap_or("AWS default"),
        (width - 16.) / 2.,
        mtm,
    );
    let region = detail_field(
        "Region",
        connection.region.as_deref().unwrap_or("AWS default"),
        (width - 16.) / 2.,
        mtm,
    );
    let metadata_height = profile.frame().size.height.max(region.frame().size.height);
    let height =
        destination.frame().size.height + target.frame().size.height + metadata_height + 48.;
    let details = NSView::initWithFrame(NSView::alloc(mtm), rect(16., 0., WIDTH - 32., height));
    let background = NSBox::new(mtm);
    background.setBoxType(NSBoxType::Custom);
    background.setTitlePosition(NSTitlePosition::NoTitle);
    background.setBorderWidth(0.5);
    background.setBorderColor(&NSColor::separatorColor());
    background.setFillColor(&NSColor::controlBackgroundColor());
    background.setCornerRadius(6.);
    background.setFrame(rect(0., 8., WIDTH - 32., height - 8.));
    details.addSubview(&background);
    let mut y = height - 12.;
    for field in [&destination, &target] {
        y -= field.frame().size.height;
        field.setFrameOrigin(NSPoint::new(12., y));
        details.addSubview(field);
        y -= 8.;
    }
    for (field, x) in [(&profile, 12.), (&region, 20. + width / 2.)] {
        field.setFrameOrigin(NSPoint::new(x, y - field.frame().size.height));
        details.addSubview(field);
    }
    details
}

fn detail_field(title: &str, value: &str, width: f64, mtm: MainThreadMarker) -> Retained<NSView> {
    let value_label = NSTextField::wrappingLabelWithString(&NSString::from_str(value), mtm);
    value_label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(12., 0.)));
    value_label.setSelectable(true);
    value_label.setToolTip(Some(&NSString::from_str(value)));
    let value_height = text_height(&value_label, width);
    value_label.setFrame(rect(0., 0., width, value_height));
    let title_label = label(title, rect(0., value_height + 2., width, 16.), mtm);
    title_label.setFont(Some(&NSFont::systemFontOfSize(12.)));
    title_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let field = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., width, value_height + 18.));
    field.addSubview(&title_label);
    field.addSubview(&value_label);
    field
}

fn text_height(field: &NSTextField, width: f64) -> f64 {
    field
        .cell()
        .map(|cell| {
            cell.cellSizeForBounds(rect(0., 0., width, 10_000.))
                .height
                .ceil()
        })
        .unwrap_or(18.)
        .max(18.)
}

fn icon_button(symbol: &str, description: &str, mtm: MainThreadMarker) -> Retained<NSButton> {
    let button = NSButton::new(mtm);
    button.setBezelStyle(NSBezelStyle::Push);
    button.setShowsBorderOnlyWhileMouseInside(true);
    button.setControlSize(NSControlSize::Small);
    button.setSymbolConfiguration(Some(
        &NSImageSymbolConfiguration::configurationWithPointSize_weight(11., 0.),
    ));
    button.setTitle(&NSString::from_str(""));
    button.setImagePosition(NSCellImagePosition::ImageOnly);
    button.setAccessibilityLabel(Some(&NSString::from_str(description)));
    button.setToolTip(Some(&NSString::from_str(description)));
    set_symbol(&button, symbol, description);
    button
}

fn set_symbol(button: &NSButton, symbol: &str, description: &str) {
    button.setImage(
        NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            Some(&NSString::from_str(description)),
        )
        .as_deref(),
    );
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn label(text: &str, frame: NSRect, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(frame);
    label.setFont(Some(&NSFont::systemFontOfSize(13.)));
    label.setTextColor(Some(&NSColor::labelColor()));
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routine_states_stay_compact_while_failures_and_retries_keep_guidance() {
        for state in [
            ConnectionState::Stopped,
            ConnectionState::Connecting,
            ConnectionState::Running,
            ConnectionState::Stopping,
        ] {
            assert!(status_hint(state, None, 0, false).is_empty());
        }
        assert_eq!(
            status_hint(
                ConnectionState::Failed,
                Some(FailureKind::SsoExpired),
                0,
                false
            ),
            FailureKind::SsoExpired.suggestion()
        );
        assert_eq!(
            status_hint(ConnectionState::Reconnecting, None, 2, false),
            "Retry 2 of 3 · Click Cancel to stop."
        );
        assert!(!status_hint(ConnectionState::Stopped, None, 0, true).is_empty());
    }

    #[test]
    fn expanded_destination_retains_remote_port_and_target_fallback() {
        let mut connection: ConnectionConfig = toml::from_str(
            "name = 'database'\ntarget = 'i-123'\n[parameters]\nportNumber = '5432'\n",
        )
        .unwrap();
        assert_eq!(destination(&connection), "i-123:5432");
        connection
            .parameters
            .insert("host".into(), "db.example.test".into());
        assert_eq!(destination(&connection), "db.example.test:5432");
        connection.parameters.remove("portNumber");
        assert_eq!(destination(&connection), "db.example.test");
    }
}
