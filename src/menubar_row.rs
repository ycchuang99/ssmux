use crate::{config::ConnectionConfig, session::ConnectionState};
use anyhow::{Context, Result};
use objc2::{
    class, define_class, msg_send, rc::Retained, sel, DefinedClass, MainThreadMarker,
    MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSBezelStyle, NSButton, NSCellImagePosition, NSColor, NSFont, NSImage,
    NSImageSymbolConfiguration, NSLineBreakMode, NSMenu, NSPopUpButton, NSTextField, NSView,
    NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use tao::event_loop::EventLoopProxy;
use tray_icon::menu::{ContextMenu, MenuEvent, MenuId, Submenu};

#[link(name = "QuartzCore", kind = "framework")]
extern "C" {}

pub const ROW_HEIGHT: f64 = 80.;

pub struct RowState {
    view: Retained<NSView>,
    button: Retained<NSButton>,
    status: Retained<NSTextField>,
    action: MenuId,
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
        proxy: EventLoopProxy<MenuEvent>,
    ) -> Result<Retained<Self>> {
        let mtm = MainThreadMarker::new().context("menu rows require the main thread")?;
        let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., 420., ROW_HEIGHT));
        let name = label(&connection.name, rect(56., 54., 252., 20.), mtm);
        name.setFont(Some(&NSFont::boldSystemFontOfSize(13.)));
        name.setToolTip(Some(&NSString::from_str(&connection.name)));
        view.addSubview(&name);
        let route = label(
            &crate::menubar::connection_route(connection),
            rect(56., 32., 252., 20.),
            mtm,
        );
        route.setTextColor(Some(&NSColor::secondaryLabelColor()));
        route.setFont(NSFont::userFixedPitchFontOfSize(12.).as_deref());
        if let Some(cell) = route.cell() {
            cell.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        }
        view.addSubview(&route);
        let status = label("", rect(56., 10., 352., 18.), mtm);
        status.setFont(Some(&NSFont::systemFontOfSize(12.)));
        view.addSubview(&status);
        let button = NSButton::new(mtm);
        button.setFrame(rect(10., 30., 36., 36.));
        button.setBordered(true);
        button.setBezelStyle(NSBezelStyle::Circular);
        button.setShowsBorderOnlyWhileMouseInside(true);
        button.setTitle(&NSString::from_str(""));
        button.setImagePosition(NSCellImagePosition::ImageOnly);
        button.setWantsLayer(true);
        view.addSubview(&button);
        let more = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            rect(320., 38., 88., 28.),
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
            "Session details, edit, and delete",
        )));
        view.addSubview(&more);
        unsafe {
            button.setNextKeyView(Some(&more));
        }
        let row = Self::alloc(mtm).set_ivars(RowState {
            view,
            button,
            status,
            action,
            proxy,
            name: connection.name.clone(),
            interactive: crate::ssm::is_interactive(connection),
        });
        let row: Retained<Self> = unsafe { msg_send![super(row), init] };
        unsafe {
            row.ivars().button.setTarget(Some(&row));
            row.ivars().button.setAction(Some(sel!(toggleSession:)));
        }
        row.update(ConnectionState::Stopped, None);
        Ok(row)
    }

    pub fn view(&self) -> &NSView {
        &self.ivars().view
    }

    pub fn update(&self, state: ConnectionState, error: Option<&str>) {
        let row = self.ivars();
        let (symbol, action, status, color) = match state {
            ConnectionState::Starting => (
                "hourglass.circle.fill",
                "Cancel Connection",
                "Starting…",
                NSColor::systemOrangeColor(),
            ),
            ConnectionState::Forwarding => (
                "checkmark.circle.fill",
                "Disconnect",
                "Port Open",
                NSColor::systemGreenColor(),
            ),
            ConnectionState::Running => (
                "arrow.left.arrow.right.circle.fill",
                "Stop Session",
                "Process Running",
                NSColor::systemGreenColor(),
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
                "Disconnected",
                NSColor::systemGrayColor(),
            ),
            ConnectionState::Failed => (
                "exclamationmark.circle.fill",
                "Retry",
                "Failed",
                NSColor::systemRedColor(),
            ),
        };
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
        let animate = state == ConnectionState::Forwarding
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
        let status_text = if row.interactive {
            "Choose a forwarding document in Edit Session".to_owned()
        } else if state == ConnectionState::Failed {
            match error {
                Some(error) => format!("Failed — {}", crate::session::error_summary(error)),
                None => "Failed — Retry or view details".to_owned(),
            }
        } else {
            status.to_owned()
        };
        let status_detail = error.unwrap_or(match state {
            ConnectionState::Starting => "Waiting for the local forwarding port. Click to cancel.",
            ConnectionState::Forwarding => {
                "Local forwarding port is open; remote service availability is not verified."
            }
            ConnectionState::Running => {
                "Session process is running; forwarding readiness is unknown for this document."
            }
            _ => &status_text,
        });
        row.status.setStringValue(&NSString::from_str(&status_text));
        row.status
            .setToolTip(Some(&NSString::from_str(status_detail)));
        row.status
            .setAccessibilityLabel(Some(&NSString::from_str(&format!(
                "{}: {status_text}. {status_detail}",
                row.name
            ))));
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
    label.setMaximumNumberOfLines(1);
    label.setToolTip(Some(&NSString::from_str(text)));
    if let Some(cell) = label.cell() {
        cell.setUsesSingleLineMode(true);
        cell.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    }
    label
}
