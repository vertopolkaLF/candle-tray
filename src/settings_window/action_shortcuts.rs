//! Live editor for ordered monitor actions, using the existing settings kit.
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, Window, div, px};

use super::{
    input::HasInputs,
    kit::{self, Button, Kit, MenuItem, Row},
    window::SettingsWindow,
};
use crate::{
    actions::{Action, Control, Operation, Shortcut},
    cli::Selector,
};

fn tr(key: &str) -> gpui::SharedString {
    crate::i18n::tr(key).into()
}

fn number(text: &str) -> anyhow::Result<i32> {
    let text = text.trim();
    Ok(
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            i32::from_str_radix(hex, 16)?
        } else {
            text.parse()?
        },
    )
}

impl SettingsWindow {
    /// One card per shortcut, laid out like a profile: name, shortcut, its
    /// actions in order, then the shortcut's own buttons.
    pub(super) fn action_shortcuts_page(
        &mut self,
        k: &mut Kit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let settings = self.model.read(cx).settings.clone();
        let new = Button::new("actions-new", tr("actions-new"))
            .with_icon("plus-bold")
            .on_click(Self::h(cx, |this, (), _, cx| {
                this.edit(cx, |s| {
                    s.action_shortcuts.push(Shortcut {
                        actions: vec![Action::default()],
                        ..Shortcut::default()
                    })
                });
            }))
            .render(k);
        let mut page = vec![kit::section_header(
            k,
            tr("actions-title"),
            Some(tr("actions-help")),
            Some(new),
        )];
        for (index, shortcut) in settings.action_shortcuts.iter().enumerate() {
            let name = self.input(
                format!("action-name-{index}"),
                &shortcut.name,
                tr("actions-name"),
                window,
                cx,
                move |this, text, _, _, cx| {
                    this.edit(cx, move |s| {
                        if let Some(shortcut) = s.action_shortcuts.get_mut(index) {
                            shortcut.name = text;
                        }
                    })
                },
            );
            let mut rows = vec![
                Row::new(format!("action-name-row-{index}"), tr("actions-name"))
                    .trailing(kit::text_field(k, &name, Some(240.0), window, cx))
                    .render(k),
                self.hotkey_row(
                    k,
                    crate::hotkey::Action::Sequence(index),
                    shortcut.hotkey,
                    tr("profiles-shortcut"),
                    None,
                    cx,
                ),
            ];
            let count = shortcut.actions.len();
            for (position, action) in shortcut.actions.iter().enumerate() {
                rows.push(self.action_row(k, (index, position, count), action, window, cx));
            }
            let add = Button::new(format!("actions-add-{index}"), tr("actions-add"))
                .with_icon("plus-bold")
                .on_click(Self::h(cx, move |this, (), _, cx| {
                    this.edit(cx, move |s| {
                        if let Some(shortcut) = s.action_shortcuts.get_mut(index) {
                            shortcut.actions.push(Action::default());
                        }
                    })
                }))
                .render(k);
            let delete = Button::new(
                format!("actions-delete-{index}"),
                tr("actions-delete-shortcut"),
            )
            .danger()
            .on_click(Self::h(cx, move |this, (), _, cx| {
                this.stop_hotkey_recording();
                this.inputs_mut().remove_prefixed("action-");
                this.edit(cx, move |s| {
                    if index < s.action_shortcuts.len() {
                        s.action_shortcuts.remove(index);
                    }
                });
            }))
            .render(k);
            rows.push(
                Row::new(format!("actions-manage-{index}"), tr("profiles-manage"))
                    .trailing(add)
                    .trailing(delete)
                    .render(k),
            );
            page.push(kit::card(k, rows));
        }
        page
    }

    /// One action as a single row: its fields side by side, and a menu to
    /// reorder or remove it.
    fn action_row(
        &mut self,
        k: &mut Kit,
        (index, position, count): (usize, usize, usize),
        action: &Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefix = format!("action-{index}-{position}");
        let mut selectors = vec![Selector::All];
        let mut labels = vec![tr("actions-all")];
        for row in self.model.read(cx).rows() {
            labels.push(row.name.into());
            selectors.push(Selector::Id(row.monitor.id));
        }
        if !selectors.contains(&action.selector) {
            // Imported or scripted selectors keep a readable label.
            labels.push(match &action.selector {
                Selector::Position(position) => format!("#{position}").into(),
                Selector::Id(text) | Selector::PartialId(text) | Selector::NameOrId(text) => {
                    text.clone().into()
                }
                Selector::All => tr("actions-all"),
            });
            selectors.push(action.selector.clone());
        }
        let selected = selectors
            .iter()
            .position(|s| *s == action.selector)
            .unwrap_or(0);
        let target = kit::dropdown(
            k,
            format!("{prefix}-target"),
            labels,
            Some(selected),
            false,
            180.0,
            Self::h(cx, move |this, choice: usize, _, cx| {
                if let Some(selector) = selectors.get(choice).cloned() {
                    this.edit_action(index, position, cx, move |a| a.selector = selector);
                }
            }),
        );
        let control = kit::dropdown(
            k,
            format!("{prefix}-control"),
            [
                "profiles-brightness",
                "popup-sdr",
                "profiles-contrast",
                "actions-vcp",
                "actions-input",
                "actions-power",
            ]
            .iter()
            .map(|key| tr(key))
            .collect(),
            Some(match action.control {
                Control::Brightness => 0,
                Control::Sdr => 1,
                Control::Contrast => 2,
                Control::Vcp(_) => 3,
                Control::Input => 4,
                Control::Power => 5,
            }),
            false,
            150.0,
            Self::h(cx, move |this, choice: usize, _, cx| {
                this.inputs_mut()
                    .remove_prefixed(&format!("action-{index}-{position}"));
                this.edit_action(index, position, cx, move |a| {
                    a.control = match choice {
                        1 => Control::Sdr,
                        2 => Control::Contrast,
                        3 => Control::Vcp(0x10),
                        4 => Control::Input,
                        5 => Control::Power,
                        _ => Control::Brightness,
                    };
                    a.operation = Operation::Set(match a.control {
                        Control::Input => 17,
                        Control::Power => 1,
                        _ => 50,
                    });
                });
            }),
        );
        let mut fields = div()
            .pt(px(8.0))
            .flex()
            .flex_wrap()
            .gap(px(8.0))
            .child(kit::field(k, tr("actions-target"), target))
            .child(kit::field(k, tr("actions-control"), control));
        if let Control::Vcp(code) = action.control {
            let input = self.input(
                format!("{prefix}-code"),
                &format!("0x{code:02X}"),
                "0x10",
                window,
                cx,
                move |this, text, _, _, cx| match number(&text)
                    .ok()
                    .and_then(|v| u8::try_from(v).ok())
                    .filter(|v| *v != 0)
                {
                    Some(code) => this
                        .edit_action(index, position, cx, move |a| a.control = Control::Vcp(code)),
                    None => this.show_error(tr("actions-invalid"), cx),
                },
            );
            fields = fields.child(kit::field(
                k,
                tr("actions-code"),
                kit::text_field(k, &input, Some(80.0), window, cx),
            ));
        }
        let operation = kit::dropdown(
            k,
            format!("{prefix}-operation"),
            ["actions-set", "actions-offset", "actions-cycle"]
                .iter()
                .map(|key| tr(key))
                .collect(),
            Some(match action.operation {
                Operation::Set(_) => 0,
                Operation::Offset(_) => 1,
                Operation::Cycle(_) => 2,
            }),
            false,
            120.0,
            Self::h(cx, move |this, choice: usize, _, cx| {
                this.inputs_mut()
                    .remove_prefixed(&format!("action-{index}-{position}"));
                this.edit_action(index, position, cx, move |a| {
                    let initial = match &a.operation {
                        Operation::Set(v) => *v,
                        Operation::Cycle(v) => v.first().copied().unwrap_or(0),
                        Operation::Offset(_) => match a.control {
                            Control::Input => 17,
                            Control::Power => 1,
                            _ => 50,
                        },
                    };
                    a.operation = match choice {
                        1 => Operation::Offset(1),
                        2 => Operation::Cycle(vec![initial]),
                        _ => Operation::Set(initial),
                    };
                });
            }),
        );
        let value = match &action.operation {
            Operation::Set(v) | Operation::Offset(v) => v.to_string(),
            Operation::Cycle(v) => v
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        };
        let current = action.clone();
        let input = self.input(
            format!("{prefix}-values"),
            &value,
            "25, 50, 100",
            window,
            cx,
            move |this, text, _, _, cx| {
                let parsed = (|| -> anyhow::Result<Operation> {
                    let operation = match current.operation {
                        Operation::Set(_) => Operation::Set(number(&text)?),
                        Operation::Offset(_) => Operation::Offset(number(&text)?),
                        Operation::Cycle(_) => Operation::Cycle(
                            text.split(',')
                                .map(number)
                                .collect::<anyhow::Result<Vec<_>>>()?,
                        ),
                    };
                    let mut candidate = current.clone();
                    candidate.operation = operation.clone();
                    candidate.validate()?;
                    Ok(operation)
                })();
                match parsed {
                    Ok(operation) => {
                        this.edit_action(index, position, cx, move |a| a.operation = operation)
                    }
                    Err(_) => this.show_error(tr("actions-invalid"), cx),
                }
            },
        );
        fields = fields
            .child(kit::field(k, tr("actions-operation"), operation))
            .child(kit::field(
                k,
                tr("actions-values"),
                kit::text_field(k, &input, Some(140.0), window, cx),
            ));

        let mut up = MenuItem::new(tr("monitors-move-up"));
        if position == 0 {
            up = up.disabled();
        }
        let mut down = MenuItem::new(tr("monitors-move-down")).separator();
        if position + 1 >= count {
            down = down.disabled();
        }
        let menu = kit::more_menu(
            k,
            format!("{prefix}-menu"),
            vec![up, down, MenuItem::new(tr("actions-delete")).danger()],
            Self::h(cx, move |this, choice: usize, _, cx| {
                this.inputs_mut().remove_prefixed("action-");
                this.edit(cx, move |s| {
                    let Some(shortcut) = s.action_shortcuts.get_mut(index) else {
                        return;
                    };
                    let len = shortcut.actions.len();
                    match choice {
                        0 if position > 0 => shortcut.actions.swap(position, position - 1),
                        1 if position + 1 < len => shortcut.actions.swap(position, position + 1),
                        2 if position < len => {
                            shortcut.actions.remove(position);
                        }
                        _ => {}
                    }
                });
            }),
        );
        Row::new(
            format!("{prefix}-row"),
            format!("{} {}", crate::i18n::tr("actions-action"), position + 1),
        )
        .detail(fields.into_any_element())
        .trailing(menu)
        .render(k)
    }

    fn edit_action(
        &mut self,
        index: usize,
        position: usize,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Action) + 'static,
    ) {
        let Some(mut candidate) = self
            .model
            .read(cx)
            .settings
            .action_shortcuts
            .get(index)
            .and_then(|s| s.actions.get(position))
            .cloned()
        else {
            return;
        };
        edit(&mut candidate);
        if candidate.validate().is_err() {
            self.show_error(tr("actions-invalid"), cx);
            return;
        }
        self.edit(cx, move |s| {
            if let Some(action) = s
                .action_shortcuts
                .get_mut(index)
                .and_then(|s| s.actions.get_mut(position))
            {
                *action = candidate;
            }
        });
    }
}
