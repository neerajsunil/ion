//! Desktop notifications, for an agent that finishes or needs input while
//! Ion is in the background.
//!
//! On Windows a toast is shown by a short-lived, hidden Windows PowerShell
//! process (GPUI has no notification API, and Ion's own code has no
//! `unsafe` to call WinRT directly). It runs only when a notification is
//! due, so nothing waits or polls in between.

use gpui::{Context, Entity, SharedString, Window};
use terminal::TerminalView;

use crate::workspace::Workspace;

impl Workspace {
    /// A terminal wants attention: notify if Ion isn't the active window.
    pub(crate) fn notify_attention(
        &mut self,
        view: &Entity<TerminalView>,
        message: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.is_window_active() || !settings::get(cx).desktop_notifications {
            return;
        }
        let view = view.read(cx);
        let title = view.title().to_string();
        let body = match (message, view.harness()) {
            (Some(message), _) => message.to_string(),
            (None, Some(_)) => "Finished, or waiting for your input".to_owned(),
            (None, None) => "Needs your attention".to_owned(),
        };
        let project = self
            .root
            .as_ref()
            .and_then(|root| root.file_name())
            .map(|name| format!(" · {}", name.to_string_lossy()))
            .unwrap_or_default();
        show(&format!("{title}{project}"), &body);
    }
}

/// Shows a notification; failures are ignored (it's only a nudge).
pub(crate) fn show(title: &str, body: &str) {
    #[cfg(windows)]
    windows_toast(title, body);
    #[cfg(not(windows))]
    let _ = (title, body);
}

/// The ID of Windows PowerShell's registered app, which may show toasts
/// without an installed shortcut of Ion's own.
#[cfg(windows)]
const APP_ID: &str =
    r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";

#[cfg(windows)]
fn windows_toast(title: &str, body: &str) {
    use std::os::windows::process::CommandExt;

    use base64::Engine;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = toast_script(title, body);
    // -EncodedCommand takes UTF-16LE base64, which needs no quoting.
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-EncodedCommand",
            &encoded,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .ok();
}

#[cfg(any(windows, test))]
fn toast_script(title: &str, body: &str) -> String {
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text>\
         </binding></visual></toast>",
        xml_escape(title),
        xml_escape(body)
    );
    format!(
        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, \
         ContentType = WindowsRuntime] | Out-Null\n\
         [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, \
         ContentType = WindowsRuntime] | Out-Null\n\
         $xml = New-Object Windows.Data.Xml.Dom.XmlDocument\n\
         $xml.LoadXml('{}')\n\
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{}')\
         .Show([Windows.UI.Notifications.ToastNotification]::new($xml))\n",
        xml.replace('\'', "''"),
        APP_ID_FOR_SCRIPT
    )
}

#[cfg(windows)]
const APP_ID_FOR_SCRIPT: &str = APP_ID;
#[cfg(all(test, not(windows)))]
const APP_ID_FOR_SCRIPT: &str = "test";

#[cfg(any(windows, test))]
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // Control characters aren't allowed in XML.
            ch if ch.is_control() => out.push(' '),
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_text_for_xml_and_powershell() {
        let script = toast_script("Claude <Code>", "It's done & \"ready\"");
        assert!(script.contains("Claude &lt;Code&gt;"));
        // The single quote is doubled inside the PowerShell string.
        assert!(script.contains("It''s done &amp; &quot;ready&quot;"));
    }
}
