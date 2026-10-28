use anyhow::{Context, Result};
use objc2::{
    define_class, msg_send, rc::Retained, sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSBezelStyle, NSButton, NSColor, NSFont, NSPopover,
    NSPopoverBehavior, NSScrollView, NSSearchField, NSTextField, NSView, NSViewController,
    NSWorkspace,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSRectEdge, NSSize, NSString};
use tao::event_loop::EventLoopProxy;
use tray_icon::menu::{MenuEvent, MenuId};

pub const WIDTH: f64 = 560.;
pub const ROW_HEIGHT: f64 = 116.;

pub struct PopoverState {
    popover: Retained<NSPopover>,
    controller: Retained<NSViewController>,
    content: Retained<NSView>,
    title: Retained<NSTextField>,
    summary: Retained<NSTextField>,
    notice: Retained<NSTextField>,
    search: Retained<NSSearchField>,
    scroll: Retained<NSScrollView>,
    add: Retained<NSButton>,
    setup: Retained<NSButton>,
    quit: Retained<NSButton>,
    proxy: EventLoopProxy<MenuEvent>,
    add_id: MenuId,
    setup_id: MenuId,
    search_id: MenuId,
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
        fn add_session(&self, _sender: &NSButton) { self.send(self.ivars().add_id.clone()); }
        #[unsafe(method(checkSetup:))]
        fn check_setup(&self, _sender: &NSButton) { self.send(self.ivars().setup_id.clone()); }
        #[unsafe(method(searchSessions:))]
        fn search_sessions(&self, _sender: &NSSearchField) { self.send(self.ivars().search_id.clone()); }
        #[unsafe(method(quitSessions:))]
        fn quit_sessions(&self, _sender: &NSButton) { self.send(self.ivars().quit_id.clone()); }
    }
);

impl SessionPopover {
    fn send(&self, id: MenuId) {
        let _ = self.ivars().proxy.send_event(MenuEvent { id });
    }

    pub fn new(
        proxy: EventLoopProxy<MenuEvent>,
        add_id: MenuId,
        setup_id: MenuId,
        search_id: MenuId,
        quit_id: MenuId,
    ) -> Result<Retained<Self>> {
        let mtm = MainThreadMarker::new().context("session panel requires the main thread")?;
        let popover = NSPopover::new(mtm);
        popover.setBehavior(NSPopoverBehavior::Transient);
        let controller = NSViewController::new(mtm);
        popover.setContentViewController(Some(&controller));
        let content = NSView::new(mtm);
        controller.setView(&content);
        let title = NSTextField::labelWithString(&NSString::from_str("SSMux"), mtm);
        title.setFont(Some(&NSFont::boldSystemFontOfSize(14.)));
        content.addSubview(&title);
        let summary = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        summary.setFont(Some(&NSFont::systemFontOfSize(12.)));
        summary.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&summary);
        let notice = NSTextField::wrappingLabelWithString(
            &NSString::from_str("Checking local AWS setup…"),
            mtm,
        );
        notice.setFont(Some(&NSFont::systemFontOfSize(12.)));
        content.addSubview(&notice);
        let search = NSSearchField::new(mtm);
        search.setPlaceholderString(Some(&NSString::from_str(
            "Search name, host, target, profile or region",
        )));
        search.setAccessibilityLabel(Some(&NSString::from_str("Search Sessions")));
        search.setSendsSearchStringImmediately(true);
        search.setSendsWholeSearchString(false);
        search.setMaximumRecents(0);
        content.addSubview(&search);
        let scroll = NSScrollView::new(mtm);
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        content.addSubview(&scroll);
        let button = |title: &str| {
            let button = NSButton::new(mtm);
            button.setTitle(&NSString::from_str(title));
            button.setBezelStyle(NSBezelStyle::Push);
            content.addSubview(&button);
            button
        };
        let add = button("New Session…");
        let setup = button("Check Setup");
        let quit = button("Quit & Stop Sessions");
        let this = Self::alloc(mtm).set_ivars(PopoverState {
            popover,
            controller,
            content,
            title,
            summary,
            notice,
            search,
            scroll,
            add,
            setup,
            quit,
            proxy,
            add_id,
            setup_id,
            search_id,
            quit_id,
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe {
            this.ivars().add.setTarget(Some(&this));
            this.ivars().add.setAction(Some(sel!(addSession:)));
            this.ivars().setup.setTarget(Some(&this));
            this.ivars().setup.setAction(Some(sel!(checkSetup:)));
            this.ivars().search.setTarget(Some(&this));
            this.ivars().search.setAction(Some(sel!(searchSessions:)));
            this.ivars().quit.setTarget(Some(&this));
            this.ivars().quit.setAction(Some(sel!(quitSessions:)));
        }
        Ok(this)
    }

    pub fn query(&self) -> String {
        self.ivars().search.stringValue().to_string()
    }

    pub fn focus_search(&self) {
        unsafe {
            self.ivars().search.selectText(None);
        }
    }

    pub fn set_rows(&self, rows: &[&NSView]) {
        let state = self.ivars();
        let mtm = self.mtm();
        let list_height = rows.len().max(1) as f64 * ROW_HEIGHT;
        let visible_height = list_height.min(4. * ROW_HEIGHT);
        let height = visible_height + 170.;
        state.content.setFrame(rect(0., 0., WIDTH, height));
        state
            .title
            .setFrame(rect(12., height - 28., WIDTH - 24., 20.));
        state
            .summary
            .setFrame(rect(12., height - 48., WIDTH - 24., 18.));
        state
            .search
            .setFrame(rect(12., height - 84., WIDTH - 24., 28.));
        state.notice.setFrame(rect(12., 42., WIDTH - 24., 36.));
        let document = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., WIDTH, list_height));
        for (index, row) in rows.iter().enumerate() {
            row.setFrame(rect(
                0.,
                list_height - (index + 1) as f64 * ROW_HEIGHT,
                WIDTH,
                ROW_HEIGHT,
            ));
            document.addSubview(row);
        }
        if rows.is_empty() {
            let text = if self.query().trim().is_empty() {
                "Create your first forwarding session below.\nChoose an existing AWS profile, then enter the target and ports."
            } else {
                "No matching sessions. Try another name, host or profile."
            };
            let empty = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
            empty.setFrame(rect(16., 32., WIDTH - 32., 52.));
            document.addSubview(&empty);
        }
        state.scroll.setFrame(rect(0., 82., WIDTH, visible_height));
        state.scroll.setDocumentView(Some(&document));
        document.scrollPoint(NSPoint::new(0., list_height - visible_height));
        state.add.setFrame(rect(12., 8., 136., 28.));
        state.setup.setFrame(rect(158., 8., 114., 28.));
        state.quit.setFrame(rect(WIDTH - 184., 8., 172., 28.));
        state.popover.setContentSize(NSSize::new(WIDTH, height));
    }

    pub fn set_summary(&self, text: &str) {
        self.ivars()
            .summary
            .setStringValue(&NSString::from_str(text));
    }

    pub fn set_notice(&self, text: &str) {
        self.ivars()
            .notice
            .setStringValue(&NSString::from_str(text));
        self.ivars()
            .notice
            .setToolTip(Some(&NSString::from_str(text)));
    }

    pub fn show(&self, anchor: &NSView) {
        if self.ivars().popover.isShown() {
            return;
        }
        NSApplication::sharedApplication(self.mtm()).activate();
        let popover = &self.ivars().popover;
        popover
            .setAnimates(!NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion());
        popover.showRelativeToRect_ofView_preferredEdge(anchor.bounds(), anchor, NSRectEdge::MinY);
        if let Some(window) = self.ivars().controller.view().window() {
            window.makeKeyWindow();
        }
    }

    pub fn toggle(&self, anchor: &NSView) {
        if self.ivars().popover.isShown() {
            self.close();
        } else {
            self.show(anchor);
        }
    }

    pub fn close(&self) {
        self.ivars().popover.close();
    }

    pub fn is_shown(&self) -> bool {
        self.ivars().popover.isShown()
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}
