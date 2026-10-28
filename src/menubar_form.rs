use std::{
    cell::{Cell, RefCell},
    fmt,
};

use anyhow::{Context, Result};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityAnnouncementKey,
    NSAccessibilityAnnouncementRequestedNotification, NSAccessibilityPostNotificationWithUserInfo,
    NSAlert, NSAlertSecondButtonReturn, NSAlertStyle, NSApplication, NSBackingStoreType,
    NSBezelStyle, NSButton, NSButtonType, NSColor, NSControlTextEditingDelegate, NSEventMask,
    NSFont, NSModalPanelRunLoopMode, NSModalResponseContinue, NSPanel, NSPopUpButton, NSScrollView,
    NSTextDelegate, NSTextField, NSTextFieldBezelStyle, NSTextFieldDelegate, NSTextView,
    NSTextViewDelegate, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDate, NSDictionary, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString,
};

use crate::config::{self, Config, ConnectionConfig};

const INSTANCE_DOCUMENT: &str = "AWS-StartPortForwardingSession";
const REMOTE_DOCUMENT: &str = "AWS-StartPortForwardingSessionToRemoteHost";
const LABELS: [&str; 8] = [
    "Session Name",
    "SSM Target · Forwarding Node",
    "AWS Region · Blank Uses AWS Default",
    "AWS Profile · Blank Uses AWS Default",
    "SSM Document",
    "Remote Host · Destination Reached by the Node",
    "Remote Port",
    "Local Port",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputTarget {
    Field(usize),
    Parameters,
}

#[derive(Debug)]
struct FieldError {
    target: InputTarget,
    message: String,
}

impl fmt::Display for FieldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for FieldError {}

fn field_error(target: InputTarget, message: impl Into<String>) -> anyhow::Error {
    FieldError {
        target,
        message: message.into(),
    }
    .into()
}

struct EditorState {
    panel: Retained<NSPanel>,
    scroll: Retained<NSScrollView>,
    document: Retained<FlippedView>,
    fields: Vec<Retained<NSTextField>>,
    labels: Vec<Retained<NSTextField>>,
    mode_label: Retained<NSTextField>,
    mode: Retained<NSPopUpButton>,
    advanced: Retained<NSButton>,
    advanced_label: Retained<NSTextField>,
    parameters_label: Retained<NSTextField>,
    parameters_scroll: Retained<NSScrollView>,
    parameters: Retained<NSTextView>,
    route: Retained<NSTextField>,
    error: Retained<NSTextField>,
    field_error: Retained<NSTextField>,
    validation_target: Cell<Option<InputTarget>>,
    persistence_error: RefCell<String>,
    previous_mode: Cell<isize>,
    custom_document: RefCell<String>,
    cancel: Retained<NSButton>,
    save: Retained<NSButton>,
    config: Config,
    existing: Option<ConnectionConfig>,
    attempted_save: Cell<bool>,
    result: RefCell<Option<ConnectionConfig>>,
}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    struct FlippedView;
    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { true }
    }
);

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = EditorState]
    struct SessionEditor;

    unsafe impl NSObjectProtocol for SessionEditor {}
    unsafe impl NSTextFieldDelegate for SessionEditor {}
    unsafe impl NSControlTextEditingDelegate for SessionEditor {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _notification: &NSNotification) {
            self.refresh_validation();
        }
    }
    unsafe impl NSTextDelegate for SessionEditor {
        #[unsafe(method(textDidChange:))]
        fn parameters_changed(&self, _notification: &NSNotification) { self.refresh_validation(); }
    }
    unsafe impl NSTextViewDelegate for SessionEditor {
        #[unsafe(method(textView:doCommandBySelector:))]
        unsafe fn text_command(&self, view: &NSTextView, command: Sel) -> bool {
            if command == sel!(insertTab:) {
                self.ivars().panel.selectNextKeyView(Some(view)); true
            } else if command == sel!(insertBacktab:) {
                self.ivars().panel.selectPreviousKeyView(Some(view)); true
            } else { false }
        }
    }
    unsafe impl NSWindowDelegate for SessionEditor {
        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _window: &NSWindow) -> bool {
            NSApplication::sharedApplication(self.mtm()).stopModalWithCode(0);
            false
        }
        #[unsafe(method(windowDidResize:))]
        fn did_resize(&self, _notification: &NSNotification) { self.layout(); }
    }
    impl SessionEditor {
        #[unsafe(method(saveSession:))]
        fn save_session(&self, _sender: &NSObject) {
            self.ivars().attempted_save.set(true);
            match self.connection() {
                Ok(connection) => {
                    self.ivars().result.replace(Some(connection));
                    NSApplication::sharedApplication(self.mtm()).stopModalWithCode(1);
                }
                Err(error) => self.display_validation(Some(&error), true),
            }
        }
        #[unsafe(method(cancelSession:))]
        fn cancel_session(&self, _sender: &NSObject) {
            NSApplication::sharedApplication(self.mtm()).stopModalWithCode(0);
        }
        #[unsafe(method(changeMode:))]
        fn change_mode(&self, _sender: &NSObject) {
            let state = self.ivars();
            if state.previous_mode.get() == 2 {
                state.custom_document.replace(state.fields[4].stringValue().to_string());
            }
            let mode = state.mode.indexOfSelectedItem();
            let document = match mode {
                0 => REMOTE_DOCUMENT.to_owned(),
                1 => INSTANCE_DOCUMENT.to_owned(),
                _ => state.custom_document.borrow().clone(),
            };
            state.fields[4].setStringValue(&NSString::from_str(&document));
            state.previous_mode.set(mode);
            if mode == 2 { state.advanced.setState(1); }
            self.refresh_validation();
            self.layout();
        }
        #[unsafe(method(toggleAdvanced:))]
        fn toggle_advanced(&self, _sender: &NSObject) { self.layout(); }
    }
);

impl SessionEditor {
    fn connection(&self) -> Result<ConnectionConfig> {
        let state = self.ivars();
        let values = state
            .fields
            .iter()
            .map(|field| field.stringValue().to_string())
            .collect::<Vec<_>>();
        connection_from_fields(
            &state.config,
            state.existing.as_ref(),
            &values,
            &state.parameters.string().to_string(),
        )
    }

    fn announce_error(&self, message: &str) {
        let message = NSString::from_str(message);
        unsafe {
            let info = NSDictionary::from_slices(
                &[NSAccessibilityAnnouncementKey],
                &[&*message as &AnyObject],
            );
            NSAccessibilityPostNotificationWithUserInfo(
                &self.ivars().panel,
                NSAccessibilityAnnouncementRequestedNotification,
                Some(&info),
            );
        }
    }

    fn display_validation(&self, error: Option<&anyhow::Error>, focus: bool) {
        let state = self.ivars();
        let target = error
            .and_then(|error| error.downcast_ref::<FieldError>())
            .map(|error| error.target);
        let message = error.map(|error| format!("{error:#}")).unwrap_or_default();
        let previous_target = state.validation_target.replace(target);
        let custom = state.mode.indexOfSelectedItem() == 2;
        if matches!(
            target,
            Some(InputTarget::Field(4) | InputTarget::Parameters)
        ) || (custom && matches!(target, Some(InputTarget::Field(5..=7))))
        {
            state.advanced.setState(1);
        }
        state
            .field_error
            .setStringValue(&NSString::from_str(if target.is_some() {
                &message
            } else {
                ""
            }));
        let persistence = state.persistence_error.borrow();
        let footer = if target.is_none() && error.is_some() {
            &message
        } else {
            &persistence
        };
        state.error.setStringValue(&NSString::from_str(footer));
        state.error.setToolTip(Some(&NSString::from_str(footer)));
        for (index, field) in state.fields.iter().enumerate() {
            let invalid = target == Some(InputTarget::Field(index));
            let color = if invalid {
                NSColor::systemRedColor()
            } else {
                NSColor::labelColor()
            };
            state.labels[index].setTextColor(Some(&color));
            field.setAccessibilityHelp(invalid.then(|| NSString::from_str(&message)).as_deref());
        }
        state.parameters.setAccessibilityHelp(
            (target == Some(InputTarget::Parameters))
                .then(|| NSString::from_str(&message))
                .as_deref(),
        );
        if previous_target != target || focus {
            self.layout();
        }
        if focus {
            match target {
                Some(InputTarget::Field(index)) => {
                    let top = state.fields[index].frame().origin.y - 23.;
                    state.document.scrollRectToVisible(rect(
                        8.,
                        top,
                        state.document.frame().size.width - 16.,
                        state.field_error.frame().origin.y + 44. - top,
                    ));
                    state.panel.makeFirstResponder(Some(&state.fields[index]));
                }
                Some(InputTarget::Parameters) => {
                    let top = state.parameters_label.frame().origin.y;
                    state.document.scrollRectToVisible(rect(
                        8.,
                        top,
                        state.document.frame().size.width - 16.,
                        state.field_error.frame().origin.y + 44. - top,
                    ));
                    state.panel.makeFirstResponder(Some(&state.parameters));
                }
                None => {}
            }
            self.announce_error(&message);
        }
    }

    fn refresh_validation(&self) {
        let state = self.ivars();
        let route = if state.mode.indexOfSelectedItem() == 2 {
            "Custom document · Parameters are passed as entered".to_owned()
        } else {
            let local = state.fields[7].stringValue();
            let remote = state.fields[6].stringValue();
            let destination = state.fields[if state.mode.indexOfSelectedItem() == 1 {
                1
            } else {
                5
            }]
            .stringValue();
            format!("localhost:{local}  →  {destination}:{remote}")
        };
        state.route.setStringValue(&NSString::from_str(&route));
        state.route.setToolTip(Some(&NSString::from_str(&route)));
        if state.attempted_save.get() {
            self.display_validation(self.connection().err().as_ref(), false);
        }
    }

    fn layout(&self) {
        let state = self.ivars();
        let content = state.panel.contentView().expect("panel content view");
        let size = content.bounds().size;
        let compact = size.width < 600.;
        let custom = state.mode.indexOfSelectedItem() == 2;
        let advanced = state.advanced.state() == 1;
        state
            .route
            .setFrame(rect(24., size.height - 85., size.width - 48., 22.));
        state
            .scroll
            .setFrame(rect(16., 108., size.width - 32., size.height - 208.));
        state.error.setFrame(rect(24., 56., size.width - 48., 44.));
        state
            .cancel
            .setFrame(rect(size.width - 226., 16., 92., 32.));
        state.save.setFrame(rect(size.width - 124., 16., 100., 32.));
        let width = state.scroll.contentSize().width - 16.;
        for (index, (field, label)) in state.fields.iter().zip(&state.labels).enumerate() {
            let visible = match index {
                4 => custom || advanced,
                5 => state.mode.indexOfSelectedItem() != 1 && (!custom || advanced),
                6 | 7 => !custom || advanced,
                _ => true,
            };
            field.setHidden(!visible);
            label.setHidden(!visible);
        }
        state.parameters_label.setHidden(!custom && !advanced);
        state.parameters_scroll.setHidden(!custom && !advanced);
        state
            .field_error
            .setHidden(state.validation_target.get().is_none());
        let place = |index: usize, x: f64, y: f64, w: f64| {
            state.labels[index].setHidden(false);
            state.fields[index].setHidden(false);
            state.labels[index].setFrame(rect(x, y, w, 20.));
            state.fields[index].setFrame(rect(x, y + 23., w, 32.));
        };
        let advance = |y: f64, indices: &[usize]| {
            if indices
                .iter()
                .any(|index| state.validation_target.get() == Some(InputTarget::Field(*index)))
            {
                state.field_error.setFrame(rect(8., y + 58., width, 44.));
                y + 112.
            } else {
                y + 68.
            }
        };
        let forwarding = |mut y: f64| {
            if state.mode.indexOfSelectedItem() != 1 {
                place(5, 8., y, width);
                y = advance(y, &[5]);
            }
            if compact {
                place(6, 8., y, width);
                y = advance(y, &[6]);
                place(7, 8., y, width);
                advance(y, &[7])
            } else {
                let half = (width - 16.) / 2.;
                place(6, 8., y, half);
                place(7, 24. + half, y, half);
                advance(y, &[6, 7])
            }
        };
        let document_fields = |mut y: f64| {
            place(4, 8., y, width);
            y = advance(y, &[4]);
            state.parameters_label.setHidden(false);
            state.parameters_scroll.setHidden(false);
            state.parameters_label.setFrame(rect(8., y, width, 20.));
            state
                .parameters_scroll
                .setFrame(rect(8., y + 23., width, 112.));
            state
                .parameters
                .setFrameSize(NSSize::new(width - 20., 112.));
            y += 143.;
            if state.validation_target.get() == Some(InputTarget::Parameters) {
                state.field_error.setFrame(rect(8., y, width, 44.));
                y += 48.;
            }
            y + 8.
        };
        let mut y = 8.;
        place(0, 8., y, width);
        y = advance(y, &[0]);
        state.mode_label.setFrame(rect(8., y, width, 20.));
        state.mode.setFrame(rect(5., y + 23., width + 6., 32.));
        y += 68.;
        y = if custom {
            document_fields(y)
        } else {
            forwarding(y)
        };
        place(1, 8., y, width);
        y = advance(y, &[1]);
        if compact {
            place(2, 8., y, width);
            y = advance(y, &[2]);
            place(3, 8., y, width);
            y = advance(y, &[3]);
        } else {
            let half = (width - 16.) / 2.;
            place(2, 8., y, half);
            place(3, 24. + half, y, half);
            y = advance(y, &[2, 3]);
        }
        let advanced_title = if custom {
            "Optional Forwarding Parameters"
        } else {
            "Advanced Settings"
        };
        state
            .advanced_label
            .setStringValue(&NSString::from_str(advanced_title));
        state
            .advanced
            .setAccessibilityLabel(Some(&NSString::from_str(advanced_title)));
        state.advanced.setFrame(rect(8., y, 28., 28.));
        state
            .advanced_label
            .setFrame(rect(42., y + 4., width - 34., 20.));
        y += 40.;
        if advanced {
            y = if custom {
                forwarding(y)
            } else {
                document_fields(y)
            };
        }
        state.fields[4].setEnabled(custom);
        state
            .document
            .setFrameSize(NSSize::new(state.scroll.contentSize().width, y));

        let mut order: Vec<(&NSView, NSPoint)> = state
            .fields
            .iter()
            .filter(|field| !field.isHidden() && field.isEnabled())
            .map(|field| (&**field as &NSView, field.frame().origin))
            .collect();
        order.push((&state.mode, state.mode.frame().origin));
        order.push((&state.advanced, state.advanced.frame().origin));
        if !state.parameters_scroll.isHidden() {
            order.push((&state.parameters, state.parameters_scroll.frame().origin));
        }
        order.sort_by(|a, b| a.1.y.total_cmp(&b.1.y).then(a.1.x.total_cmp(&b.1.x)));
        let mut views = order.iter().map(|(view, _)| *view).collect::<Vec<_>>();
        views.extend([&*state.cancel as &NSView, &*state.save as &NSView]);
        unsafe {
            for pair in views.windows(2) {
                pair[0].setNextKeyView(Some(pair[1]));
            }
            state.save.setNextKeyView(views.first().copied());
        }
    }
}

pub fn edit_connection(
    config: &Config,
    existing: Option<&ConnectionConfig>,
    mut on_save: impl FnMut(ConnectionConfig) -> Result<()>,
    mut on_tick: impl FnMut(),
) -> Result<bool> {
    let mtm = MainThreadMarker::new().context("session form requires the main thread")?;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect(0., 0., 640., 700.),
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe {
        panel.setReleasedWhenClosed(false);
    }
    panel.setHidesOnDeactivate(false);
    panel.setContentMinSize(NSSize::new(520., 520.));
    let title = if existing.is_some() {
        "Edit Session"
    } else {
        "New Session"
    };
    panel.setTitle(&NSString::from_str(title));
    let content = panel.contentView().context("creating editor content")?;
    let heading = label(title, 20., mtm);
    heading.setFont(Some(&NSFont::boldSystemFontOfSize(20.)));
    heading.setFrame(rect(24., 648., 400., 28.));
    heading.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewMinYMargin);
    content.addSubview(&heading);
    let route = label("localhost → destination", 13., mtm);
    route.setFont(NSFont::userFixedPitchFontOfSize(13.).as_deref());
    content.addSubview(&route);
    let scroll = NSScrollView::new(mtm);
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    let document: Retained<FlippedView> = unsafe { msg_send![FlippedView::alloc(mtm), init] };
    scroll.setDocumentView(Some(&document));
    content.addSubview(&scroll);
    let (values, parameter_text) = editor_values(existing);
    let placeholders = [
        "e.g. Production Database",
        "i-0123456789abcdef0",
        "AWS Default",
        "AWS Default",
        "Custom SSM document name",
        "database.example.com",
        "3306",
        "13306",
    ];
    let mut fields = Vec::new();
    let mut labels = Vec::new();
    for (index, title) in LABELS.iter().enumerate() {
        let caption = label(title, 13., mtm);
        let field = NSTextField::textFieldWithString(&NSString::from_str(&values[index]), mtm);
        field.setBezelStyle(NSTextFieldBezelStyle::RoundedBezel);
        field.setFont(Some(&NSFont::systemFontOfSize(13.)));
        field.setPlaceholderString(Some(&NSString::from_str(placeholders[index])));
        field.setAccessibilityLabel(Some(&NSString::from_str(title)));
        document.addSubview(&caption);
        document.addSubview(&field);
        labels.push(caption);
        fields.push(field);
    }
    let mode_label = label("Session Type", 13., mtm);
    let mode = NSPopUpButton::new(mtm);
    for title in ["Remote Host", "SSM Instance", "Custom Document"] {
        mode.addItemWithTitle(&NSString::from_str(title));
    }
    mode.selectItemAtIndex(match values[4].as_str() {
        REMOTE_DOCUMENT => 0,
        INSTANCE_DOCUMENT => 1,
        _ => 2,
    });
    mode.setAccessibilityLabel(Some(&NSString::from_str("Session Type")));
    document.addSubview(&mode_label);
    document.addSubview(&mode);
    let advanced = NSButton::new(mtm);
    advanced.setButtonType(NSButtonType::PushOnPushOff);
    advanced.setBezelStyle(NSBezelStyle::Disclosure);
    advanced.setTitle(&NSString::from_str(""));
    advanced.setState(isize::from(
        !parameter_text.is_empty() || mode.indexOfSelectedItem() == 2,
    ));
    document.addSubview(&advanced);
    let advanced_label = label("Advanced Settings", 13., mtm);
    document.addSubview(&advanced_label);
    let parameters_label = label("Additional Parameters · One key=value per line", 12., mtm);
    document.addSubview(&parameters_label);
    let parameters_scroll = NSScrollView::new(mtm);
    parameters_scroll.setHasVerticalScroller(true);
    parameters_scroll.setAutohidesScrollers(true);
    let parameters = NSTextView::new(mtm);
    parameters.setString(&NSString::from_str(&parameter_text));
    parameters.setRichText(false);
    parameters.setAutomaticQuoteSubstitutionEnabled(false);
    parameters.setAutomaticDashSubstitutionEnabled(false);
    parameters.setTextContainerInset(NSSize::new(8., 8.));
    parameters.setFont(NSFont::userFixedPitchFontOfSize(13.).as_deref());
    parameters.setTextColor(Some(&NSColor::textColor()));
    parameters.setBackgroundColor(&NSColor::textBackgroundColor());
    parameters.setAccessibilityLabel(Some(&NSString::from_str("Additional Parameters")));
    parameters_scroll.setDocumentView(Some(&parameters));
    document.addSubview(&parameters_scroll);
    let error = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
    error.setFont(Some(&NSFont::systemFontOfSize(12.)));
    error.setTextColor(Some(&NSColor::labelColor()));
    error.setAccessibilityLabel(Some(&NSString::from_str("Validation Error")));
    content.addSubview(&error);
    let field_error = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
    field_error.setFont(Some(&NSFont::systemFontOfSize(12.)));
    field_error.setTextColor(Some(&NSColor::labelColor()));
    document.addSubview(&field_error);
    let cancel = NSButton::new(mtm);
    cancel.setTitle(&NSString::from_str("Cancel"));
    cancel.setBezelStyle(NSBezelStyle::Push);
    cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));
    let save = NSButton::new(mtm);
    save.setTitle(&NSString::from_str("Save Session"));
    save.setBezelStyle(NSBezelStyle::Push);
    save.setKeyEquivalent(&NSString::from_str("\r"));
    content.addSubview(&cancel);
    content.addSubview(&save);
    let editor = SessionEditor::alloc(mtm).set_ivars(EditorState {
        panel: panel.clone(),
        scroll,
        document,
        fields,
        labels,
        mode_label,
        previous_mode: Cell::new(mode.indexOfSelectedItem()),
        custom_document: RefCell::new(if mode.indexOfSelectedItem() == 2 {
            values[4].clone()
        } else {
            String::new()
        }),
        mode,
        advanced,
        advanced_label,
        parameters_label,
        parameters_scroll,
        parameters,
        route,
        error,
        field_error,
        validation_target: Cell::new(None),
        persistence_error: RefCell::new(String::new()),
        cancel,
        save,
        config: config.clone(),
        existing: existing.cloned(),
        attempted_save: Cell::new(false),
        result: RefCell::new(None),
    });
    let editor: Retained<SessionEditor> = unsafe { msg_send![super(editor), init] };
    let state = editor.ivars();
    unsafe {
        state.save.setTarget(Some(&editor));
        state.save.setAction(Some(sel!(saveSession:)));
        state.cancel.setTarget(Some(&editor));
        state.cancel.setAction(Some(sel!(cancelSession:)));
        state.mode.setTarget(Some(&editor));
        state.mode.setAction(Some(sel!(changeMode:)));
        state.advanced.setTarget(Some(&editor));
        state.advanced.setAction(Some(sel!(toggleAdvanced:)));
        for field in &state.fields {
            field.setDelegate(Some(ProtocolObject::from_ref(&*editor)));
        }
        state
            .parameters
            .setDelegate(Some(ProtocolObject::from_ref(&*editor)));
    }
    panel.setDelegate(Some(ProtocolObject::from_ref(&*editor)));
    panel.setInitialFirstResponder(Some(&state.fields[0]));
    editor.refresh_validation();
    editor.layout();
    panel.center();
    panel.makeKeyAndOrderFront(None);
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    let saved = loop {
        let session = app.beginModalSessionForWindow(&panel);
        // Pump a native modal session so background connection state keeps refreshing.
        unsafe {
            while app.runModalSession(session) == NSModalResponseContinue {
                on_tick();
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&NSDate::dateWithTimeIntervalSinceNow(0.1)),
                    NSModalPanelRunLoopMode,
                    false,
                );
            }
            app.endModalSession(session);
        }
        let Some(connection) = state.result.borrow_mut().take() else {
            break false;
        };
        match on_save(connection) {
            Ok(()) => break true,
            Err(error) => {
                let message = format!("Could not save. Your changes are still here. {error:#}");
                state.persistence_error.replace(message.clone());
                editor.display_validation(None, false);
                editor.announce_error(&message);
            }
        }
    };
    panel.setDelegate(None);
    panel.orderOut(None);
    panel.close();
    Ok(saved)
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn label(text: &str, size: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize(size)));
    label.setTextColor(Some(&NSColor::labelColor()));
    label
}

fn editor_values(existing: Option<&ConnectionConfig>) -> ([String; 8], String) {
    let fields = existing
        .map(|item| {
            [
                item.name.clone(),
                item.target.clone(),
                item.region.clone().unwrap_or_default(),
                item.profile.clone().unwrap_or_default(),
                item.document_name.clone().unwrap_or_default(),
                item.parameters.get("host").cloned().unwrap_or_default(),
                item.parameters
                    .get("portNumber")
                    .cloned()
                    .unwrap_or_default(),
                item.parameters
                    .get("localPortNumber")
                    .cloned()
                    .unwrap_or_default(),
            ]
        })
        .unwrap_or_else(|| {
            ["", "", "", "", REMOTE_DOCUMENT, "", "3306", "13306"].map(str::to_owned)
        });
    let parameters = existing
        .map(|item| {
            item.parameters
                .iter()
                .filter(|(key, _)| {
                    !matches!(key.as_str(), "host" | "portNumber" | "localPortNumber")
                })
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    (fields, parameters)
}

fn connection_from_fields(
    config: &Config,
    existing: Option<&ConnectionConfig>,
    fields: &[String],
    parameters: &str,
) -> Result<ConnectionConfig> {
    let optional = |index: usize| {
        let value = fields[index].trim();
        (!value.is_empty()).then(|| value.to_owned())
    };
    let document = fields[4].trim();
    let standard = matches!(document, INSTANCE_DOCUMENT | REMOTE_DOCUMENT);
    if fields[0].trim().is_empty() {
        return Err(field_error(InputTarget::Field(0), "Enter a session name."));
    }
    if fields[1].trim().is_empty() {
        return Err(field_error(
            InputTarget::Field(1),
            "Enter the SSM target instance or managed node.",
        ));
    }
    if standard {
        for (index, name) in [(6, "remote"), (7, "local")] {
            if !matches!(fields[index].trim().parse::<u16>(), Ok(1..=65535)) {
                return Err(field_error(
                    InputTarget::Field(index),
                    format!("Enter a {name} port between 1 and 65535."),
                ));
            }
        }
        if document == REMOTE_DOCUMENT && fields[5].trim().is_empty() {
            return Err(field_error(
                InputTarget::Field(5),
                "Enter the remote host, or choose SSM Instance.",
            ));
        }
    }
    let mut parameters = config::parse_parameters(
        &parameters
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    )
    .map_err(|error| field_error(InputTarget::Parameters, error.to_string()))?;
    for (key, index) in [("host", 5), ("portNumber", 6), ("localPortNumber", 7)] {
        if key == "host" && document == INSTANCE_DOCUMENT {
            continue;
        }
        if let Some(value) = optional(index) {
            if parameters.insert(key.into(), value).is_some() {
                return Err(field_error(
                    InputTarget::Parameters,
                    format!("Set {key} in its labeled field, not Additional Parameters."),
                ));
            }
        }
    }
    let connection = ConnectionConfig {
        name: fields[0].trim().into(),
        target: fields[1].trim().into(),
        region: optional(2),
        profile: optional(3),
        document_name: optional(4),
        parameters,
        extra_args: existing
            .map(|item| item.extra_args.clone())
            .unwrap_or_default(),
    };
    if crate::ssm::is_interactive(&connection) {
        return Err(field_error(
            InputTarget::Field(4),
            "Choose a forwarding document. Interactive shells require a terminal.",
        ));
    }
    let mut candidate = config.clone();
    if let Some(existing) = existing {
        candidate
            .connections
            .retain(|item| item.name != existing.name);
    }
    candidate.connections.push(connection.clone());
    candidate
        .validate()
        .map_err(|error| field_error(InputTarget::Field(0), error.to_string()))?;
    Ok(connection)
}

pub fn confirm_delete(name: &str) -> Result<bool> {
    let mtm = MainThreadMarker::new().context("confirmation requires the main thread")?;
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Informational);
    alert.setMessageText(&NSString::from_str(&format!("Delete “{name}”?")));
    alert.setInformativeText(&NSString::from_str(
        "The saved connection will be removed. This won't delete the AWS instance or database.",
    ));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.addButtonWithTitle(&NSString::from_str("Delete Session"));
    NSApplication::sharedApplication(mtm).activate();
    Ok(alert.runModal() == NSAlertSecondButtonReturn)
}

pub fn show_error(error: &anyhow::Error) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str("Couldn't Complete That Action"));
    alert.setInformativeText(&NSString::from_str(&format!("{error:#}")));
    alert.addButtonWithTitle(&NSString::from_str("OK"));
    NSApplication::sharedApplication(mtm).activate();
    alert.runModal();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote() -> [String; 8] {
        [
            "demo",
            "i-123",
            "",
            "",
            REMOTE_DOCUMENT,
            "db.example.com",
            "3306",
            "13308",
        ]
        .map(str::to_owned)
    }

    #[test]
    fn edit_round_trips_all_connection_values_and_extra_arguments() {
        let mut connection =
            connection_from_fields(&Config::default(), None, &remote(), "custom=value").unwrap();
        connection.extra_args = vec!["--endpoint-url".into(), "https://example.test".into()];
        let config = Config {
            connections: vec![connection.clone()],
        };
        let (fields, parameters) = editor_values(Some(&connection));
        assert_eq!(
            connection_from_fields(&config, Some(&connection), &fields, &parameters).unwrap(),
            connection
        );
        assert!(connection_from_fields(&config, None, &fields, &parameters).is_err());
        let mut renamed = fields;
        renamed[0] = "renamed".into();
        assert_eq!(
            connection_from_fields(&config, Some(&connection), &renamed, &parameters)
                .unwrap()
                .name,
            "renamed"
        );
    }

    #[test]
    fn forwarding_requires_host_and_valid_ports_and_rejects_duplicate_parameters() {
        for index in [0, 1, 5, 6, 7] {
            let mut fields = remote();
            fields[index] = " ".into();
            assert!(connection_from_fields(&Config::default(), None, &fields, "").is_err());
        }
        for value in ["0", "65536", "abc"] {
            let mut fields = remote();
            fields[7] = value.into();
            assert!(connection_from_fields(&Config::default(), None, &fields, "").is_err());
        }
        assert!(
            connection_from_fields(&Config::default(), None, &remote(), "portNumber=22").is_err()
        );
        let mut instance = remote();
        instance[4] = INSTANCE_DOCUMENT.into();
        let connection = connection_from_fields(&Config::default(), None, &instance, "").unwrap();
        assert!(!connection.parameters.contains_key("host"));
    }

    #[test]
    fn validation_identifies_the_field_to_focus_without_parsing_error_text() {
        for index in [0, 1, 5, 6, 7] {
            let mut fields = remote();
            fields[index] = String::new();
            let error = connection_from_fields(&Config::default(), None, &fields, "").unwrap_err();
            assert_eq!(
                error.downcast_ref::<FieldError>().unwrap().target,
                InputTarget::Field(index)
            );
        }
        for parameters in ["invalid", "portNumber=22", "host=other.example.test"] {
            let error = connection_from_fields(&Config::default(), None, &remote(), parameters)
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<FieldError>().unwrap().target,
                InputTarget::Parameters
            );
        }
        let connection = connection_from_fields(&Config::default(), None, &remote(), "").unwrap();
        let config = Config {
            connections: vec![connection],
        };
        let error = connection_from_fields(&config, None, &remote(), "").unwrap_err();
        assert_eq!(
            error.downcast_ref::<FieldError>().unwrap().target,
            InputTarget::Field(0)
        );
        let mut fields = remote();
        fields[4] = String::new();
        let error = connection_from_fields(&Config::default(), None, &fields, "").unwrap_err();
        assert_eq!(
            error.downcast_ref::<FieldError>().unwrap().target,
            InputTarget::Field(4)
        );
    }

    #[test]
    fn custom_document_preserves_parameters_without_assuming_port_semantics() {
        let mut fields = remote();
        fields[4] = "Custom-Forwarding".into();
        fields[6] = "a,b".into();
        let connection =
            connection_from_fields(&Config::default(), None, &fields, "other=one,two").unwrap();
        let (values, parameters) = editor_values(Some(&connection));
        assert_eq!(
            connection_from_fields(&Config::default(), None, &values, &parameters).unwrap(),
            connection
        );
        fields[4] = "".into();
        assert!(connection_from_fields(&Config::default(), None, &fields, "").is_err());
    }
}
