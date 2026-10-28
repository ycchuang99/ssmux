use std::cell::{Cell, RefCell};

use anyhow::{Context, Result};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{ProtocolObject, Sel},
    sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAlertSecondButtonReturn, NSAlertStyle, NSApplication,
    NSBackingStoreType, NSBezelStyle, NSButton, NSButtonType, NSColor,
    NSControlTextEditingDelegate, NSFont, NSPanel, NSPopUpButton, NSScrollView, NSTextDelegate,
    NSTextField, NSTextFieldBezelStyle, NSTextFieldDelegate, NSTextView, NSTextViewDelegate,
    NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use crate::{
    aws_settings::AwsSettings,
    config::{self, Config, ConnectionConfig},
};

const INSTANCE_DOCUMENT: &str = "AWS-StartPortForwardingSession";
const REMOTE_DOCUMENT: &str = "AWS-StartPortForwardingSessionToRemoteHost";
const LABELS: [&str; 8] = [
    "Session Name",
    "SSM Target",
    "AWS Region",
    "AWS Profile",
    "SSM Document",
    "Remote Host",
    "Remote Port",
    "Local Port",
];

struct EditorState {
    panel: Retained<NSPanel>,
    scroll: Retained<NSScrollView>,
    document: Retained<FlippedView>,
    fields: Vec<Retained<NSTextField>>,
    labels: Vec<Retained<NSTextField>>,
    mode_label: Retained<NSTextField>,
    mode: Retained<NSPopUpButton>,
    profiles: Retained<NSPopUpButton>,
    setup: Retained<NSTextField>,
    reconnect: Retained<NSButton>,
    reconnect_hint: Retained<NSTextField>,
    advanced: Retained<NSButton>,
    parameters_label: Retained<NSTextField>,
    parameters_scroll: Retained<NSScrollView>,
    parameters: Retained<NSTextView>,
    route: Retained<NSTextField>,
    error: Retained<NSTextField>,
    cancel: Retained<NSButton>,
    save: Retained<NSButton>,
    config: Config,
    existing: Option<ConnectionConfig>,
    aws: AwsSettings,
    suggested_region: RefCell<Option<String>>,
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
                Err(error) => self.ivars().error.setStringValue(&NSString::from_str(&error.to_string())),
            }
        }
        #[unsafe(method(cancelSession:))]
        fn cancel_session(&self, _sender: &NSObject) {
            NSApplication::sharedApplication(self.mtm()).stopModalWithCode(0);
        }
        #[unsafe(method(changeMode:))]
        fn change_mode(&self, _sender: &NSObject) {
            let state = self.ivars();
            match state.mode.indexOfSelectedItem() {
                0 => state.fields[4].setStringValue(&NSString::from_str(REMOTE_DOCUMENT)),
                1 => state.fields[4].setStringValue(&NSString::from_str(INSTANCE_DOCUMENT)),
                _ => state.advanced.setState(1),
            }
            self.refresh_validation();
            self.layout();
        }
        #[unsafe(method(toggleAdvanced:))]
        fn toggle_advanced(&self, _sender: &NSObject) { self.layout(); }
        #[unsafe(method(selectProfile:))]
        fn select_profile(&self, _sender: &NSObject) {
            let state = self.ivars();
            let Some(selected) = state.profiles.titleOfSelectedItem() else { return; };
            let Some(profile) = state.aws.profiles.iter().find(|profile| profile.name == selected.to_string()) else { return; };
            state.fields[3].setStringValue(&NSString::from_str(&profile.name));
            let current = state.fields[2].stringValue().to_string();
            let suggested = state.suggested_region.borrow();
            if should_fill_region(&current, suggested.as_deref()) {
                state.fields[2].setStringValue(&NSString::from_str(profile.region.as_deref().unwrap_or("")));
                drop(suggested);
                state.suggested_region.replace(profile.region.clone());
            }
            self.refresh_validation();
        }
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
        let mut connection = connection_from_fields(
            &state.config,
            state.existing.as_ref(),
            &values,
            &state.parameters.string().to_string(),
        )?;
        connection.auto_reconnect = state.reconnect.state() == 1;
        Ok(connection)
    }

    fn refresh_validation(&self) {
        let state = self.ivars();
        let local = state.fields[7].stringValue();
        let remote = state.fields[6].stringValue();
        let destination = if state.mode.indexOfSelectedItem() == 1 {
            state.fields[1].stringValue().to_string()
        } else {
            state.fields[5].stringValue().to_string()
        };
        let destination = if destination.is_empty() {
            "destination"
        } else {
            &destination
        };
        let route = format!("localhost:{local}  →  {destination}:{remote}");
        state.route.setStringValue(&NSString::from_str(&route));
        let warning =
            local_port_warning(&state.config, state.existing.as_ref(), &local.to_string());
        let message = if state.attempted_save.get() {
            self.connection()
                .err()
                .map(|error| error.to_string())
                .unwrap_or(warning)
        } else {
            warning
        };
        state.error.setStringValue(&NSString::from_str(&message));
    }

    fn layout(&self) {
        let state = self.ivars();
        let content = state.panel.contentView().expect("panel content view");
        let size = content.bounds().size;
        let width = size.width - 48.;
        let compact = size.width < 600.;
        state
            .route
            .setFrame(rect(24., size.height - 85., width, 22.));
        state
            .scroll
            .setFrame(rect(16., 108., size.width - 32., size.height - 272.));
        state
            .setup
            .setFrame(rect(24., size.height - 149., width, 56.));
        state.error.setFrame(rect(24., 56., width, 44.));
        state
            .cancel
            .setFrame(rect(size.width - 226., 16., 92., 32.));
        state.save.setFrame(rect(size.width - 124., 16., 100., 32.));
        let width = state.scroll.contentSize().width - 16.;
        let mut y = 8.;
        let place = |index: usize, x: f64, y: f64, w: f64| {
            state.labels[index].setHidden(false);
            state.fields[index].setHidden(false);
            state.labels[index].setFrame(rect(x, y, w, 20.));
            state.fields[index].setFrame(rect(x, y + 23., w, 32.));
        };
        place(0, 8., y, width);
        y += 68.;
        state.mode_label.setFrame(rect(8., y, width, 20.));
        state.mode.setFrame(rect(5., y + 23., width + 6., 32.));
        y += 68.;
        let show_host = state.mode.indexOfSelectedItem() != 1;
        state.labels[5].setHidden(!show_host);
        state.fields[5].setHidden(!show_host);
        if show_host {
            place(5, 8., y, width);
            y += 68.;
        }
        if compact {
            place(6, 8., y, width);
            y += 68.;
            place(7, 8., y, width);
            y += 68.;
        } else {
            let half = (width - 16.) / 2.;
            place(6, 8., y, half);
            place(7, 24. + half, y, half);
            y += 68.;
        }
        place(1, 8., y, width);
        y += 68.;
        if compact {
            place(2, 8., y, width);
            y += 68.;
            place(3, 8., y, width);
            y += 68.;
        } else {
            let half = (width - 16.) / 2.;
            place(2, 8., y, half);
            place(3, 24. + half, y, half);
            y += 68.;
        }
        state.profiles.setFrame(rect(5., y, width + 6., 32.));
        y += 44.;
        state.reconnect.setFrame(rect(8., y, width, 28.));
        state.reconnect_hint.setFrame(rect(8., y + 30., width, 36.));
        y += 76.;
        state.advanced.setFrame(rect(8., y, width, 32.));
        y += 44.;
        let advanced = state.advanced.state() == 1 || state.mode.indexOfSelectedItem() == 2;
        state.labels[4].setHidden(!advanced);
        state.fields[4].setHidden(!advanced);
        state.fields[4].setEnabled(state.mode.indexOfSelectedItem() == 2);
        state.parameters_label.setHidden(!advanced);
        state.parameters_scroll.setHidden(!advanced);
        if advanced {
            place(4, 8., y, width);
            y += 68.;
            state.parameters_label.setFrame(rect(8., y, width, 20.));
            state
                .parameters_scroll
                .setFrame(rect(8., y + 23., width, 112.));
            y += 151.;
            state
                .parameters
                .setFrameSize(NSSize::new(width - 20., 112.));
        }
        state
            .document
            .setFrameSize(NSSize::new(state.scroll.contentSize().width, y));
    }
}

pub fn edit_connection(
    config: &Config,
    existing: Option<&ConnectionConfig>,
    aws: &AwsSettings,
) -> Result<Option<ConnectionConfig>> {
    edit_form(
        config,
        existing,
        aws,
        if existing.is_some() {
            "Edit Session"
        } else {
            "New Session"
        },
    )
}

pub fn duplicate_connection(
    config: &Config,
    source: &ConnectionConfig,
    aws: &AwsSettings,
) -> Result<Option<ConnectionConfig>> {
    let copy = duplicate_draft(config, source);
    edit_form(config, Some(&copy), aws, "Duplicate Session")
}

fn edit_form(
    config: &Config,
    existing: Option<&ConnectionConfig>,
    aws: &AwsSettings,
    title: &str,
) -> Result<Option<ConnectionConfig>> {
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
    let setup = NSTextField::wrappingLabelWithString(&NSString::from_str(&aws.summary()), mtm);
    setup.setFont(Some(&NSFont::systemFontOfSize(12.)));
    setup.setTextColor(Some(&NSColor::secondaryLabelColor()));
    setup.setAccessibilityLabel(Some(&NSString::from_str("Local AWS Setup")));
    content.addSubview(&setup);
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
    let profiles = NSPopUpButton::new(mtm);
    profiles.addItemWithTitle(&NSString::from_str("Choose Existing AWS Profile…"));
    for profile in &aws.profiles {
        profiles.addItemWithTitle(&NSString::from_str(&profile.name));
    }
    if aws.profiles.iter().any(|profile| profile.name == values[3]) {
        profiles.selectItemWithTitle(&NSString::from_str(&values[3]));
    }
    profiles.setEnabled(!aws.profiles.is_empty());
    profiles.setAccessibilityLabel(Some(&NSString::from_str("Choose Existing AWS Profile")));
    document.addSubview(&profiles);
    let reconnect = NSButton::new(mtm);
    reconnect.setButtonType(NSButtonType::Switch);
    reconnect.setTitle(&NSString::from_str(
        "Reconnect After an Unexpected Disconnect",
    ));
    reconnect.setState(isize::from(
        existing.is_some_and(|item| item.auto_reconnect),
    ));
    reconnect.setAccessibilityLabel(Some(&NSString::from_str(
        "Reconnect After an Unexpected Disconnect",
    )));
    document.addSubview(&reconnect);
    let reconnect_hint = NSTextField::wrappingLabelWithString(
        &NSString::from_str("Limited retries while SSMux is open. Stopping a session cancels retries; quitting stops all sessions."), mtm,
    );
    reconnect_hint.setFont(Some(&NSFont::systemFontOfSize(12.)));
    reconnect_hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
    document.addSubview(&reconnect_hint);
    let mode_label = label("Forward To", 13., mtm);
    let mode = NSPopUpButton::new(mtm);
    for title in ["Remote Host", "SSM Instance", "Custom Document"] {
        mode.addItemWithTitle(&NSString::from_str(title));
    }
    mode.selectItemAtIndex(match values[4].as_str() {
        REMOTE_DOCUMENT => 0,
        INSTANCE_DOCUMENT => 1,
        _ => 2,
    });
    mode.setAccessibilityLabel(Some(&NSString::from_str("Forward To")));
    document.addSubview(&mode_label);
    document.addSubview(&mode);
    let advanced = NSButton::new(mtm);
    advanced.setButtonType(NSButtonType::Switch);
    advanced.setTitle(&NSString::from_str("Advanced Settings"));
    advanced.setState(isize::from(
        !parameter_text.is_empty() || mode.indexOfSelectedItem() == 2,
    ));
    document.addSubview(&advanced);
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
        mode,
        profiles,
        setup,
        reconnect,
        reconnect_hint,
        advanced,
        parameters_label,
        parameters_scroll,
        parameters,
        route,
        error,
        cancel,
        save,
        config: config.clone(),
        existing: existing.cloned(),
        aws: aws.clone(),
        suggested_region: RefCell::new(None),
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
        state.profiles.setTarget(Some(&editor));
        state.profiles.setAction(Some(sel!(selectProfile:)));
        for field in &state.fields {
            field.setDelegate(Some(ProtocolObject::from_ref(&*editor)));
        }
        state
            .parameters
            .setDelegate(Some(ProtocolObject::from_ref(&*editor)));
        let order: [&NSView; 15] = [
            &state.fields[0],
            &state.mode,
            &state.fields[5],
            &state.fields[6],
            &state.fields[7],
            &state.fields[1],
            &state.fields[2],
            &state.fields[3],
            &state.profiles,
            &state.reconnect,
            &state.advanced,
            &state.fields[4],
            &state.parameters,
            &state.cancel,
            &state.save,
        ];
        for pair in order.windows(2) {
            pair[0].setNextKeyView(Some(pair[1]));
        }
        state.save.setNextKeyView(Some(&state.fields[0]));
    }
    panel.setDelegate(Some(ProtocolObject::from_ref(&*editor)));
    panel.setInitialFirstResponder(Some(&state.fields[0]));
    editor.refresh_validation();
    editor.layout();
    panel.center();
    panel.makeKeyAndOrderFront(None);
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    app.runModalForWindow(&panel);
    panel.setDelegate(None);
    panel.orderOut(None);
    panel.close();
    let result = editor.ivars().result.borrow_mut().take();
    Ok(result)
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

fn should_fill_region(current: &str, previous_suggestion: Option<&str>) -> bool {
    current.trim().is_empty() || previous_suggestion == Some(current)
}

fn duplicate_draft(config: &Config, source: &ConnectionConfig) -> ConnectionConfig {
    let mut copy = source.clone();
    copy.name = format!("{} Copy", source.name);
    let mut number = 2;
    while config.connections.iter().any(|item| item.name == copy.name) {
        copy.name = format!("{} Copy {number}", source.name);
        number += 1;
    }
    copy.parameters.remove("localPortNumber");
    copy.auto_reconnect = false;
    copy.pinned = false;
    copy
}

fn local_port_warning(config: &Config, existing: Option<&ConnectionConfig>, port: &str) -> String {
    let Ok(port) = port.trim().parse::<u16>() else {
        return String::new();
    };
    let conflict = config.connections.iter().find(|item| {
        existing.is_none_or(|current| current.name != item.name)
            && item
                .parameters
                .get("localPortNumber")
                .and_then(|value| value.parse::<u16>().ok())
                == Some(port)
    });
    conflict.map(|item| format!("Local port {port} is also used by “{}”. These sessions cannot run together; choose a different port to use both.", item.name)).unwrap_or_default()
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
        anyhow::bail!("Enter a session name.");
    }
    if fields[1].trim().is_empty() {
        anyhow::bail!("Enter the SSM target instance or managed node.");
    }
    if standard {
        for (index, name) in [(6, "remote"), (7, "local")] {
            if !matches!(fields[index].trim().parse::<u16>(), Ok(1..=65535)) {
                anyhow::bail!("Enter a {name} port between 1 and 65535.");
            }
        }
        if document == REMOTE_DOCUMENT && fields[5].trim().is_empty() {
            anyhow::bail!("Enter the remote host, or choose SSM Instance.");
        }
    }
    let mut parameters = config::parse_parameters(
        &parameters
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    )?;
    for (key, index) in [("host", 5), ("portNumber", 6), ("localPortNumber", 7)] {
        if key == "host" && document == INSTANCE_DOCUMENT {
            continue;
        }
        if let Some(value) = optional(index) {
            if parameters.insert(key.into(), value).is_some() {
                anyhow::bail!("Set {key} in its labeled field, not Additional Parameters.");
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
        auto_reconnect: existing.is_some_and(|item| item.auto_reconnect),
        pinned: existing.is_some_and(|item| item.pinned),
    };
    if crate::ssm::is_interactive(&connection) {
        anyhow::bail!("Choose a forwarding document. Interactive shells require a terminal.");
    }
    let mut candidate = config.clone();
    if let Some(existing) = existing {
        candidate
            .connections
            .retain(|item| item.name != existing.name);
    }
    candidate.connections.push(connection.clone());
    candidate.validate()?;
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
        connection.auto_reconnect = true;
        connection.pinned = true;
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

    #[test]
    fn profile_selection_preserves_explicit_regions() {
        assert!(should_fill_region("", None));
        assert!(should_fill_region("us-west-2", Some("us-west-2")));
        assert!(!should_fill_region("us-east-1", Some("us-west-2")));
        assert!(!should_fill_region("us-east-1", None));
    }

    #[test]
    fn duplicating_requires_a_new_local_port_and_preserves_advanced_arguments() {
        let mut source =
            connection_from_fields(&Config::default(), None, &remote(), "custom=value").unwrap();
        source.extra_args = vec!["--endpoint-url".into(), "https://example.test".into()];
        source.auto_reconnect = true;
        source.pinned = true;
        let mut config = Config {
            connections: vec![source.clone()],
        };
        let first = duplicate_draft(&config, &source);
        config.connections.push(first.clone());
        let copy = duplicate_draft(&config, &source);
        assert_eq!(copy.name, "demo Copy 2");
        assert_eq!(copy.extra_args, source.extra_args);
        assert_eq!(copy.parameters["custom"], "value");
        assert!(!copy.auto_reconnect);
        assert!(!copy.pinned);
        let (mut fields, parameters) = editor_values(Some(&copy));
        assert!(connection_from_fields(&config, Some(&copy), &fields, &parameters).is_err());
        fields[7] = "13309".into();
        let saved = connection_from_fields(&config, Some(&copy), &fields, &parameters).unwrap();
        assert_eq!(saved.extra_args, source.extra_args);
        assert!(local_port_warning(&config, Some(&copy), "13308").contains("demo"));
        assert!(local_port_warning(&config, Some(&copy), "13309").is_empty());
        assert!(local_port_warning(&config, Some(&source), "13308").is_empty());
    }
}
