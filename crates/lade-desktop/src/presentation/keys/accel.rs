//! One shortcut: parsed from GTK accelerator syntax, compared, and shown in GTK and human form.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accel {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub other_modifier: bool,
    pub key: String,
}

impl Accel {
    pub fn parse(text: &str) -> Result<Self, String> {
        let invalid = || {
            format!(
                "‘{text}’ is not a valid shortcut. Use GTK syntax, for example F8 or <Control>F8."
            )
        };
        let mut rest = text.trim();
        let mut accel = Self {
            shift: false,
            control: false,
            alt: false,
            other_modifier: false,
            key: String::new(),
        };
        while let Some(inner) = rest.strip_prefix('<') {
            let (name, after) = inner.split_once('>').ok_or_else(invalid)?;
            match name.to_ascii_lowercase().as_str() {
                "shift" => accel.shift = true,
                "control" | "ctrl" | "ctl" | "primary" => accel.control = true,
                "alt" | "mod1" => accel.alt = true,
                "super" | "hyper" | "meta" | "release" => accel.other_modifier = true,
                _ => return Err(invalid()),
            }
            rest = after;
        }
        if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(invalid());
        }
        accel.key = canonical_key(rest);
        Ok(accel)
    }

    pub fn function_key(&self) -> Option<u8> {
        let digits = self.key.strip_prefix('F')?;
        if digits.starts_with('0') {
            return None;
        }
        digits.parse::<u8>().ok().filter(|n| (1..=35).contains(n))
    }

    pub fn to_gtk(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.shift, "<Shift>"),
            (self.control, "<Control>"),
            (self.alt, "<Alt>"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&self.key);
        out
    }

    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (on, name) in [
            (self.control, "Ctrl"),
            (self.shift, "Shift"),
            (self.alt, "Alt"),
        ] {
            if on {
                parts.push(name.to_owned());
            }
        }
        parts.push(match self.key.as_str() {
            "Return" => "Enter".to_owned(),
            "question" => "?".to_owned(),
            "Escape" => "Esc".to_owned(),
            key if key.chars().count() == 1 => key.to_ascii_uppercase(),
            key => key.to_owned(),
        });
        parts.join("+")
    }

    pub(super) fn same_combination(&self, other: &Self) -> bool {
        self.shift == other.shift
            && self.control == other.control
            && self.alt == other.alt
            && self.other_modifier == other.other_modifier
            && self.key.eq_ignore_ascii_case(&other.key)
    }
}

fn canonical_key(key: &str) -> String {
    if key.chars().count() == 1 {
        return key.to_ascii_lowercase();
    }
    match key.strip_prefix(['f', 'F']) {
        Some(digits) if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) => {
            format!("F{digits}")
        }
        _ => key.to_owned(),
    }
}
