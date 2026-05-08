//! Header component for the S-DET commit view: avatar, author, date,
//! parsed message body with mentions.

use std::sync::Arc;

use git::repository::CommitDetails;
use git::{BuildCommitPermalinkParams, GitRemote, ParsedGitRemote};
use gpui::{AnyElement, App, ClipboardItem, IntoElement, ParentElement, Styled, Window};
use ui::{Tooltip, prelude::*};

use crate::commit_tooltip::CommitAvatar;
use crate::git_panel_settings::{CommitViewSettings, GitPanelSettings};
use settings::Settings as _;

use super::mentions::{MessageToken, parse_message_tokens};

/// Render the IDEA-style header for the commit view.
///
/// `gutter_width` is plumbed in so the avatar column lines up with the
/// editor gutter. `extra_committer` is `Some((name, email))` only when
/// the committer differs from the author — the typical clean commit
/// uses the same identity for both, so we hide the second line.
pub(crate) fn render_header(
    commit: &CommitDetails,
    remote: Option<&GitRemote>,
    extra_committer: Option<(SharedString, SharedString)>,
    is_stash: bool,
    gutter_width: gpui::Pixels,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let settings = GitPanelSettings::get_global(cx).commit_view.clone();

    let author_name = commit.author_name.clone();
    let author_email = commit.author_email.clone();
    let commit_sha = commit.sha.clone();

    let commit_date = time::OffsetDateTime::from_unix_timestamp(commit.commit_timestamp)
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let absolute_date = time_format::format_localized_timestamp(
        commit_date,
        time::OffsetDateTime::now_utc(),
        local_offset,
        time_format::TimestampFormat::MediumAbsolute,
    );
    let relative_date = time_format::format_localized_timestamp(
        commit_date,
        time::OffsetDateTime::now_utc(),
        local_offset,
        time_format::TimestampFormat::Relative,
    );

    let clipboard_has_sha = cx
        .read_from_clipboard()
        .and_then(|entry| entry.text())
        .map_or(false, |clipboard_text| {
            clipboard_text.trim() == commit_sha.as_ref()
        });

    let (copy_icon, copy_icon_color) = if clipboard_has_sha {
        (IconName::Check, Color::Success)
    } else {
        (IconName::Copy, Color::Muted)
    };

    let parsed_remote_arc = remote.map(|remote| {
        Arc::new(ParsedGitRemote {
            owner: remote.owner.as_ref().into(),
            repo: remote.repo.as_ref().into(),
        })
    });

    let avatar = render_avatar(
        &commit_sha,
        Some(author_email.clone()),
        remote,
        &author_name,
        &settings,
        window,
        cx,
    );

    let absolute_for_tooltip: SharedString = absolute_date.into();
    let date_button = Button::new("commit-date", relative_date)
        .style(ButtonStyle::Subtle)
        .label_size(LabelSize::Small)
        .color(Color::Muted)
        .tooltip(move |_, cx| Tooltip::simple(absolute_for_tooltip.clone(), cx));

    h_flex()
        .py_2()
        .pr_2p5()
        .w_full()
        .justify_between()
        .border_b_1()
        .border_color(cx.theme().colors().border_variant)
        .child(
            h_flex()
                .gap_2()
                .child(h_flex().w(gutter_width).justify_center().child(avatar))
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(render_message_block(commit, &settings, parsed_remote_arc))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .child(Label::new(author_name).size(LabelSize::Small))
                                .when(!author_email.is_empty(), |this| {
                                    this.child(
                                        Label::new("•")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted)
                                            .alpha(0.5),
                                    )
                                    .child(
                                        Label::new(author_email)
                                            .color(Color::Muted)
                                            .size(LabelSize::Small),
                                    )
                                })
                                .child(
                                    Label::new("•")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .alpha(0.5),
                                )
                                .child(date_button),
                        )
                        .when_some(extra_committer, |this, (name, email)| {
                            this.child(
                                h_flex()
                                    .gap_1p5()
                                    .child(
                                        Label::new("Committed by")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .child(Label::new(name).size(LabelSize::Small))
                                    .when(!email.is_empty(), |this| {
                                        this.child(
                                            Label::new(email)
                                                .size(LabelSize::Small)
                                                .color(Color::Muted),
                                        )
                                    }),
                            )
                        }),
                ),
        )
        .when(!is_stash, |this| {
            this.child(
                Button::new("sha", "Commit SHA")
                    .start_icon(
                        Icon::new(copy_icon)
                            .size(IconSize::Small)
                            .color(copy_icon_color),
                    )
                    .tooltip({
                        let commit_sha = commit_sha.clone();
                        move |_, cx| {
                            Tooltip::with_meta("Copy Commit SHA", None, commit_sha.clone(), cx)
                        }
                    })
                    .on_click({
                        let commit_sha = commit_sha.clone();
                        move |_, _, cx| {
                            cx.stop_propagation();
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                commit_sha.to_string(),
                            ));
                        }
                    }),
            )
        })
        .into_any_element()
}

fn render_avatar(
    sha: &SharedString,
    author_email: Option<SharedString>,
    remote: Option<&GitRemote>,
    author_name: &SharedString,
    settings: &CommitViewSettings,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    if settings.fetch_avatars && remote.is_some() {
        return CommitAvatar::new(sha, author_email, remote)
            .size(rems_from_px(40.))
            .render(window, cx);
    }

    // Privacy-default: don't fetch Gravatar URLs unless explicitly opted in.
    // Render a character tile instead so the layout is identical.
    let initial = author_name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().next().unwrap_or(c))
        .unwrap_or('?');
    let size = rems_from_px(40.).to_pixels(window.rem_size());
    h_flex()
        .size(size)
        .justify_center()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .bg(cx.theme().colors().element_disabled)
        .child(
            Label::new(initial.to_string())
                .size(LabelSize::Default)
                .color(Color::Muted),
        )
        .into_any_element()
}

fn render_message_block(
    commit: &CommitDetails,
    settings: &CommitViewSettings,
    remote: Option<Arc<ParsedGitRemote>>,
) -> AnyElement {
    let raw = commit.message.as_ref().trim_end();
    let mut lines = raw.split('\n');
    let subject = lines.next().unwrap_or("").to_string();
    let body: String = lines.collect::<Vec<_>>().join("\n").trim().to_string();

    let parse_issues = settings.parse_issue_references;
    let subject_tokens = parse_message_tokens(&subject, parse_issues);
    let body_tokens = if body.is_empty() {
        Vec::new()
    } else {
        parse_message_tokens(&body, parse_issues)
    };

    let permalink_for_issue = move |number: &str| -> Option<String> {
        let remote = remote.as_ref()?;
        let host = remote.owner.as_ref();
        let repo = remote.repo.as_ref();
        // Build a generic issue permalink using the cached remote owner /
        // repo and a github-style path. Hosting providers that don't follow
        // this layout simply produce a 404 — we keep the link clickable so
        // the user can override.
        let _ = host;
        let _ = BuildCommitPermalinkParams { sha: "" };
        Some(format!("https://github.com/{}/{}/issues/{}", host, repo, number))
    };

    let mut subject_children: Vec<AnyElement> = Vec::with_capacity(subject_tokens.len());
    for token in subject_tokens {
        subject_children.push(render_token(token, &permalink_for_issue));
    }
    let mut body_children: Vec<AnyElement> = Vec::with_capacity(body_tokens.len());
    for token in body_tokens {
        body_children.push(render_token(token, &permalink_for_issue));
    }

    v_flex()
        .gap_1()
        .child(h_flex().flex_wrap().children(subject_children))
        .when(!body.is_empty(), |this| {
            this.child(h_flex().flex_wrap().children(body_children))
        })
        .into_any_element()
}

fn render_token(
    token: MessageToken,
    permalink_for_issue: &impl Fn(&str) -> Option<String>,
) -> AnyElement {
    match token {
        MessageToken::Text(text) => Label::new(text).into_any_element(),
        MessageToken::Url(url) => {
            let label = url.clone();
            let id = SharedString::from(format!("url-{}", url));
            Button::new(id, label)
                .style(ButtonStyle::Subtle)
                .color(Color::Accent)
                .label_size(LabelSize::Default)
                .on_click(move |_, _, cx| cx.open_url(&url))
                .into_any_element()
        }
        MessageToken::IssueRef(number) => {
            let label = format!("#{}", number);
            let url = permalink_for_issue(&number);
            let mut btn = Button::new(
                SharedString::from(format!("issue-{}", number)),
                label,
            )
            .style(ButtonStyle::Subtle)
            .color(Color::Accent)
            .label_size(LabelSize::Default);
            if let Some(url) = url {
                btn = btn.on_click(move |_, _, cx| cx.open_url(&url));
            }
            btn.into_any_element()
        }
        MessageToken::JiraRef(key) => {
            // No click handler — a Jira target needs configuration that
            // the plan defers. The token still renders styled so the user
            // sees it as a recognised reference.
            Label::new(format!("[{}]", key))
                .color(Color::Accent)
                .into_any_element()
        }
    }
}
