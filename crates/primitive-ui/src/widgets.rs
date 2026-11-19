use crate::{
    screen_rect, Canvas, Icon, Rect, TextStyle, BLACK, BODY, CAPTION, HAIRLINE, PAPER, SECONDARY,
    TITLE,
};

const SECTION_INK: u8 = 60;
const CHEVRON: Icon = Icon {
    stroke: &["M9 5 L16 12 L9 19"],
    fill: &[],
};

pub fn section_header(canvas: &mut Canvas, bounds: Rect, title: &str) {
    let inset = canvas.px(8);
    canvas.text(
        Rect::new(
            bounds.x + inset,
            bounds.y + inset,
            bounds.w - 2 * inset,
            bounds.h - inset,
        ),
        title,
        TextStyle::new(26).bold().ink(SECTION_INK),
    );
}

pub fn chevron(canvas: &mut Canvas, bounds: Rect, ink: u8) {
    canvas.icon(bounds, CHEVRON, ink);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowAccessory {
    #[default]
    None,
    Disclosure,
    Toggle(bool),
}

pub struct Row<'a> {
    pub title: &'a str,
    pub detail: Option<&'a str>,
    pub accessory: RowAccessory,
}

/// A grouped list. The card bounds and row count define both drawing and touch regions.
pub struct Rows<'a> {
    pub bounds: Rect,
    pub rows: &'a [Row<'a>],
}

impl Rows<'_> {
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        split(self.bounds.y, self.bounds.h, self.rows.len(), index)
            .map(|(y, height)| Rect::new(self.bounds.x, y, self.bounds.w, height))
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        (0..self.rows.len()).find(|&index| self.row_rect(index).is_some_and(|r| r.contains(x, y)))
    }

    pub fn draw(&self, canvas: &mut Canvas, pressed: Option<usize>) {
        if self.rows.is_empty() {
            return;
        }
        canvas.card(self.bounds);
        let pad = canvas.px(32);
        for (index, row) in self.rows.iter().enumerate() {
            let Some(rect) = self.row_rect(index) else {
                continue;
            };
            let active = canvas.control_pressed(rect, pressed == Some(index));
            if active {
                canvas.panel(rect.inset(canvas.px(6)), 16, 2, BLACK);
            }
            let ink = if active { PAPER } else { BLACK };
            let detail_ink = if active { 255 - SECONDARY } else { SECONDARY };
            let accessory_width = match row.accessory {
                RowAccessory::None => 0,
                RowAccessory::Disclosure => canvas.px(56),
                RowAccessory::Toggle(_) => canvas.px(136),
            };
            let title_height = canvas.px(50);
            let title_y = match row.detail {
                Some(_) => rect.y + canvas.px(17),
                None => rect.y + (rect.h - title_height) / 2,
            };
            let text_width = rect.w - 2 * pad - accessory_width;
            canvas.text(
                Rect::new(rect.x + pad, title_y, text_width, title_height),
                row.title,
                TextStyle::new(38).bold().ink(ink),
            );
            if let Some(detail) = row.detail {
                canvas.text(
                    Rect::new(
                        rect.x + pad,
                        title_y + title_height,
                        text_width,
                        canvas.px(40),
                    ),
                    detail,
                    CAPTION.ink(detail_ink),
                );
            }
            match row.accessory {
                RowAccessory::None => {}
                RowAccessory::Disclosure => {
                    let size = canvas.px(40);
                    let accessory = Rect::new(
                        rect.right() - pad - size,
                        rect.y + (rect.h - size) / 2,
                        size,
                        size,
                    );
                    chevron(canvas, accessory, ink);
                }
                RowAccessory::Toggle(enabled) => {
                    let (width, height) = (canvas.px(112), canvas.px(64));
                    let accessory = Rect::new(
                        rect.right() - pad - width,
                        rect.y + (rect.h - height) / 2,
                        width,
                        height,
                    );
                    if active {
                        canvas.circle(accessory.inset(-canvas.px(2)), PAPER);
                    }
                    crate::switch(canvas, accessory, enabled);
                }
            }
            if index + 1 < self.rows.len() {
                let line = canvas.px(2).max(1);
                canvas.fill(
                    Rect::new(rect.x + pad, rect.bottom() - line, rect.w - 2 * pad, line),
                    HAIRLINE,
                );
            }
        }
    }
}

pub struct Tab<'a> {
    pub label: &'a str,
    pub icon: Option<Icon>,
}

/// A floating tab bar. A tab may use one of the shared 24-unit icons.
pub struct TabBar<'a> {
    pub bounds: Rect,
    pub tabs: &'a [Tab<'a>],
    pub selected: usize,
}

impl TabBar<'_> {
    pub fn tab_rect(&self, index: usize) -> Option<Rect> {
        split(self.bounds.x, self.bounds.w, self.tabs.len(), index)
            .map(|(x, width)| Rect::new(x, self.bounds.y, width, self.bounds.h))
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        (0..self.tabs.len()).find(|&index| self.tab_rect(index).is_some_and(|r| r.contains(x, y)))
    }

    pub fn draw(&self, canvas: &mut Canvas, pressed: Option<usize>) {
        if self.tabs.is_empty() {
            return;
        }
        canvas.pill(self.bounds, 3, PAPER);
        for (index, tab) in self.tabs.iter().enumerate() {
            let Some(rect) = self.tab_rect(index) else {
                continue;
            };
            let inset = rect.inset(canvas.px(8));
            let active =
                (index == self.selected) != canvas.control_pressed(rect, pressed == Some(index));
            if active {
                canvas.circle(inset, BLACK);
            }
            let ink = if active { PAPER } else { BLACK };
            let y = if let Some(icon) = tab.icon {
                let size = canvas.px(40);
                let icon_rect = Rect::new(
                    rect.x + (rect.w - size) / 2,
                    rect.y + canvas.px(23),
                    size,
                    size,
                );
                canvas.icon(icon_rect, icon, ink);
                rect.y + canvas.px(69)
            } else {
                rect.y + (rect.h - canvas.px(34)) / 2
            };
            canvas.text(
                Rect::new(inset.x, y, inset.w, rect.bottom() - y),
                tab.label,
                TextStyle::new(24).bold().center().ink(ink),
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageTurn {
    Previous,
    Next,
}

/// Previous and Next controls for a zero-based page index.
pub struct Pager {
    pub bounds: Rect,
    pub page: usize,
    pub pages: usize,
}

impl Pager {
    pub fn button(&self, direction: PageTurn) -> Option<Rect> {
        if self.page >= self.pages {
            return None;
        }
        let width = self.bounds.w / 4;
        let x = match direction {
            PageTurn::Previous if self.page > 0 => self.bounds.x,
            PageTurn::Next if self.page < self.pages - 1 => self.bounds.right() - width,
            _ => return None,
        };
        Some(Rect::new(x, self.bounds.y, width, self.bounds.h))
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<PageTurn> {
        [PageTurn::Previous, PageTurn::Next]
            .into_iter()
            .find(|&turn| self.button(turn).is_some_and(|r| r.contains(x, y)))
    }

    pub fn draw(&self, canvas: &mut Canvas, pressed: Option<PageTurn>) {
        if self.pages < 2 || self.page >= self.pages {
            return;
        }
        for (turn, label) in [(PageTurn::Previous, "Previous"), (PageTurn::Next, "Next")] {
            if let Some(button) = self.button(turn) {
                canvas.button(button, label, false, pressed == Some(turn));
            }
        }
        let width = self.bounds.w / 2;
        let y = self.bounds.y + (self.bounds.h - canvas.px(40)) / 2;
        canvas.text(
            Rect::new(
                self.bounds.x + (self.bounds.w - width) / 2,
                y,
                width,
                canvas.px(40),
            ),
            &format!("{} of {}", self.page + 1, self.pages),
            CAPTION.center(),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetAction {
    Cancel,
    Confirm,
}

/// A two-action sheet over the current screen.
pub struct Sheet<'a> {
    bounds: Rect,
    screen_width: i32,
    title: &'a str,
    detail: &'a str,
    confirm: &'a str,
}

impl<'a> Sheet<'a> {
    pub fn centered(screen: Rect, title: &'a str, detail: &'a str, confirm: &'a str) -> Self {
        let design = screen_rect(screen.w, 0, 0, 864, 420);
        Self {
            bounds: Rect::new(
                screen.x + (screen.w - design.w) / 2,
                screen.y + (screen.h - design.h) / 2,
                design.w,
                design.h,
            ),
            screen_width: screen.w,
            title,
            detail,
            confirm,
        }
    }

    pub fn button(&self, action: SheetAction) -> Rect {
        let margin_gap = screen_rect(self.screen_width, 48, 24, 0, 100);
        let (side, gap, height) = (margin_gap.x, margin_gap.y, margin_gap.h);
        let width = (self.bounds.w - 2 * side - gap) / 2;
        let x = match action {
            SheetAction::Cancel => self.bounds.x + side,
            SheetAction::Confirm => self.bounds.right() - side - width,
        };
        Rect::new(x, self.bounds.bottom() - side - height, width, height)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<SheetAction> {
        if !self.bounds.contains(x, y) || self.button(SheetAction::Cancel).contains(x, y) {
            Some(SheetAction::Cancel)
        } else if self.button(SheetAction::Confirm).contains(x, y) {
            Some(SheetAction::Confirm)
        } else {
            None
        }
    }

    pub fn draw(&self, canvas: &mut Canvas, pressed: Option<SheetAction>) {
        canvas.popup(self.bounds);
        let pad = canvas.px(48);
        let inner = self.bounds.inset(pad);
        canvas.text(
            Rect::new(inner.x, inner.y, inner.w, canvas.px(65)),
            self.title,
            TextStyle::new(50).bold(),
        );
        canvas.text(
            Rect::new(
                inner.x,
                inner.y + canvas.px(95),
                inner.w,
                self.button(SheetAction::Cancel).y - inner.y - canvas.px(115),
            ),
            self.detail,
            TextStyle::new(36).ink(SECONDARY).lines(3),
        );
        for (action, label) in [
            (SheetAction::Cancel, "Cancel"),
            (SheetAction::Confirm, self.confirm),
        ] {
            canvas.button(
                self.button(action),
                label,
                action == SheetAction::Confirm,
                pressed == Some(action),
            );
        }
    }
}

pub fn full_screen_message(canvas: &mut Canvas, title: &str, detail: &str) {
    let screen = canvas.bounds();
    let margin = canvas.px(40);
    let bounds = Rect::new(
        screen.x + margin,
        screen.y + screen.h / 3,
        screen.w - 2 * margin,
        canvas.px(240),
    );
    canvas.text(bounds, title, TITLE.center());
    canvas.text(
        Rect::new(
            bounds.x,
            bounds.y + canvas.px(110),
            bounds.w,
            canvas.px(180),
        ),
        detail,
        BODY.center().lines(3),
    );
}

fn split(start: i32, size: i32, count: usize, index: usize) -> Option<(i32, i32)> {
    if index >= count || size <= 0 {
        return None;
    }
    let offset = |index: usize| (i64::from(size) * index as i64 / count as i64) as i32;
    let offset_start = offset(index);
    Some((start + offset_start, offset(index + 1) - offset_start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_tabs_use_the_complete_area_for_hit_testing() {
        let rows = Rows {
            bounds: Rect::new(20, 50, 400, 241),
            rows: &[
                Row {
                    title: "First",
                    detail: None,
                    accessory: RowAccessory::None,
                },
                Row {
                    title: "Second",
                    detail: None,
                    accessory: RowAccessory::Disclosure,
                },
            ],
        };
        assert_eq!(rows.hit(20.0, 169.0), Some(0));
        assert_eq!(rows.hit(20.0, 170.0), Some(1));
        assert_eq!(rows.hit(20.0, 290.0), Some(1));
        assert_eq!(rows.hit(20.0, 291.0), None);
        let tabs = TabBar {
            bounds: Rect::new(20, 50, 401, 130),
            tabs: &[
                Tab {
                    label: "First",
                    icon: None,
                },
                Tab {
                    label: "Second",
                    icon: None,
                },
            ],
            selected: 0,
        };
        assert_eq!(tabs.hit(219.0, 70.0), Some(0));
        assert_eq!(tabs.hit(220.0, 70.0), Some(1));
        assert_eq!(tabs.hit(420.0, 70.0), Some(1));
        assert_eq!(tabs.hit(421.0, 70.0), None);
    }

    #[test]
    fn pager_and_sheet_hit_only_available_actions() {
        let pager = Pager {
            bounds: Rect::new(40, 1440, 1184, 100),
            page: 0,
            pages: 3,
        };
        assert_eq!(pager.hit(80.0, 1450.0), None);
        assert_eq!(pager.hit(1000.0, 1450.0), Some(PageTurn::Next));
        let pager = Pager { page: 2, ..pager };
        assert_eq!(pager.hit(80.0, 1450.0), Some(PageTurn::Previous));
        assert_eq!(pager.hit(1000.0, 1450.0), None);
        let sheet = Sheet::centered(
            Rect::new(0, 0, 1264, 1680),
            "Remove note?",
            "You can restore it from Recently deleted.",
            "Remove",
        );
        let confirm = sheet.button(SheetAction::Confirm);
        assert_eq!(confirm.h, 100);
        assert_eq!(
            sheet.hit(confirm.x as f32, confirm.y as f32),
            Some(SheetAction::Confirm)
        );
        assert_eq!(sheet.hit(100.0, 100.0), Some(SheetAction::Cancel));
        assert_eq!(sheet.hit(250.0, 700.0), None);
    }
}
