use super::{switch_rect, Align, Layout, Rect, Renderer, Status, Text, Ui, BLACK, MARGIN_X, WHITE};
use kl_core::{
    battery::{Sample, Source, HISTORY_SECS},
    layout::Page,
    Usb,
};
use vello_cpu::kurbo::{self, Affine, Shape};

impl Renderer {
    pub(super) fn settings_pager(&mut self, layout: Layout, index: usize, pages: usize) {
        if pages <= 1 {
            return;
        }
        let [previous, next] = layout.settings_nav();
        if index > 0 {
            self.button(previous, "Previous", false);
        }
        if index + 1 < pages {
            self.button(next, "Next", false);
        }
        self.text(Text {
            text: &format!("{} of {pages}", index + 1),
            x: previous.right() as f32,
            y: (previous.y + 20) as f32,
            w: (next.x - previous.right()) as f32,
            size: 29.0,
            bold: false,
            align: Align::Center,
            lines: 1,
            gray: 85,
        });
    }

    pub(super) fn settings_visibility(&mut self, ui: &Ui) {
        let layout = ui.layout;
        let apps: Vec<_> = ui.visible_visibility_apps().collect();
        if apps.is_empty() {
            let r = layout.list_row(0);
            self.text(Text {
                text: "Apps that can be hidden will appear here.",
                x: (r.x + 24) as f32,
                y: (r.y + 25) as f32,
                w: (r.w - 48) as f32,
                size: 34.0,
                bold: false,
                align: Align::Start,
                lines: 2,
                gray: BLACK,
            });
            return;
        }
        let mut card = layout.list_row(0);
        card.h *= apps.len() as i32;
        self.card(card);
        for (i, app) in apps.iter().enumerate() {
            let r = layout.list_row(i);
            self.text(Text {
                text: &app.name,
                x: (r.x + 32) as f32,
                y: (r.y + 22) as f32,
                w: (r.w - 240) as f32,
                size: 36.0,
                bold: true,
                align: Align::Start,
                lines: 1,
                gray: BLACK,
            });
            self.text(Text {
                text: if app.hidden {
                    "Hidden"
                } else {
                    "On the home screen"
                },
                x: (r.x + 32) as f32,
                y: (r.y + 69) as f32,
                w: (r.w - 240) as f32,
                size: 26.0,
                bold: false,
                align: Align::Start,
                lines: 1,
                gray: 85,
            });
            self.switch(switch_rect(r), !app.hidden);
            if i + 1 < apps.len() {
                self.hairline(r.bottom() - 1, card.x + 32, card.right() - 32);
            }
        }
        self.settings_pager(layout, ui.visibility_index, ui.visibility_pages());
    }

    pub(super) fn level_meter(&mut self, row: Rect, level: u64, total: u64) {
        if total == 0 {
            return;
        }
        let width = (row.w / 4).min(220);
        let bar = Rect::new(
            row.right() - 44 - width,
            row.y + (row.h - 24) / 2,
            width,
            24,
        );
        self.rounded_frame(bar, 12.0, 2, BLACK, WHITE);
        let inner = bar.inset(4);
        let filled = (inner.w as f64 * level.min(total) as f64 / total as f64).round() as i32;
        if filled > 0 {
            self.rounded(
                Rect::new(inner.x, inner.y, filled, inner.h),
                f64::from(filled.min(inner.h)) / 2.0,
                BLACK,
            );
        }
    }

    pub(super) fn battery_history(&mut self, layout: Layout, status: &Status) {
        let y = layout.page_end(Page::Battery);
        let card = Rect::new(
            MARGIN_X,
            y + 52,
            layout.width - 2 * MARGIN_X,
            (layout.height - y - 156).min(390),
        );
        if card.h < 200 {
            return;
        }
        self.section_header(card.x, y, "Battery level over the last 24 hours");
        self.card(card);
        let start = status.battery_now.saturating_sub(HISTORY_SECS);
        let entries: Vec<_> = status
            .battery_history
            .iter()
            .filter(|item| (start..=status.battery_now).contains(&item.observed_at))
            .collect();
        let inset = card.inset(28);
        if entries.len() < 2 {
            self.history_text(inset, "Collecting history", 34.0, BLACK);
            self.history_text(
                Rect::new(inset.x, inset.y + 56, inset.w, 36),
                "Readings are saved while Launcher is awake.",
                27.0,
                85,
            );
            return;
        }
        let plot = Rect::new(card.x + 96, card.y + 34, card.w - 134, card.h - 155);
        self.frame(plot, 2, 195);
        self.history_text(Rect::new(inset.x, plot.y - 14, 64, 35), "100%", 22.0, 85);
        self.history_text(
            Rect::new(inset.x, plot.bottom() - 16, 64, 35),
            "0%",
            22.0,
            85,
        );
        let point = |item: &Sample| {
            let x = f64::from(plot.x)
                + f64::from(plot.w) * (item.observed_at - start) as f64 / HISTORY_SECS as f64;
            let y = f64::from(plot.bottom())
                - f64::from(plot.h) * f64::from(item.percent.min(100)) / 100.0;
            kurbo::Point::new(x, y)
        };
        for (i, item) in entries.iter().enumerate() {
            let end = point(item);
            if i > 0 && !item.gap_before {
                let previous = entries[i - 1];
                let begin = point(previous);
                self.ctx.set_transform(Affine::IDENTITY);
                self.ctx.set_stroke(kurbo::Stroke::new(4.0));
                self.ctx.set_paint(super::gray(BLACK));
                self.ctx
                    .stroke_path(&kurbo::Line::new(begin, end).to_path(0.1));
                if previous.source == Source::Charging {
                    self.fill(
                        Rect::new(
                            begin.x as i32,
                            plot.bottom() + 9,
                            ((end.x - begin.x) as i32).max(3),
                            7,
                        ),
                        BLACK,
                    );
                }
            }
            self.fill(Rect::new(end.x as i32 - 3, end.y as i32 - 3, 6, 6), BLACK);
        }
        for (label, align) in [("24 hours ago", Align::Start), ("Now", Align::End)] {
            self.text(Text {
                text: label,
                x: plot.x as f32,
                y: (plot.bottom() + 19) as f32,
                w: plot.w as f32,
                size: 23.0,
                bold: false,
                align,
                lines: 1,
                gray: 85,
            });
        }
        let description = entries
            .iter()
            .rev()
            .find(|item| item.source == Source::Charging)
            .map_or_else(
                || "Gaps show periods without readings.".into(),
                |item| {
                    let minutes = (status.battery_now - item.observed_at) / 60;
                    match minutes {
                        0 => "Charging observed just now".into(),
                        1..=59 => format!("Charging observed {minutes} min ago"),
                        _ => format!("Charging observed {} hr ago", minutes / 60),
                    }
                },
            );
        self.history_text(
            Rect::new(inset.x, card.bottom() - 57, inset.w, 35),
            &description,
            25.0,
            85,
        );
    }

    fn history_text(&mut self, rect: Rect, text: &str, size: f32, gray: u8) {
        self.text(Text {
            text,
            x: rect.x as f32,
            y: rect.y as f32,
            w: rect.w as f32,
            size,
            bold: false,
            align: Align::Start,
            lines: 1,
            gray,
        });
    }
}

pub(super) fn battery_status(status: &Status) -> String {
    let level = status
        .power
        .percent
        .map_or_else(|| "Level unavailable".into(), |value| format!("{value}%"));
    format!("{level} · {}", status.power.source.label())
}

pub(super) fn storage_status(ui: &Ui, status: &Status) -> String {
    if ui.usb == Usb::Drive {
        return "Kindle drive is connected to a computer".into();
    }
    let Some((used, total)) = status.system.storage.filter(|(_, total)| *total != 0) else {
        return "Storage unavailable".into();
    };
    let gb = |bytes: u64| bytes as f64 / 1_000_000_000.0;
    format!(
        "{:.1} of {:.1} GB used · {:.1} GB free",
        gb(used),
        gb(total),
        gb(total.saturating_sub(used))
    )
}

pub(super) fn usb_status(usb: Usb) -> &'static str {
    match usb {
        Usb::Unplugged => "Unplugged",
        Usb::Charging => "Cable connected",
        Usb::Drive => "Kindle drive connected",
    }
}
