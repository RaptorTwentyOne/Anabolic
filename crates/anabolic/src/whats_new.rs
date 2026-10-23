//! What changed since the version that ran last, shown once in a small
//! window the first time a new version starts. It comes from CHANGELOG.md,
//! carried inside the executable: nothing is fetched for it.

use anabolic_core::update::newer;
use eframe::egui::{
    self, Align, FontId, Id, Label, Layout, Modal, ScrollArea, TextFormat, text::LayoutJob,
};

use crate::{theme, widgets};

const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// One release's part of the changelog.
#[derive(Debug, PartialEq)]
pub struct Release {
    /// "0.2.1": the heading's first word.
    pub version: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, PartialEq)]
pub enum Block {
    Text(String),
    /// A list item, without its "- ".
    Item(String),
}

/// The releases newer than `last` up to `current`, newest first. Coming
/// from a version before Anabolic remembered it (`last` None): only
/// `current`'s.
pub fn since(last: Option<&str>, current: &str) -> Vec<Release> {
    since_in(CHANGELOG, last, current)
}

fn since_in(changelog: &str, last: Option<&str>, current: &str) -> Vec<Release> {
    parse(changelog)
        .into_iter()
        .filter(|r| {
            // "Unreleased" waits for its version.
            r.version.starts_with(|c: char| c.is_ascii_digit())
                && !newer(&r.version, current)
                && last.map_or(r.version == current, |last| newer(&r.version, last))
        })
        .collect()
}

/// The `## ` sections of a changelog, with their paragraphs and list items.
/// Lines indented under an item continue it.
fn parse(text: &str) -> Vec<Release> {
    fn flush(release: &mut Option<Release>, block: &mut Option<Block>) {
        if let (Some(release), Some(block)) = (release.as_mut(), block.take()) {
            release.blocks.push(block);
        }
    }
    let mut releases = Vec::new();
    let mut release: Option<Release> = None;
    let mut block: Option<Block> = None;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            flush(&mut release, &mut block);
            releases.extend(release.take());
            release = Some(Release {
                version: heading.split_whitespace().next().unwrap_or_default().into(),
                blocks: Vec::new(),
            });
        } else if line.trim().is_empty() || line.starts_with('#') {
            flush(&mut release, &mut block);
        } else if let Some(item) = line.strip_prefix("- ") {
            flush(&mut release, &mut block);
            block = Some(Block::Item(item.trim().into()));
        } else {
            match &mut block {
                Some(Block::Text(text) | Block::Item(text)) => {
                    text.push(' ');
                    text.push_str(line.trim());
                }
                None => block = Some(Block::Text(line.trim().into())),
            }
        }
    }
    flush(&mut release, &mut block);
    releases.extend(release);
    releases
}

/// A line of the changelog as text: **bold**, *italic*, `code`, and links
/// as their words.
fn inline(text: &str) -> LayoutJob {
    #[derive(Clone, Copy, Default)]
    struct Style {
        bold: bool,
        italic: bool,
        code: bool,
    }
    fn push(job: &mut LayoutJob, run: &mut String, style: Style) {
        if run.is_empty() {
            return;
        }
        let font = if style.code {
            FontId::monospace(theme::BODY - 1.0)
        } else if style.bold {
            theme::semibold(theme::BODY)
        } else {
            FontId::proportional(theme::BODY)
        };
        let mut format = TextFormat::simple(font, theme::TEXT);
        format.italics = style.italic;
        job.append(&std::mem::take(run), 0.0, format);
    }
    let mut job = LayoutJob::default();
    let mut run = String::new();
    let mut style = Style::default();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix('`') {
            push(&mut job, &mut run, style);
            style.code = !style.code;
            rest = after;
        } else if style.code {
            run.push(c);
            rest = &rest[c.len_utf8()..];
        } else if let Some(after) = rest.strip_prefix("**") {
            push(&mut job, &mut run, style);
            style.bold = !style.bold;
            rest = after;
        } else if let Some(after) = rest.strip_prefix('*') {
            push(&mut job, &mut run, style);
            style.italic = !style.italic;
            rest = after;
        } else if let Some(link) = rest.strip_prefix('[')
            && let Some(middle) = link.find("](")
            && let Some(end) = link[middle..].find(')')
        {
            run.push_str(&link[..middle]);
            rest = &link[middle + end + 1..];
        } else {
            run.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    push(&mut job, &mut run, style);
    job
}

/// The window. False once it is closed; `all` turns true for the releases
/// page.
pub fn show(ctx: &egui::Context, releases: &[Release], all: &mut bool) -> bool {
    let mut close = false;
    let response = Modal::new(Id::new("whats-new")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.horizontal(|ui| {
            ui.label(widgets::strong(
                format!("What's new in Anabolic {}", env!("CARGO_PKG_VERSION")),
                theme::SUBTITLE,
            ));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button(widgets::icon(theme::icon::CLOSE, 12.0))
                    .on_hover_text("Close (Esc)")
                    .clicked()
                {
                    close = true;
                }
            });
        });
        ui.add_space(8.0);
        ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for (i, release) in releases.iter().enumerate() {
                // More than one release since the last start: say which.
                if releases.len() > 1 {
                    if i > 0 {
                        ui.add_space(8.0);
                    }
                    ui.label(
                        widgets::strong(&release.version, theme::BODY).color(theme::TEXT_WEAK),
                    );
                    ui.add_space(2.0);
                }
                for block in &release.blocks {
                    match block {
                        Block::Text(text) => {
                            ui.add(Label::new(inline(text)).wrap());
                        }
                        Block::Item(text) => {
                            ui.horizontal_top(|ui| {
                                ui.label(egui::RichText::new("•").color(theme::ACCENT));
                                ui.add(Label::new(inline(text)).wrap());
                            });
                        }
                    }
                    ui.add_space(4.0);
                }
            }
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.link("Every release").clicked() {
                *all = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    });
    !(close || response.should_close())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Changelog\n\
        \n\
        Intro that belongs to no release.\n\
        \n\
        ## Unreleased\n\
        \n\
        - Not out yet.\n\
        \n\
        ## 0.3.0 (2026-11-01)\n\
        \n\
        A summary\n\
        over two lines.\n\
        \n\
        - **One:** first\n  \
          continued.\n\
        - Two.\n\
        \n\
        ## 0.2.1 (2026-10-20)\n\
        \n\
        - Three.\n\
        \n\
        ## 0.2.0 (2026-10-20)\n\
        \n\
        - Four.\n";

    #[test]
    fn reads_releases_paragraphs_and_items() {
        let releases = parse(SAMPLE);
        let versions: Vec<_> = releases.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, ["Unreleased", "0.3.0", "0.2.1", "0.2.0"]);
        assert_eq!(
            releases[1].blocks,
            [
                Block::Text("A summary over two lines.".into()),
                Block::Item("**One:** first continued.".into()),
                Block::Item("Two.".into()),
            ]
        );
    }

    #[test]
    fn shows_what_is_new_since_the_last_start() {
        let versions = |last, current| -> Vec<String> {
            since_in(SAMPLE, last, current)
                .into_iter()
                .map(|r| r.version)
                .collect()
        };
        assert_eq!(versions(Some("0.2.0"), "0.3.0"), ["0.3.0", "0.2.1"]);
        assert_eq!(versions(Some("0.2.1"), "0.3.0"), ["0.3.0"]);
        assert!(versions(Some("0.3.0"), "0.3.0").is_empty());
        // From before Anabolic remembered: only this version's.
        assert_eq!(versions(None, "0.2.1"), ["0.2.1"]);
        // Built from a newer source than the changelog knows: nothing.
        assert!(versions(None, "0.4.0").is_empty());
    }

    #[test]
    fn formatting_becomes_plain_words() {
        let job = inline("**Bold:** a `--tray` and *this* [page](https://example.invalid).");
        assert_eq!(job.text, "Bold: a --tray and this page.");
        assert_eq!(job.sections.len(), 6);
    }

    /// Every release in CHANGELOG.md has a version and something to say,
    /// and this version is among them: a release without its part would
    /// show nothing new.
    #[test]
    fn the_changelog_covers_this_version() {
        let releases = parse(CHANGELOG);
        for r in &releases {
            assert!(!r.blocks.is_empty(), "{} says nothing", r.version);
        }
        let current = env!("CARGO_PKG_VERSION");
        assert!(
            releases.iter().any(|r| r.version == current),
            "CHANGELOG.md has no part for {current}: name its Unreleased part after it"
        );
    }
}
