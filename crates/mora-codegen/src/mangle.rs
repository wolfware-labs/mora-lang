//! Symbol names.

/// The symbol of the function whose qualified name has the given segments:
/// `_M`, then each segment as its byte length in decimal followed by the
/// segment, then `E`.
pub fn mangle<'a>(segments: impl IntoIterator<Item = &'a str>) -> String {
    let mut symbol = String::from("_M");
    for segment in segments {
        symbol.push_str(&segment.len().to_string());
        symbol.push_str(segment);
    }
    symbol.push('E');
    symbol
}

#[cfg(test)]
mod tests {
    use super::mangle;

    #[test]
    fn mangle_prefixes_each_segment_with_its_byte_length() {
        assert_eq!(
            mangle(["UsersApi", "UserStore", "Add"]),
            "_M8UsersApi9UserStore3AddE"
        );
        assert_eq!(mangle(["Ünï"]), "_M5ÜnïE");
    }
}
