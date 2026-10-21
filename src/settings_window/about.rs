//! The About page: version, updates and project links.

use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, Styled, div, px};

use super::kit::{self, Button, Kit, Row};
use super::window::SettingsWindow;
use crate::updater::{self, ISSUES_URL, RELEASES_URL, REPO_URL, UpdatePhase};

fn tr(key: &str) -> String {
    crate::i18n::tr(key)
}

fn status_label(phase: &UpdatePhase) -> String {
    match phase {
        UpdatePhase::Idle => tr("about-update-idle"),
        UpdatePhase::Checking => tr("about-update-checking"),
        UpdatePhase::UpToDate => tr("about-update-current"),
        UpdatePhase::Available(update) => crate::i18n::format(
            "about-update-available",
            &[("version", update.version.clone())],
        ),
        UpdatePhase::Installing { progress, .. } => match progress {
            Some(fraction) => crate::i18n::format(
                "about-update-downloading-percent",
                &[("percent", format!("{:.0}", fraction * 100.0))],
            ),
            None => tr("about-update-downloading"),
        },
        UpdatePhase::Restarting => tr("about-update-restarting"),
        // Raw transport errors stay in the log.
        UpdatePhase::Failed => tr("about-update-failed"),
    }
}

impl SettingsWindow {
    pub(super) fn about_page(&mut self, k: &mut Kit, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = k.theme.clone();
        let hero = div()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(8.0))
            .pb(px(24.0))
            .child(kit::image("color/app-icon.png", 96.0).mb(px(14.0)))
            .child(
                div()
                    .text_size(px(28.0))
                    .line_height(px(34.0))
                    .font_weight(FontWeight::BOLD)
                    .child(tr("app-name")),
            )
            .child(
                div()
                    .mt(px(8.0))
                    .max_w(px(420.0))
                    .text_center()
                    .text_size(px(14.0))
                    .line_height(px(20.0))
                    .text_color(theme.text_secondary)
                    .child(tr("about-tagline")),
            )
            .child(
                div()
                    .mt(px(14.0))
                    .h(px(22.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(theme.accent_soft)
                    .text_size(px(12.0))
                    .line_height(px(16.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.accent_text)
                    .child(crate::i18n::format(
                        "about-version",
                        &[("version", updater::current_version().to_string())],
                    )),
            )
            .into_any_element();

        let phase = updater::phase();
        let status_icon = match phase {
            UpdatePhase::UpToDate => kit::icon("check-circle-fill", 18.0, theme.success),
            UpdatePhase::Available(_) => kit::icon("info-fill", 18.0, theme.accent_text),
            UpdatePhase::Failed => kit::icon("warning-fill", 18.0, theme.caution),
            _ => kit::icon("arrow-counter-clockwise-bold", 18.0, theme.text_secondary),
        };
        let status = Row::new("about-status", status_label(&phase)).icon(
            div()
                .size(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .child(status_icon)
                .into_any_element(),
        );
        let status = match &phase {
            UpdatePhase::Available(_) => status
                .description_opt(
                    k,
                    updater::install_failed().then(|| tr("about-update-install-failed")),
                )
                .trailing(
                    Button::new("about-whats-new", tr("about-whats-new"))
                        .on_click(kit::handler(|(), _, _| {
                            let url = match updater::phase() {
                                UpdatePhase::Available(update) => update.html_url,
                                _ => RELEASES_URL.to_owned(),
                            };
                            updater::open_url(&url);
                        }))
                        .render(k),
                )
                .trailing(
                    Button::new("about-update", tr("about-update"))
                        .accent()
                        .on_click(kit::handler(|(), _, _| updater::install()))
                        .render(k),
                ),
            _ => {
                let busy = matches!(
                    phase,
                    UpdatePhase::Checking
                        | UpdatePhase::Installing { .. }
                        | UpdatePhase::Restarting
                );
                status.trailing(
                    Button::new("about-check", tr("about-check"))
                        .accent()
                        .disabled(busy)
                        .on_click(kit::handler(|(), _, _| updater::check_now()))
                        .render(k),
                )
            }
        };
        let check_for_updates = self.model.read(cx).settings.check_for_updates;
        let updates = kit::card_of(k, |k| {
            vec![
                status.render(k),
                kit::toggle_row(
                    k,
                    "about-auto-check",
                    tr("about-auto-check"),
                    Some(tr("about-auto-check-description").into()),
                    check_for_updates,
                    Self::h(cx, |this, value: bool, _, cx| {
                        this.edit(cx, move |settings| settings.check_for_updates = value)
                    }),
                ),
            ]
        });

        let link = |k: &mut Kit, id: &'static str, label: String, url: &'static str| {
            Button::new(id, label)
                .on_click(kit::handler(move |(), _, _| updater::open_url(url)))
                .render(k)
        };
        let links = div()
            .flex()
            .flex_wrap()
            .gap(px(8.0))
            .child(link(k, "about-github", tr("about-github"), REPO_URL))
            .child(link(
                k,
                "about-releases",
                tr("about-releases"),
                RELEASES_URL,
            ))
            .child(link(k, "about-issues", tr("about-issues"), ISSUES_URL))
            .into_any_element();
        let resources = kit::card_of(k, |k| {
            vec![
                Row::new("about-links", tr("about-links"))
                    .description(k, tr("about-links-description"))
                    .trailing(links)
                    .render(k),
            ]
        });

        vec![
            hero,
            kit::section_heading(k, tr("about-section-updates")),
            updates,
            kit::section_heading(k, tr("about-section-links")),
            resources,
        ]
    }
}
