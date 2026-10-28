use anyhow::{Context, Result};
use objc2::{
    define_class, msg_send, rc::Retained, sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSBezelStyle, NSButton, NSColor, NSFont, NSMenu, NSPopUpButton,
    NSPopover, NSPopoverBehavior, NSScrollView, NSTextField, NSView, NSViewController, NSWorkspace,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSRectEdge, NSSize, NSString};
use tao::event_loop::EventLoopProxy;
use tray_icon::menu::{ContextMenu, MenuEvent, MenuId, MenuItem, Submenu};

use crate::menubar_row::ROW_HEIGHT;

pub struct PopoverState {
    popover: Retained<NSPopover>,
    controller: Retained<NSViewController>,
    summary: Retained<NSTextField>,
    add: Retained<NSButton>,
    more: Retained<NSPopUpButton>,
    scroll: Retained<NSScrollView>,
    _actions: Submenu,
    proxy: EventLoopProxy<MenuEvent>,
    add_id: MenuId,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PopoverState]
    pub struct SessionPopover;
    unsafe impl NSObjectProtocol for SessionPopover {}
    impl SessionPopover {
        #[unsafe(method(addSession:))]
        fn add_session(&self, _sender: &NSButton) {
            let state = self.ivars();
            let _ = state.proxy.send_event(MenuEvent { id: state.add_id.clone() });
        }
    }
);

impl SessionPopover {
    pub fn new(
        proxy: EventLoopProxy<MenuEvent>,
        add_id: MenuId,
        quit_id: MenuId,
    ) -> Result<Retained<Self>> {
        let mtm = MainThreadMarker::new().context("session panel requires the main thread")?;
        let popover = NSPopover::new(mtm);
        popover.setBehavior(NSPopoverBehavior::Transient);
        let controller = NSViewController::new(mtm);
        popover.setContentViewController(Some(&controller));
        let summary = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        summary.setFont(Some(&NSFont::systemFontOfSize(12.)));
        summary.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let add = NSButton::new(mtm);
        add.setTitle(&NSString::from_str("New Session…"));
        add.setBezelStyle(NSBezelStyle::Push);
        let actions = Submenu::new("More", true);
        actions.append(&MenuItem::new("More", false, None))?;
        actions.append(&MenuItem::with_id(
            quit_id,
            "Quit SSMux && Stop Sessions",
            true,
            None,
        ))?;
        let more = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            rect(0., 0., 80., 28.),
            true,
        );
        more.setBezelStyle(NSBezelStyle::Push);
        more.setAccessibilityLabel(Some(&NSString::from_str("More SSMux Actions")));
        unsafe {
            more.setMenu(Some(&*actions.ns_menu().cast::<NSMenu>()));
        }
        let scroll = NSScrollView::new(mtm);
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        let this = Self::alloc(mtm).set_ivars(PopoverState {
            popover,
            controller,
            summary,
            add,
            more,
            scroll,
            _actions: actions,
            proxy,
            add_id,
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe {
            this.ivars().add.setTarget(Some(&this));
            this.ivars().add.setAction(Some(sel!(addSession:)));
            this.ivars().add.setNextKeyView(Some(&this.ivars().more));
        }
        Ok(this)
    }

    pub fn set_rows(&self, rows: &[&NSView]) {
        let state = self.ivars();
        let mtm = self.mtm();
        let previous_offset = state.scroll.documentView().map_or(0., |document| {
            let visible = state.scroll.documentVisibleRect();
            (document.bounds().size.height - visible.origin.y - visible.size.height).max(0.)
        });
        let list_height = rows.len().max(1) as f64 * ROW_HEIGHT;
        let visible_height = list_height.min(480.);
        let height = visible_height + 104.;
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., 420., height));
        let title = NSTextField::labelWithString(&NSString::from_str("SSMux"), mtm);
        title.setFont(Some(&NSFont::boldSystemFontOfSize(14.)));
        title.setFrame(rect(12., height - 28., 396., 20.));
        content.addSubview(&title);
        state.summary.setFrame(rect(12., height - 48., 396., 18.));
        content.addSubview(&state.summary);
        let document = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., 420., list_height));
        for (index, row) in rows.iter().enumerate() {
            row.setFrame(rect(
                0.,
                list_height - (index + 1) as f64 * ROW_HEIGHT,
                420.,
                ROW_HEIGHT,
            ));
            document.addSubview(row);
        }
        if rows.is_empty() {
            let empty = NSTextField::labelWithString(
                &NSString::from_str("No sessions yet. Add one below."),
                mtm,
            );
            empty.setFrame(rect(12., 20., 396., 20.));
            document.addSubview(&empty);
        }
        state.scroll.setFrame(rect(0., 48., 420., visible_height));
        state.scroll.setDocumentView(Some(&document));
        content.addSubview(&state.scroll);
        state.add.setFrame(rect(12., 10., 136., 28.));
        state.more.setFrame(rect(328., 10., 80., 28.));
        content.addSubview(&state.add);
        content.addSubview(&state.more);
        state.controller.setView(&content);
        state.popover.setContentSize(NSSize::new(420., height));
        document.scrollPoint(NSPoint::new(
            0.,
            scroll_origin(list_height, visible_height, previous_offset),
        ));
    }

    pub fn set_summary(&self, text: &str) {
        self.ivars()
            .summary
            .setStringValue(&NSString::from_str(text));
    }

    pub fn toggle(&self, anchor: &NSView) {
        let popover = &self.ivars().popover;
        if popover.isShown() {
            popover.close();
        } else {
            NSApplication::sharedApplication(self.mtm()).activate();
            popover.setAnimates(
                !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion(),
            );
            popover.showRelativeToRect_ofView_preferredEdge(
                anchor.bounds(),
                anchor,
                NSRectEdge::MinY,
            );
            if let Some(window) = self.ivars().controller.view().window() {
                window.setAutorecalculatesKeyViewLoop(true);
                window.recalculateKeyViewLoop();
                window.makeKeyWindow();
            }
        }
    }

    pub fn close(&self) {
        self.ivars().popover.close();
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn scroll_origin(document_height: f64, visible_height: f64, offset_from_top: f64) -> f64 {
    (document_height - visible_height - offset_from_top)
        .clamp(0., (document_height - visible_height).max(0.))
}

#[cfg(test)]
mod tests {
    use super::scroll_origin;

    #[test]
    fn list_updates_preserve_distance_from_top_and_clamp_after_deletions() {
        assert_eq!(scroll_origin(800., 480., 0.), 320.);
        assert_eq!(scroll_origin(800., 480., 160.), 160.);
        assert_eq!(scroll_origin(880., 480., 160.), 240.);
        assert_eq!(scroll_origin(560., 480., 160.), 0.);
        for rows in [0_usize, 1, 6, 8, 20] {
            let height = rows.max(1) as f64 * super::ROW_HEIGHT;
            let visible = height.min(480.);
            let origin = scroll_origin(height, visible, 160.);
            assert!(origin >= 0. && origin + visible <= height);
        }
    }
}
