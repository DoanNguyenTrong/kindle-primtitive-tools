//! Wrapped multiline editing based on the shared keyboard's cursor and key handling.

use std::ops::Range;

use super::{Canvas, Editor, Key, Press, Rect, TextStyle, BLACK};

pub struct Multiline {
    pub editor: Editor,
    pub first_line: usize,
}

impl Multiline {
    pub fn new(text: &str, max: usize) -> Self {
        let mut editor = Editor::new(max);
        // The cap bounds future edits without truncating notes created outside the app.
        editor.text = text.chars().collect();
        Self {
            editor,
            first_line: 0,
        }
    }

    pub fn press(&mut self, key: Key) -> Press {
        self.editor
            .press(if key == Key::Go { Key::Char('\n') } else { key })
    }

    pub fn lines(&self, columns: usize) -> Vec<Range<usize>> {
        wrap(&self.editor.text, columns)
    }

    pub fn show_caret(&mut self, columns: usize, visible: usize) {
        let lines = self.lines(columns);
        let row = line_at(&lines, self.editor.caret);
        if row < self.first_line {
            self.first_line = row;
        } else if row >= self.first_line.saturating_add(visible.max(1)) {
            self.first_line = row + 1 - visible.max(1);
        }
    }

    pub fn page(&mut self, delta: isize, columns: usize, visible: usize) {
        let max = self.lines(columns).len().saturating_sub(visible.max(1));
        self.first_line = self
            .first_line
            .saturating_add_signed(delta.saturating_mul(visible.max(1) as isize))
            .min(max);
    }

    pub fn place(&mut self, row: usize, column: usize, columns: usize) {
        if let Some(line) = self.lines(columns).get(self.first_line.saturating_add(row)) {
            self.editor.caret = line.start.saturating_add(column).min(line.end);
        }
    }

    pub fn draw(
        &self,
        canvas: &mut Canvas,
        area: Rect,
        columns: usize,
        line_height: i32,
        size: i32,
    ) {
        if line_height <= 0 {
            return;
        }
        let lines = self.lines(columns);
        let caret_line = line_at(&lines, self.editor.caret);
        for (index, line) in lines
            .iter()
            .enumerate()
            .skip(self.first_line)
            .take((area.h / line_height).max(0) as usize)
        {
            let y = area.y + (index - self.first_line) as i32 * line_height;
            let content: String = self.editor.text[line.clone()].iter().collect();
            canvas.text(
                Rect::new(area.x, y, area.w, line_height),
                &content,
                TextStyle::new(size),
            );
            if index == caret_line {
                let prefix: String = self.editor.text[line.start..self.editor.caret.min(line.end)]
                    .iter()
                    .collect();
                let caret = canvas.measure(&prefix, size).min(area.w - 3).max(0);
                canvas.fill(Rect::new(area.x + caret, y + 3, 3, line_height - 6), BLACK);
            }
        }
    }
}

fn wrap(text: &[char], columns: usize) -> Vec<Range<usize>> {
    let columns = columns.max(1);
    let mut lines = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = start.saturating_add(columns).min(text.len());
        let slice = &text[start..end];
        if let Some(offset) = slice.iter().position(|&c| c == '\n') {
            lines.push(start..start + offset);
            start += offset + 1;
        } else {
            let split = if end < text.len() && text[end] != '\n' {
                slice
                    .iter()
                    .rposition(|c| c.is_whitespace())
                    .map_or(end, |pos| start + pos + 1)
            } else {
                end
            };
            lines.push(start..split);
            start = split;
            if text.get(start) == Some(&'\n') {
                start += 1;
            }
        }
    }
    if text.is_empty() || text.last() == Some(&'\n') {
        lines.push(start..start);
    }
    lines
}

fn line_at(lines: &[Range<usize>], caret: usize) -> usize {
    lines
        .partition_point(|line| line.start <= caret)
        .saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_unicode_and_keeps_explicit_blank_lines_and_end_caret() {
        let mut editor = Multiline::new("éa bcd\n\nnew\n", 100);
        assert_eq!(editor.lines(5), vec![0..3, 3..6, 7..7, 8..11, 12..12]);
        editor.editor.caret = 12;
        editor.show_caret(5, 2);
        assert_eq!(editor.first_line, 3);
        editor.place(0, 2, 5);
        assert_eq!(editor.editor.caret, 10);
        editor.press(Key::Go);
        assert_eq!(editor.editor.string(), "éa bcd\n\nne\nw\n");
    }
}
