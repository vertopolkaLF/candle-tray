//! TEMPORARY stub; replaced by the ported logger on merge.
pub fn info(message: impl AsRef<str>) {
    eprintln!("{}", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    eprintln!("{}", message.as_ref());
}
