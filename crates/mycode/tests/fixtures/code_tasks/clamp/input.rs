pub fn clamp_percent(value: u32) -> u32 {
    value.max(100)
}

pub fn percent_label() -> &'static str {
    "percent"
}
