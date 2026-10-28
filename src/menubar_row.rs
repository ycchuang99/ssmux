use crate::{
    config::ConnectionConfig,
    session::{ConnectionState, FailureKind},
};
use anyhow::{Context, Result};
use objc2::{
    class, define_class, msg_send, rc::Retained, sel, DefinedClass, MainThreadMarker,
    MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSBezelStyle, NSButton, NSCellImagePosition, NSColor, NSFont, NSImage,
    NSImageSymbolConfiguration, NSMenu, NSPopUpButton, NSTextField, NSView, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use tao::event_loop::EventLoopProxy;
use tray_icon::menu::{ContextMenu, MenuEvent, MenuId, Submenu};

#[link(name = "QuartzCore", kind = "framework")]
extern "C" {}

pub struct RowState {
    view: Retained<NSView>,
    button: Retained<NSButton>,
    action: MenuId,
    recovery_action: MenuId,
    recovery: Retained<NSButton>,
    status: Retained<NSTextField>,
    hint: Retained<NSTextField>,
    proxy: EventLoopProxy<MenuEvent>,
    name: String,
    interactive: bool,
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
            let state = self.ivars();
            let _ = state.proxy.send_event(MenuEvent { id: state.recovery_action.clone() });
        }
        #[unsafe(method(toggleSession:))]
        fn toggle_session(&self, _sender: &NSButton) {
            let state = self.ivars();
            let _ = state.proxy.send_event(MenuEvent { id: state.action.clone() });
        }
    }
);

impl ConnectionRow {
    pub fn new(
        connection: &ConnectionConfig,
        details: &Submenu,
        action: MenuId,
        recovery_action: MenuId,
        proxy: EventLoopProxy<MenuEvent>,
    ) -> Result<Retained<Self>> {
        let mtm = MainThreadMarker::new().context("menu rows require the main thread")?;
        let view = NSView::initWithFrame(
            NSView::alloc(mtm),
            rect(
                0.,
                0.,
                crate::menubar_popover::WIDTH,
                crate::menubar_popover::ROW_HEIGHT,
            ),
        );
        let title = if connection.pinned {
            format!("★ {}", connection.name)
        } else {
            connection.name.clone()
        };
        let name = label(&title, rect(56., 90., 380., 20.), mtm);
        name.setFont(Some(&NSFont::boldSystemFontOfSize(13.)));
        name.setToolTip(Some(&NSString::from_str(&connection.name)));
        view.addSubview(&name);
        let route = label(
            &crate::menubar::connection_route(connection),
            rect(56., 70., 490., 20.),
            mtm,
        );
        route.setTextColor(Some(&NSColor::secondaryLabelColor()));
        route.setToolTip(Some(&NSString::from_str(
            &crate::menubar::connection_route(connection),
        )));
        view.addSubview(&route);
        let identity = crate::menubar::connection_identity(connection);
        let profile = label(&identity, rect(56., 50., 490., 18.), mtm);
        profile.setFont(Some(&NSFont::systemFontOfSize(12.)));
        profile.setTextColor(Some(&NSColor::secondaryLabelColor()));
        profile.setToolTip(Some(&NSString::from_str(&identity)));
        view.addSubview(&profile);
        let status = label("Not Connected", rect(56., 28., 380., 20.), mtm);
        let hint = label("", rect(56., 8., 380., 18.), mtm);
        hint.setFont(Some(&NSFont::systemFontOfSize(12.)));
        hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
        view.addSubview(&status);
        view.addSubview(&hint);
        let recovery = NSButton::new(mtm);
        recovery.setFrame(rect(448., 12., 100., 28.));
        recovery.setBezelStyle(NSBezelStyle::Push);
        recovery.setHidden(true);
        view.addSubview(&recovery);
        let button = NSButton::new(mtm);
        button.setFrame(rect(10., 66., 36., 36.));
        button.setBordered(true);
        button.setBezelStyle(NSBezelStyle::Circular);
        button.setShowsBorderOnlyWhileMouseInside(true);
        button.setTitle(&NSString::from_str(""));
        button.setImagePosition(NSCellImagePosition::ImageOnly);
        button.setWantsLayer(true);
        view.addSubview(&button);
        let more = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            rect(448., 82., 100., 28.),
            true,
        );
        more.setBezelStyle(NSBezelStyle::Push);
        unsafe {
            more.setMenu(Some(&*details.ns_menu().cast::<NSMenu>()));
        }
        more.setAccessibilityLabel(Some(&NSString::from_str(&format!(
            "Manage {}",
            connection.name
        ))));
        more.setToolTip(Some(&NSString::from_str(
            "Session details, copy, pin, edit, and delete",
        )));
        view.addSubview(&more);
        let row = Self::alloc(mtm).set_ivars(RowState {
            view,
            button,
            action,
            recovery_action,
            recovery,
            status,
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
        }
        row.update(ConnectionState::Stopped, None, None, 0);
        Ok(row)
    }

    pub fn view(&self) -> &NSView {
        &self.ivars().view
    }

    pub fn update(
        &self,
        state: ConnectionState,
        error: Option<&str>,
        failure: Option<FailureKind>,
        retry_attempt: u8,
    ) {
        let row = self.ivars();
        let (symbol, action, status, color) = match state {
            ConnectionState::Running => (
                "arrow.left.arrow.right.circle.fill",
                "Disconnect",
                "Tunnel Ready",
                NSColor::systemGreenColor(),
            ),
            ConnectionState::Connecting | ConnectionState::Reconnecting => (
                "hourglass.circle.fill",
                "Cancel",
                if state == ConnectionState::Reconnecting {
                    "Waiting to Reconnect…"
                } else {
                    "Connecting…"
                },
                NSColor::systemOrangeColor(),
            ),
            ConnectionState::Disconnected => (
                "exclamationmark.arrow.circlepath",
                "Reconnect",
                "Connection Interrupted",
                NSColor::systemOrangeColor(),
            ),
            ConnectionState::Stopping => (
                "hourglass.circle.fill",
                "Disconnecting",
                "Disconnecting",
                NSColor::systemOrangeColor(),
            ),
            ConnectionState::Stopped => (
                "arrow.left.arrow.right.circle.fill",
                "Connect",
                "Not Connected",
                NSColor::systemGrayColor(),
            ),
            ConnectionState::Failed => (
                "exclamationmark.circle.fill",
                "Reconnect",
                "Failed",
                NSColor::systemRedColor(),
            ),
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
        row.status
            .setToolTip(Some(&NSString::from_str(error.unwrap_or(status_text))));
        let hint = if row.interactive {
            "Choose a port-forwarding document in Edit Session.".to_owned()
        } else if problem {
            failure
                .map(|kind| kind.suggestion().to_owned())
                .unwrap_or_else(|| "Open Details for the diagnostic output.".to_owned())
        } else if state == ConnectionState::Running {
            "Remote service availability is not verified.".to_owned()
        } else if state == ConnectionState::Reconnecting {
            format!("Retry {retry_attempt} of 3 · Click Cancel to stop.")
        } else if state == ConnectionState::Connecting {
            "Waiting for the Session Manager tunnel.".to_owned()
        } else {
            String::new()
        };
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
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            Some(&NSString::from_str(description)),
        );
        row.button.setImage(image.as_deref());
        let palette =
            NSImageSymbolConfiguration::configurationWithPaletteColors(&NSArray::from_slice(&[
                &*NSColor::blackColor(),
                &*color,
            ]));
        row.button.setSymbolConfiguration(Some(
            &NSImageSymbolConfiguration::configurationWithPointSize_weight(30., 0.0)
                .configurationByApplyingConfiguration(&palette),
        ));
        let animate = state == ConnectionState::Running
            && row.view.window().is_some_and(|window| window.isVisible())
            && !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
        // Core Animation keeps the pulse smooth without adding a UI polling timer.
        unsafe {
            let layer: Option<Retained<NSObject>> = msg_send![&row.button, layer];
            if let Some(layer) = layer {
                let key = NSString::from_str("connectionPulse");
                let existing: Option<Retained<NSObject>> =
                    msg_send![&layer, animationForKey: &*key];
                if !animate {
                    let _: () = msg_send![&layer, removeAnimationForKey: &*key];
                } else if existing.is_none() {
                    let animation: Retained<NSObject> = msg_send![class!(CABasicAnimation),
                        animationWithKeyPath: &*NSString::from_str("transform.scale")];
                    let _: () =
                        msg_send![&animation, setFromValue: &*NSNumber::numberWithDouble(1.0)];
                    let _: () =
                        msg_send![&animation, setToValue: &*NSNumber::numberWithDouble(0.94)];
                    let _: () = msg_send![&animation, setDuration: 1.2_f64];
                    let _: () = msg_send![&animation, setAutoreverses: true];
                    let _: () = msg_send![&animation, setRepeatCount: f32::INFINITY];
                    let _: () = msg_send![&layer, addAnimation: &*animation, forKey: &*key];
                }
            }
        }
        row.button
            .setEnabled(!row.interactive && state != ConnectionState::Stopping);
        row.button
            .setAccessibilityLabel(Some(&NSString::from_str(&format!(
                "{description} {}. {status}",
                row.name
            ))));
        row.button.setToolTip(Some(&NSString::from_str(&format!(
            "{status} — {description}"
        ))));
        row.view
            .setToolTip(Some(&NSString::from_str(error.unwrap_or(status))));
    }
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
