//! The Skills settings page: discovered slash-command skills from the
//! project and user `.agents` trees.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, div};

use super::widgets::settings_card;
use crate::i18n::t;
use crate::ui::skin;
use crate::workspace::Workspace;

pub(super) fn render_skills_section(workspace: &Workspace, cx: &Context<Workspace>) -> AnyElement {
    let theme: &Theme = cx.theme();
    let skills = workspace.vm().skills.clone();
    let preferences = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.skills.clone())
        .unwrap_or_default();
    let rows: Vec<AnyElement> = if skills.is_empty() {
        vec![
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(t(
                    "No skills yet. Add SKILL.md under the project .agents/skills/ folder \
                     or ~/.agents/skills/.",
                    "还没有技能。在项目 .agents/skills/ 或 ~/.agents/skills/ 下添加 SKILL.md。",
                ))
                .into_any_element(),
        ]
    } else {
        skills
            .into_iter()
            .map(|skill| {
                let slug = skill.slug.clone();
                let enabled = preferences.is_enabled(&skill.slug);
                let scope = if skill.global {
                    t("user", "用户")
                } else {
                    t("workspace", "工作区")
                };
                div()
                    .id(format!("skill-row-{}", skill.slug))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .p_3()
                    .rounded(skin::radius_control())
                    .border_1()
                    .border_color(skin::glass_border(theme))
                    .bg(skin::frost(theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .when(!enabled, |this| this.opacity(0.55))
                            .child(
                                div()
                                    .text_sm()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(format!("/{}", skill.slug)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.foreground)
                                    .opacity(0.85)
                                    .whitespace_normal()
                                    .child(skill.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .whitespace_normal()
                                    .child(format!("{scope} · {}", skill.path)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
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
                                    .on_click(cx.listener(
                                        move |workspace, checked: &bool, _, cx| {
                                            workspace.on_skill_enabled(&slug, *checked, cx);
                                        },
                                    )),
                            ),
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
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
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
                "Type / in the composer to insert a skill. Workspace .agents/skills win \
                 over the same slug in ~/.agents. A switched-off skill is left out of \
                 the / menu and the agent's skill list.",
                "在输入框输入 / 即可插入技能。工作区 .agents/skills 中的同名技能优先于 \
                 ~/.agents。关掉的技能不会出现在 / 菜单和智能体的技能列表里。",
            )),
            theme,
            rows,
        ))
        .into_any_element()
}
