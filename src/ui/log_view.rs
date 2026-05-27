//! Log view component — scrollable text output for subprocess logs.
//!
//! Pattern: Append-only scrolling text view
//! Uses a `gtk::TextView` inside a `gtk::ScrolledWindow` with auto-scroll behavior.
//! The text view is non-editable and uses a monospace font for log readability.
//!
//! HIG notes:
//! - Monospace text is appropriate for technical/log output
//! - Auto-scroll keeps latest output visible without user intervention
//! - The view should be selectable so users can copy error messages

use gtk::prelude::*;
use relm4::prelude::*;

/// Input messages for the log view.
#[derive(Debug)]
pub enum LogViewInput {
    /// Append a single line to the log.
    AppendLine(String),
    /// Clear all log content.
    Clear,
}

/// The log view model.
pub struct LogView {
    buffer: gtk::TextBuffer,
    /// We store the text_view ref so we can auto-scroll in update().
    text_view: gtk::TextView,
}

#[relm4::component(pub)]
impl SimpleComponent for LogView {
    type Init = ();
    type Input = LogViewInput;
    type Output = ();

    view! {
        #[root]
        gtk::ScrolledWindow {
            set_vexpand: true,
            set_hexpand: true,
            set_min_content_height: 200,
            set_hscrollbar_policy: gtk::PolicyType::Automatic,
            set_vscrollbar_policy: gtk::PolicyType::Automatic,

            // Use #[local_ref] to reference our pre-created text_view.
            // This is the relm4 pattern for widgets you need to keep a handle to.
            #[local_ref]
            text_view -> gtk::TextView {
                set_editable: false,
                set_cursor_visible: false,
                add_css_class: "monospace",
                set_left_margin: 12,
                set_right_margin: 12,
                set_top_margin: 8,
                set_bottom_margin: 8,
                set_wrap_mode: gtk::WrapMode::WordChar,
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let buffer = gtk::TextBuffer::new(None);

        // Pre-create the text view so we can store a reference in the model
        // and use #[local_ref] in the view! macro.
        let text_view_widget = gtk::TextView::builder().buffer(&buffer).build();

        let model = LogView {
            buffer,
            text_view: text_view_widget.clone(),
        };

        // The view! macro references `text_view` via #[local_ref]
        let text_view = &model.text_view;
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            LogViewInput::AppendLine(line) => {
                let mut end_iter = self.buffer.end_iter();
                if self.buffer.char_count() > 0 {
                    self.buffer.insert(&mut end_iter, "\n");
                    end_iter = self.buffer.end_iter();
                }
                self.buffer.insert(&mut end_iter, &line);

                // Auto-scroll to the end after inserting.
                let end_iter = self.buffer.end_iter();
                let end_mark = self.buffer.create_mark(None, &end_iter, false);
                self.text_view
                    .scroll_to_mark(&end_mark, 0.0, true, 0.0, 1.0);
                self.buffer.delete_mark(&end_mark);
            }
            LogViewInput::Clear => {
                self.buffer.set_text("");
            }
        }
    }
}
