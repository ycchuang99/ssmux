use std::cell::RefCell;

use anyhow::{Context, Result};
use objc2::{
    define_class, msg_send, rc::Retained, sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSAnimationContext, NSApplication, NSBezelStyle, NSBox, NSBoxType, NSButton,
    NSColor, NSControlSize, NSFont, NSImage, NSMenu, NSPopover, NSPopoverBehavior, NSScrollView,
    NSSearchField, NSTextAlignment, NSTextField, NSView, NSViewController, NSWorkspace,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSRectEdge, NSSize, NSString};
use tao::event_loop::EventLoopProxy;
use tray_icon::menu::{ContextMenu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

pub const WIDTH: f64 = 400.;
pub const ROW_HEIGHT: f64 = 72.;
const MAX_LIST_HEIGHT: f64 = 400.;

pub struct PopoverState {
    popover: Retained<NSPopover>,
    controller: Retained<NSViewController>,
    content: Retained<NSView>,
    title: Retained<NSTextField>,
    summary: Retained<NSTextField>,
    notice: Retained<NSTextField>,
    search: Retained<NSSearchField>,
    scroll: Retained<NSScrollView>,
    document: Retained<NSView>,
    empty: Retained<NSTextField>,
    rows: RefCell<Vec<(usize, f64)>>,
    add: Retained<NSButton>,
    setup: Retained<NSButton>,
    setup_status: Retained<NSTextField>,
    more: Retained<NSButton>,
    menu: Submenu,
    header_rule: Retained<NSBox>,
    footer_rule: Retained<NSBox>,
    proxy: EventLoopProxy<MenuEvent>,
    add_id: MenuId,
    setup_id: MenuId,
    search_id: MenuId,
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
        #[unsafe(method(showActions:))]
        fn show_actions(&self, sender: &NSButton) { show_action_menu(&self.ivars().menu, sender); }
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
        title.setFont(Some(&NSFont::boldSystemFontOfSize(15.)));
        content.addSubview(&title);
        let summary = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        summary.setFont(Some(&NSFont::systemFontOfSize(12.)));
        summary.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&summary);
        let notice = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        notice.setFont(Some(&NSFont::systemFontOfSize(12.)));
        notice.setHidden(true);
        content.addSubview(&notice);
        let search = NSSearchField::new(mtm);
        search.setControlSize(NSControlSize::Small);
        search.setFont(Some(&NSFont::systemFontOfSize(12.)));
        search.setPlaceholderString(Some(&NSString::from_str("Search sessions")));
        search.setToolTip(Some(&NSString::from_str(
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
        let document = NSView::new(mtm);
        let empty = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        empty.setFont(Some(&NSFont::systemFontOfSize(12.)));
        empty.setTextColor(Some(&NSColor::secondaryLabelColor()));
        empty.setFrame(rect(14., 10., WIDTH - 28., 52.));
        scroll.setDocumentView(Some(&document));
        content.addSubview(&scroll);
        let add = NSButton::new(mtm);
        add.setTitle(&NSString::from_str("＋ New Session…"));
        add.setBezelStyle(NSBezelStyle::Push);
        add.setControlSize(NSControlSize::Small);
        add.setFont(Some(&NSFont::systemFontOfSize(12.)));
        add.setBordered(false);
        add.setContentTintColor(Some(&NSColor::controlAccentColor()));
        content.addSubview(&add);
        let setup_status = NSTextField::labelWithString(&NSString::from_str("Local setup"), mtm);
        setup_status.setFont(Some(&NSFont::systemFontOfSize(12.)));
        setup_status.setAlignment(NSTextAlignment::Right);
        setup_status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&setup_status);
        let setup = NSButton::new(mtm);
        setup.setBezelStyle(NSBezelStyle::Push);
        setup.setControlSize(NSControlSize::Small);
        setup.setBordered(false);
        setup.setTitle(&NSString::from_str(""));
        setup.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str("info.circle"),
                Some(&NSString::from_str("Check Local AWS Setup")),
            )
            .as_deref(),
        );
        setup.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        setup.setAccessibilityLabel(Some(&NSString::from_str("Check Local AWS Setup")));
        setup.setToolTip(Some(&NSString::from_str(
            "Check local AWS tools and profiles. AWS access is checked when connecting.",
        )));
        content.addSubview(&setup);
        let menu = Submenu::new("SSMux", true);
        menu.append(&MenuItem::with_id(
            setup_id.clone(),
            "Check AWS Setup…",
            true,
            None,
        ))?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&MenuItem::with_id(
            quit_id,
            "Quit SSMux && Stop Sessions",
            true,
            None,
        ))?;
        let more = NSButton::new(mtm);
        more.setTitle(&NSString::from_str("…"));
        more.setBezelStyle(NSBezelStyle::Push);
        more.setControlSize(NSControlSize::Small);
        more.setFont(Some(&NSFont::systemFontOfSize(12.)));
        more.setAccessibilityLabel(Some(&NSString::from_str("SSMux Actions")));
        more.setToolTip(Some(&NSString::from_str("Check setup or quit SSMux")));
        content.addSubview(&more);
        let rule = || {
            let line = NSBox::new(mtm);
            line.setBoxType(NSBoxType::Separator);
            content.addSubview(&line);
            line
        };
        let header_rule = rule();
        let footer_rule = rule();
        let this = Self::alloc(mtm).set_ivars(PopoverState {
            popover,
            controller,
            content,
            title,
            summary,
            notice,
            search,
            scroll,
            document,
            empty,
            rows: RefCell::new(Vec::new()),
            add,
            setup,
            setup_status,
            more,
            menu,
            header_rule,
            footer_rule,
            proxy,
            add_id,
            setup_id,
            search_id,
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe {
            this.ivars().add.setTarget(Some(&this));
            this.ivars().add.setAction(Some(sel!(addSession:)));
            this.ivars().setup.setTarget(Some(&this));
            this.ivars().setup.setAction(Some(sel!(checkSetup:)));
            this.ivars().search.setTarget(Some(&this));
            this.ivars().search.setAction(Some(sel!(searchSessions:)));
            this.ivars().more.setTarget(Some(&this));
            this.ivars().more.setAction(Some(sel!(showActions:)));
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
        let previous = state.rows.borrow();
        let next: Vec<_> = rows
            .iter()
            .map(|row| (*row as *const NSView as usize, row.frame().size.height))
            .collect();
        let empty_text = if self.query().trim().is_empty() {
            "Create your first forwarding session below.\nChoose an AWS profile, then enter the target and ports."
        } else {
            "No matching sessions. Try another name, host or profile."
        };
        if *previous == next
            && (!rows.is_empty() || state.empty.stringValue().to_string() == empty_text)
        {
            return;
        }
        let same_rows = previous.len() == next.len()
            && previous.iter().zip(&next).all(|(old, new)| old.0 == new.0);
        let old_height = state.document.frame().size.height;
        let visible = state.scroll.documentVisibleRect();
        let top = (old_height - visible.origin.y - visible.size.height).max(0.);
        let top = if same_rows {
            anchored_offset(&previous, &next, top)
        } else {
            0.
        };
        drop(previous);
        if !same_rows || rows.is_empty() {
            for child in state.document.subviews() {
                child.removeFromSuperview();
            }
        }
        let list_height = next
            .iter()
            .map(|(_, height)| height)
            .sum::<f64>()
            .max(ROW_HEIGHT);
        set_frame(&state.document, rect(0., 0., WIDTH, list_height));
        let mut y = list_height;
        for row in rows {
            let height = row.frame().size.height;
            y -= height;
            set_frame(row, rect(0., y, WIDTH, height));
            if !same_rows {
                state.document.addSubview(row);
            }
        }
        if rows.is_empty() {
            state.empty.setStringValue(&NSString::from_str(empty_text));
            state.document.addSubview(&state.empty);
        }
        state.rows.replace(next);
        self.layout(top);
        if !same_rows {
            if let Some(window) = state.content.window() {
                window.recalculateKeyViewLoop();
            }
        }
    }

    fn layout(&self, top: f64) {
        let state = self.ivars();
        let list_height = state.document.frame().size.height;
        let visible_height = list_height.min(MAX_LIST_HEIGHT);
        let notice_height = if state.notice.stringValue().is_empty() {
            0.
        } else {
            state
                .notice
                .cell()
                .map(|cell| {
                    cell.cellSizeForBounds(rect(0., 0., WIDTH - 32., 1000.))
                        .height
                        .ceil()
                })
                .unwrap_or(32.)
                .max(18.)
                + 12.
        };
        let footer_height = 40. + notice_height;
        let height = visible_height + 94. + footer_height;
        set_frame(&state.title, rect(14., height - 30., WIDTH - 68., 20.));
        set_frame(&state.summary, rect(14., height - 49., WIDTH - 28., 18.));
        set_frame(&state.more, rect(WIDTH - 38., height - 34., 24., 24.));
        set_frame(&state.search, rect(14., height - 82., WIDTH - 28., 24.));
        set_frame(&state.header_rule, rect(12., height - 94., WIDTH - 24., 1.));
        set_frame(
            &state.scroll,
            rect(0., footer_height, WIDTH, visible_height),
        );
        set_frame(
            &state.footer_rule,
            rect(12., footer_height, WIDTH - 24., 1.),
        );
        set_frame(&state.add, rect(8., 7., 132., 26.));
        set_frame(&state.setup_status, rect(WIDTH - 170., 11., 132., 18.));
        set_frame(&state.setup, rect(WIDTH - 36., 7., 24., 26.));
        state.notice.setHidden(notice_height == 0.);
        set_frame(
            &state.notice,
            rect(14., 40., WIDTH - 28., (notice_height - 8.).max(0.)),
        );
        let size = NSSize::new(WIDTH, height);
        if state.popover.contentSize() != size {
            let animate = state.popover.isShown()
                && !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
            state.popover.setAnimates(animate);
            NSAnimationContext::beginGrouping();
            NSAnimationContext::currentContext().setDuration(if animate { 0.18 } else { 0. });
            // Resize through NSPopover once; resizing its content view first causes a second transition.
            state.popover.setContentSize(size);
            NSAnimationContext::endGrouping();
        }
        let origin = NSPoint::new(0., (list_height - visible_height - top).max(0.));
        if state.scroll.documentVisibleRect().origin != origin {
            state.document.scrollPoint(origin);
        }
    }

    pub fn set_summary(&self, text: &str) {
        if self.ivars().summary.stringValue().to_string() == text {
            return;
        }
        self.ivars()
            .summary
            .setStringValue(&NSString::from_str(text));
        self.ivars()
            .summary
            .setToolTip(Some(&NSString::from_str(text)));
    }

    pub fn set_setup_status(&self, text: &str) {
        let state = self.ivars();
        let description = NSString::from_str(&format!(
            "{text}. Local tools and profiles only. AWS access and tunnel readiness are checked when connecting."
        ));
        state
            .setup_status
            .setStringValue(&NSString::from_str("Local setup"));
        state.setup_status.setToolTip(Some(&description));
        state.setup.setToolTip(Some(&description));
        state
            .setup
            .setAccessibilityLabel(Some(&NSString::from_str(&format!(
                "Check Local AWS Setup. {text}"
            ))));
    }

    pub fn set_feedback(&self, text: &str) {
        let status = &self.ivars().setup_status;
        status.setStringValue(&NSString::from_str(text));
        status.setToolTip(Some(&NSString::from_str(text)));
    }

    pub fn set_notice(&self, text: &str) {
        let state = self.ivars();
        if state.notice.stringValue().to_string() == text {
            return;
        }
        let visible = state.scroll.documentVisibleRect();
        let top =
            (state.document.frame().size.height - visible.origin.y - visible.size.height).max(0.);
        state.notice.setStringValue(&NSString::from_str(text));
        state.notice.setToolTip(Some(&NSString::from_str(text)));
        self.layout(top);
    }

    pub fn show(&self, anchor: &NSView) {
        if self.ivars().popover.isShown() {
            return;
        }
        self.ivars()
            .setup_status
            .setStringValue(&NSString::from_str("Local setup"));
        self.ivars()
            .setup_status
            .setToolTip(self.ivars().setup.toolTip().as_deref());
        NSApplication::sharedApplication(self.mtm()).activate();
        let popover = &self.ivars().popover;
        popover
            .setAnimates(!NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion());
        popover.showRelativeToRect_ofView_preferredEdge(anchor.bounds(), anchor, NSRectEdge::MinY);
        if let Some(window) = self.ivars().controller.view().window() {
            window.recalculateKeyViewLoop();
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

pub(crate) fn show_action_menu(actions: &Submenu, button: &NSButton) {
    // Open the menu on button activation so the press is handled only once.
    let menu = unsafe { &*actions.ns_menu().cast::<NSMenu>() };
    menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0., 0.), Some(button));
}

fn anchored_offset(previous: &[(usize, f64)], next: &[(usize, f64)], top: f64) -> f64 {
    let mut old_y = 0.;
    let mut new_y = 0.;
    for ((_, old_height), (_, new_height)) in previous.iter().zip(next) {
        if top < old_y + old_height {
            return new_y + (top - old_y).min(*new_height);
        }
        old_y += old_height;
        new_y += new_height;
    }
    new_y
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn set_frame(view: &NSView, frame: NSRect) {
    if view.frame() != frame {
        view.setFrame(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanding_a_row_above_the_viewport_preserves_the_visible_row() {
        let before = [(1, 86.), (2, 86.), (3, 86.)];
        let after = [(1, 240.), (2, 86.), (3, 86.)];
        assert_eq!(anchored_offset(&before, &after, 100.), 254.);
        assert_eq!(anchored_offset(&before, &after, 0.), 0.);
    }

    #[test]
    fn collapsing_the_visible_row_keeps_scroll_within_the_remaining_content() {
        let before = [(1, 240.), (2, 86.)];
        let after = [(1, 86.), (2, 86.)];
        assert_eq!(anchored_offset(&before, &after, 200.), 86.);
        assert_eq!(anchored_offset(&before, &after, 250.), 96.);
    }
}
