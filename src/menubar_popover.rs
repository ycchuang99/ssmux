use anyhow::{Context, Result};
use objc2::{
    define_class, msg_send, rc::Retained, sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSApplication, NSBezelStyle, NSButton, NSColor, NSFont, NSPopover, NSPopoverBehavior,
    NSScrollView, NSTextField, NSView, NSViewController, NSWorkspace,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSRectEdge, NSSize, NSString};
use tao::event_loop::EventLoopProxy;
use tray_icon::menu::{MenuEvent, MenuId};

pub struct PopoverState {
    popover: Retained<NSPopover>,
    controller: Retained<NSViewController>,
    summary: Retained<NSTextField>,
    add: Retained<NSButton>,
    quit: Retained<NSButton>,
    proxy: EventLoopProxy<MenuEvent>,
    add_id: MenuId,
    quit_id: MenuId,
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
        #[unsafe(method(quitSessions:))]
        fn quit_sessions(&self, _sender: &NSButton) {
            let state = self.ivars();
            let _ = state.proxy.send_event(MenuEvent { id: state.quit_id.clone() });
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
        let quit = NSButton::new(mtm);
        quit.setTitle(&NSString::from_str("Quit & Stop Sessions"));
        quit.setBezelStyle(NSBezelStyle::Push);
        let this = Self::alloc(mtm).set_ivars(PopoverState {
            popover,
            controller,
            summary,
            add,
            quit,
            proxy,
            add_id,
            quit_id,
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe {
            this.ivars().add.setTarget(Some(&this));
            this.ivars().add.setAction(Some(sel!(addSession:)));
            this.ivars().quit.setTarget(Some(&this));
            this.ivars().quit.setAction(Some(sel!(quitSessions:)));
        }
        Ok(this)
    }

    pub fn set_rows(&self, rows: &[&NSView]) {
        let state = self.ivars();
        let mtm = self.mtm();
        let list_height = (rows.len().max(1) * 60) as f64;
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
            row.setFrame(rect(0., list_height - (index + 1) as f64 * 60., 420., 60.));
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
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            rect(0., 48., 420., visible_height),
        );
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDocumentView(Some(&document));
        content.addSubview(&scroll);
        document.scrollPoint(NSPoint::new(0., list_height - visible_height));
        state.add.setFrame(rect(12., 10., 136., 28.));
        state.quit.setFrame(rect(236., 10., 172., 28.));
        content.addSubview(&state.add);
        content.addSubview(&state.quit);
        state.controller.setView(&content);
        state.popover.setContentSize(NSSize::new(420., height));
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
