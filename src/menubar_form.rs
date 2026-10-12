use anyhow::{Context, Result};
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSScrollView, NSTextField,
    NSTextView, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::config::{self, Config, ConnectionConfig};

pub fn new_connection(config: &Config) -> Result<Option<ConnectionConfig>> {
    let mtm = MainThreadMarker::new().context("session form requires the main thread")?;
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Informational);
    alert.setMessageText(&NSString::from_str("New Session"));
    alert.setInformativeText(&NSString::from_str("Save a connection to your SSMux configuration. It will not connect until you choose Start Session."));
    alert.addButtonWithTitle(&NSString::from_str("Save Session"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    let view = NSView::new(mtm);
    view.setFrame(NSRect::new(NSPoint::new(0., 0.), NSSize::new(450., 350.)));
    let labels = ["Name *", "Target *", "Region", "AWS Profile", "Document"];
    let placeholders = [
        "e.g. staging-db",
        "i-0123456789abcdef0",
        "AWS default",
        "AWS default",
        "Leave blank for an interactive shell",
    ];
    let mut fields = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        let y = 310. - i as f64 * 42.;
        let caption = NSTextField::labelWithString(&NSString::from_str(label), mtm);
        caption.setFrame(NSRect::new(NSPoint::new(0., y), NSSize::new(90., 24.)));
        view.addSubview(&caption);
        let value = if i == 4 {
            "AWS-StartPortForwardingSession"
        } else {
            ""
        };
        let field = NSTextField::textFieldWithString(&NSString::from_str(value), mtm);
        field.setFrame(NSRect::new(NSPoint::new(95., y), NSSize::new(350., 26.)));
        field.setPlaceholderString(Some(&NSString::from_str(placeholders[i])));
        view.addSubview(&field);
        fields.push(field);
    }
    let label = NSTextField::labelWithString(
        &NSString::from_str("Parameters (one key=value per line)"),
        mtm,
    );
    label.setFrame(NSRect::new(NSPoint::new(0., 105.), NSSize::new(450., 22.)));
    view.addSubview(&label);
    let scroll = NSScrollView::new(mtm);
    scroll.setFrame(NSRect::new(NSPoint::new(0., 0.), NSSize::new(450., 100.)));
    scroll.setHasVerticalScroller(true);
    let parameters = NSTextView::new(mtm);
    parameters.setFrame(NSRect::new(NSPoint::new(0., 0.), NSSize::new(430., 100.)));
    parameters.setString(&NSString::from_str("portNumber=22\nlocalPortNumber=10022"));
    parameters.setRichText(false);
    parameters.setTextContainerInset(NSSize::new(6., 6.));
    scroll.setDocumentView(Some(&parameters));
    view.addSubview(&scroll);
    alert.setAccessoryView(Some(&view));
    // Explicit focus order keeps Tab within the form before reaching Save and Cancel.
    for pair in fields.windows(2) {
        unsafe {
            pair[0].setNextKeyView(Some(&pair[1]));
        }
    }
    unsafe {
        fields[4].setNextKeyView(Some(&parameters));
    }
    alert.window().setInitialFirstResponder(Some(&fields[0]));
    NSApplication::sharedApplication(mtm).activate();
    loop {
        if alert.runModal() != NSAlertFirstButtonReturn {
            return Ok(None);
        }
        let values = fields
            .iter()
            .map(|field| field.stringValue().to_string())
            .collect::<Vec<_>>();
        match connection_from_fields(config, &values, &parameters.string().to_string()) {
            Ok(connection) => return Ok(Some(connection)),
            Err(error) => alert.setInformativeText(&NSString::from_str(&format!(
                "{error}\nPlease correct the fields and save again."
            ))),
        }
    }
}

fn connection_from_fields(
    config: &Config,
    fields: &[String],
    parameters: &str,
) -> Result<ConnectionConfig> {
    let optional = |index: usize| {
        let value = fields[index].trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_owned())
        }
    };
    let connection = ConnectionConfig {
        name: fields[0].trim().to_owned(),
        target: fields[1].trim().to_owned(),
        region: optional(2),
        profile: optional(3),
        document_name: optional(4),
        parameters: config::parse_parameters(
            &parameters
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )?,
        extra_args: Vec::new(),
    };
    let mut candidate = config.clone();
    candidate.connections.push(connection.clone());
    candidate.validate()?;
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_validates_names_and_multiline_parameters_before_saving() {
        let fields = ["demo", "i-123", "", "", "AWS-StartPortForwardingSession"].map(str::to_owned);
        let connection = connection_from_fields(
            &Config::default(),
            &fields,
            "portNumber=22\n\nlocalPortNumber=10022",
        )
        .unwrap();
        assert_eq!(connection.parameters["localPortNumber"], "10022");
        assert!(connection.region.is_none());
        let existing = Config {
            connections: vec![connection],
        };
        assert!(connection_from_fields(&existing, &fields, "").is_err());
        assert!(connection_from_fields(
            &Config::default(),
            &fields,
            "portNumber=22\nportNumber=80"
        )
        .is_err());
        let mut blank = fields.clone();
        blank[1] = " ".to_owned();
        assert!(connection_from_fields(&Config::default(), &blank, "").is_err());
    }
}
