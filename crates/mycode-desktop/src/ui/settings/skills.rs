//! The Skills settings page: discovered slash-command skills from the
//! project and user `.agents` trees, one slim row per skill.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, div, px,
};

use super::widgets::settings_card;
use crate::i18n::t;
use crate::ui::skin;
use crate::workspace::Workspace;

pub(super) fn render_skills_section(workspace: &Workspace, cx: &Context<Workspace>) -> AnyElement {
    let theme: &Theme = cx.theme();
    let skills = &workspace.vm().skills;
    let preferences = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| &settings.skills);
    let muted = theme.muted_foreground;
    let mono = theme.mono_font_family.clone();
    let rows: Vec<AnyElement> = if skills.is_empty() {
        vec![
            div()
                .text_xs()
                .text_color(muted)
                .whitespace_normal()
                .child(t(
                    "No skills yet. magpie's Library installs SKILL.md folders under \
                     ~/.mycode/skills/.",
                    "还没有技能。magpie 的 Library 会把 SKILL.md 装到 ~/.mycode/skills/ 下。",
                ))
                .into_any_element(),
        ]
    } else {
        skills
            .iter()
            .map(|skill| {
                let slug = skill.slug.clone();
                let enabled = preferences.is_none_or(|p| p.is_enabled(&skill.slug));
                let scope = if skill.global {
                    t("user", "用户")
                } else {
                    t("workspace", "工作区")
                };
                div()
                    .id(format!("skill-row-{}", skill.slug))
                    .w_full()
                    .h(px(32.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .rounded(skin::radius_control())
                    .when(!enabled, |this| this.opacity(0.55))
                    .hover(|this| this.bg(skin::frost_hover(theme)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_sm()
                                    .font_family(mono.clone())
                                    .child(format!("/{}", skill.slug)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{} · {} · {}", skill.title, scope, skill.path)),
                            ),
                    )
                    .when(enabled, |this| {
                        let slug = slug.clone();
                        this.child(
                            Button::new(format!("skill-use-{}", skill.slug))
                                .label(t("Use", "使用"))
                                .small()
                                .outline()
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_use_skill(&slug, cx);
                                })),
                        )
                    })
                    .child(
                        Switch::new(format!("skill-enabled-{}", skill.slug))
                            .checked(enabled)
                            .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                                workspace.on_skill_enabled(&slug, *checked, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect()
    };
    div()
        .id("skills-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div().flex().flex_row().items_center().justify_end().child(
                Button::new("skills-refresh")
                    .icon(IconName::RefreshCw)
                    .label(t("Refresh", "刷新"))
                    .small()
                    .outline()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_refresh_skills(cx);
                    })),
            ),
        )
        .child(settings_card(
            "skills-catalog",
            t("Slash commands", "斜杠命令"),
            Some(t(
                "Type / in the composer to insert a skill. Skills come from magpie's \
                 Library (~/.mycode/skills/). A switched-off skill is left out of \
                 the / menu and the agent's skill list.",
                "在输入框输入 / 即可插入技能。技能来自 magpie 的 Library（~/.mycode/skills/）。\
                 关掉的技能不会出现在 / 菜单和智能体的技能列表里。",
            )),
            theme,
            rows,
        ))
        .into_any_element()
}
