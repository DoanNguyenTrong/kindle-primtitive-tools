//! The on-screen keyboard shared by Primitive Launcher and Primitive Browser: the key layers and their rows, and the
//! text editing that a key press does (characters, the caret, Delete, one-shot capitals, Go).
//!
//! Drawing stays with each app: the browser draws HTML, the launcher draws with vello. Both read the same rows, so a
//! key sits in the same place, with the same label and width, in both.
//!
//! Layers, as Primitive Browser has them: `Lower` (letters, digits on top), `Upper` (the same, capitals; one capital
//! letter, then back to `Lower`), `Symbols` (the "123" symbols) and `More` (the "#+=" symbols). The bottom row has the
//! layer switch, ‹ and › (caret), space, a full stop, and Go.

/// The keyboard's layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Lower,
    Upper,
    Symbols,
    More,
}

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character, typed as it is (the `Upper` layer already has capitals).
    Char(char),
    /// Shift: toggles `Lower` and `Upper`.
    Shift,
    /// Delete the character before the caret.
    Delete,
    /// Move the caret left or right.
    Left,
    Right,
    Space,
    /// The key that finishes the text (the label is given by the app: "Go", "Join", "Connect").
    Go,
    /// Switch to another layer.
    Layer(Layer),
}

/// One key of a row: what it does, its label, its width in units, and whether it is drawn as on (Shift in `Upper`,
/// and Go).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyDef {
    pub key: Key,
    pub label: String,
    pub width: f32,
    pub on: bool,
}

/// The rows of a layer, top to bottom. `go` is the label of the Go key. The widest row of a layer is 11 units (the
/// symbol rows, with ABC and Del); an app scales the width of the screen to the widest row and centres each row.
pub fn rows(layer: Layer, go: &str) -> Vec<Vec<KeyDef>> {
    let up = layer == Layer::Upper;
    let letters = |s: &str| -> Vec<KeyDef> {
        s.chars()
            .map(|c| {
                let c = if up { c.to_ascii_uppercase() } else { c };
                KeyDef { key: Key::Char(c), label: c.to_string(), width: 1.0, on: false }
            })
            .collect()
    };
    let chars = |s: &str| -> Vec<KeyDef> {
        s.chars().map(|c| KeyDef { key: Key::Char(c), label: c.to_string(), width: 1.0, on: false }).collect()
    };
    let key = |key: Key, label: &str, width: f32, on: bool| KeyDef { key, label: label.to_string(), width, on };
    let bottom = |switch: KeyDef| -> Vec<KeyDef> {
        vec![
            switch,
            key(Key::Left, "‹", 1.0, false),
            key(Key::Space, "space", 3.0, false),
            key(Key::Right, "›", 1.0, false),
            key(Key::Char('.'), ".", 1.0, false),
            key(Key::Go, go, 1.5, true),
        ]
    };
    match layer {
        Layer::Lower | Layer::Upper => {
            let mut row3 = vec![key(Key::Shift, "Shift", 1.5, up)];
            row3.extend(letters("zxcvbnm"));
            row3.push(key(Key::Delete, "Del", 1.5, false));
            vec![
                letters("1234567890"),
                letters("qwertyuiop"),
                letters("asdfghjkl"),
                row3,
                bottom(key(Key::Layer(Layer::Symbols), "123", 1.5, false)),
            ]
        }
        Layer::Symbols => {
            let mut row4 = vec![key(Key::Layer(Layer::Lower), "ABC", 1.5, false)];
            row4.extend(chars(".,()?!'\""));
            row4.push(key(Key::Delete, "Del", 1.5, false));
            vec![chars("!@#$%^&*()"), chars("`~_+-=[]{}"), chars("\\|/:;\"<>"), row4, bottom(key(Key::Layer(Layer::More), "#+=", 1.5, false))]
        }
        Layer::More => {
            let mut row4 = vec![key(Key::Layer(Layer::Lower), "ABC", 1.5, false)];
            row4.extend(chars("@&$"));
            row4.push(key(Key::Delete, "Del", 1.5, false));
            vec![chars("[]{}#%^*+="), chars("_\\|~<>€£¥•"), chars(".,?!'\"/:;"), row4, bottom(key(Key::Layer(Layer::Symbols), "123", 1.5, false))]
        }
    }
}

/// The result of a key press, for the app to react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// The text changed.
    Edited,
    /// The caret moved.
    Moved,
    /// The layer changed.
    Layer,
    /// Go was pressed: the app finishes the text.
    Go,
    /// Nothing happened (a full field, a Delete at the start, a Shift that is not a change of text).
    Nothing,
}

/// The text being typed: its characters, the caret, the layer, and the most it may hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub text: Vec<char>,
    pub caret: usize,
    pub layer: Layer,
    pub max: usize,
}

impl Editor {
    pub fn new(max: usize) -> Editor {
        Editor { text: Vec::new(), caret: 0, layer: Layer::Lower, max }
    }

    /// The text as a string.
    pub fn string(&self) -> String {
        self.text.iter().collect()
    }

    /// A key pressed. A character after `Upper` returns to `Lower` (one capital letter at a time).
    pub fn press(&mut self, key: Key) -> Press {
        match key {
            Key::Char(c) => self.insert(c),
            Key::Space => self.insert(' '),
            Key::Delete => {
                if self.caret == 0 {
                    return Press::Nothing;
                }
                self.caret -= 1;
                self.text.remove(self.caret);
                Press::Edited
            }
            Key::Left => {
                if self.caret == 0 {
                    return Press::Nothing;
                }
                self.caret -= 1;
                Press::Moved
            }
            Key::Right => {
                if self.caret >= self.text.len() {
                    return Press::Nothing;
                }
                self.caret += 1;
                Press::Moved
            }
            Key::Shift => {
                self.layer = if self.layer == Layer::Upper { Layer::Lower } else { Layer::Upper };
                Press::Layer
            }
            Key::Layer(layer) => {
                self.layer = layer;
                Press::Layer
            }
            Key::Go => Press::Go,
        }
    }

    fn insert(&mut self, c: char) -> Press {
        if self.text.len() >= self.max {
            return Press::Nothing;
        }
        self.text.insert(self.caret, c);
        self.caret += 1;
        if self.layer == Layer::Upper {
            self.layer = Layer::Lower;
        }
        Press::Edited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(rows: &[Vec<KeyDef>], key: Key) -> bool {
        rows.iter().flatten().any(|k| k.key == key)
    }

    #[test]
    fn the_layers_have_the_browsers_rows() {
        let lower = rows(Layer::Lower, "Go");
        assert_eq!(lower.len(), 5, "four rows of keys and the bottom row");
        assert_eq!(lower[0].iter().map(|k| k.label.as_str()).collect::<String>(), "1234567890");
        assert_eq!(lower[1].iter().map(|k| k.label.as_str()).collect::<String>(), "qwertyuiop");
        assert!(find(&lower, Key::Shift) && find(&lower, Key::Go) && find(&lower, Key::Space));
        assert_eq!(rows(Layer::Symbols, "Go")[4][0].key, Key::Layer(Layer::More));
    }

    #[test]
    fn upper_has_capitals_and_go_takes_the_app_label() {
        let upper = rows(Layer::Upper, "Join");
        assert_eq!(upper[1][0].label, "Q");
        assert!(upper[3][0].on, "Shift is on in Upper");
        assert!(upper[4].iter().any(|k| k.key == Key::Go && k.label == "Join"));
    }

    #[test]
    fn no_row_is_wider_than_eleven_units_and_the_bottom_row_is_nine() {
        for layer in [Layer::Lower, Layer::Upper, Layer::Symbols, Layer::More] {
            for (i, row) in rows(layer, "Go").iter().enumerate() {
                let total: f32 = row.iter().map(|k| k.width).sum();
                assert!(total <= 11.01, "{layer:?} row {i} is {total} units");
                if i == 4 {
                    assert!((total - 9.0).abs() < 0.01, "{layer:?} bottom row is {total} units");
                }
            }
        }
    }

    #[test]
    fn typing_inserts_at_the_caret_and_delete_removes_before_it() {
        let mut e = Editor::new(63);
        e.press(Key::Char('a'));
        e.press(Key::Char('c'));
        e.press(Key::Left);
        e.press(Key::Char('b'));
        assert_eq!(e.string(), "abc");
        assert_eq!(e.press(Key::Delete), Press::Edited);
        assert_eq!(e.string(), "ac");
        assert_eq!(e.caret, 1);
    }

    #[test]
    fn upper_is_one_capital_then_lower_again() {
        let mut e = Editor::new(63);
        e.press(Key::Shift);
        assert_eq!(e.layer, Layer::Upper);
        e.press(Key::Char('A'));
        assert_eq!(e.layer, Layer::Lower, "one capital, then back");
        assert_eq!(e.string(), "A");
    }

    #[test]
    fn layers_switch_and_go_is_reported_not_applied() {
        let mut e = Editor::new(63);
        assert_eq!(e.press(Key::Layer(Layer::Symbols)), Press::Layer);
        assert_eq!(e.layer, Layer::Symbols);
        assert_eq!(e.press(Key::Go), Press::Go);
        assert!(e.text.is_empty());
    }

    #[test]
    fn the_length_is_capped_and_edges_do_nothing() {
        let mut e = Editor::new(2);
        e.press(Key::Char('x'));
        e.press(Key::Char('y'));
        assert_eq!(e.press(Key::Char('z')), Press::Nothing);
        assert_eq!(e.string(), "xy");
        let mut f = Editor::new(5);
        assert_eq!(f.press(Key::Delete), Press::Nothing);
        assert_eq!(f.press(Key::Left), Press::Nothing);
        assert_eq!(f.press(Key::Right), Press::Nothing);
    }
}
